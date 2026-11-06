#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <optional>
#include <ostream>
#include <stdexcept>
#include <string_view>

// Pipe-sized Matroska headers for one uncompressed top-down BGRA video track.
// The stream has no duration or index and does not need to seek on close.
class Matroska {
public:
    Matroska(std::ostream& output, uint32_t width, uint32_t height)
        : output_(output), frame_bytes_(frame_size(width, height)) {
        Bytes ebml;
        ebml.integer(0x4286, 1);  // EBMLVersion
        ebml.integer(0x42F7, 1);  // EBMLReadVersion
        ebml.integer(0x42F2, 4);  // EBMLMaxIDLength
        ebml.integer(0x42F3, 8);  // EBMLMaxSizeLength
        ebml.element(0x4282, "matroska");
        ebml.integer(0x4287, 2);  // DocTypeVersion (SimpleBlock)
        ebml.integer(0x4285, 2);  // DocTypeReadVersion

        Bytes info;
        info.integer(0x2AD7B1, 1000);  // TimestampScale in nanoseconds
        info.element(0x4D80, "Bazza");
        info.element(0x5741, "Bazza");

        Bytes bitmap;
        bitmap.little(40, 4);  // BITMAPINFOHEADER size
        bitmap.little(width, 4);
        bitmap.little(static_cast<uint32_t>(-static_cast<int32_t>(height)), 4);
        bitmap.little(1, 2);   // biPlanes
        bitmap.little(32, 2);  // biBitCount
        bitmap.little(0, 4);   // BI_RGB
        bitmap.little(frame_bytes_, 4);
        bitmap.little(0, 4);  // biXPelsPerMeter
        bitmap.little(0, 4);  // biYPelsPerMeter
        bitmap.little(0, 4);  // biClrUsed
        bitmap.little(0, 4);  // biClrImportant

        Bytes video;
        video.integer(0xB0, width);
        video.integer(0xBA, height);
        Bytes track;
        track.integer(0xD7, 1);    // TrackNumber
        track.integer(0x73C5, 1);  // TrackUID
        track.integer(0x83, 1);    // TrackType video
        track.integer(0x9C, 0);    // FlagLacing
        track.element(0x86, "V_MS/VFW/FOURCC");
        track.element(0x63A2, bitmap.view());
        track.element(0xE0, video.view());
        Bytes tracks;
        tracks.element(0xAE, track.view());

        Bytes prefix;
        prefix.element(0x1A45DFA3, ebml.view());
        prefix.big(0x18538067, 4);
        prefix.big(UINT64_C(0x01FFFFFFFFFFFFFF), 8);  // Unknown Segment size
        prefix.element(0x1549A966, info.view());
        prefix.element(0x1654AE6B, tracks.view());
        write(prefix.view());
        flush();
    }

    void frame(uint64_t pts_us, const uint8_t* bgra, size_t bytes) {
        if (!bgra || bytes != frame_bytes_) throw std::invalid_argument("Invalid BGRA frame size");
        if (pts_us > static_cast<uint64_t>((std::numeric_limits<int64_t>::max)()) / 1000
            || (last_pts_ && pts_us <= *last_pts_)) {
            throw std::invalid_argument("Invalid or non-increasing Matroska timestamp");
        }
        Bytes timestamp;
        timestamp.integer(0xE7, pts_us);
        Bytes block;
        block.header(0xA3, 4 + uint64_t(bytes));
        block.big(0x81000080, 4);  // Track 1, signed relative timestamp 0, keyframe.
        Bytes prefix;
        prefix.header(0x1F43B675, timestamp.size() + block.size() + uint64_t(bytes));
        prefix.append(timestamp.view());
        prefix.append(block.view());
        write(prefix.view());
        write({reinterpret_cast<const char*>(bgra), bytes});
        flush();
        last_pts_ = pts_us;
    }

private:
    class Bytes {
    public:
        size_t size() const { return size_; }
        std::string_view view() const { return {data_.data(), size_}; }

        void append(std::string_view value) {
            if (value.size() > data_.size() - size_) throw std::length_error("Matroska header too large");
            for (char ch : value) data_[size_++] = ch;
        }

        void big(uint64_t value, unsigned width) {
            for (unsigned i = width; i != 0; --i) byte(value >> (8 * (i - 1)));
        }

        void little(uint32_t value, unsigned width) {
            for (unsigned i = 0; i < width; ++i) byte(value >> (8 * i));
        }

        void header(uint32_t id, uint64_t length) {
            big(id, width(id));
            for (unsigned i = 1; i <= 8; ++i) {
                const uint64_t marker = UINT64_C(1) << (7 * i);
                if (length < marker - 1) {
                    big(marker | length, i);
                    return;
                }
            }
            throw std::length_error("Matroska element too large");
        }

        void element(uint32_t id, std::string_view value) {
            header(id, value.size());
            append(value);
        }

        void integer(uint32_t id, uint64_t value) {
            const auto bytes = width(value);
            header(id, bytes);
            big(value, bytes);
        }

    private:
        static unsigned width(uint64_t value) {
            unsigned bytes = 1;
            while (value > 0xff) {
                value >>= 8;
                ++bytes;
            }
            return bytes;
        }

        void byte(uint64_t value) {
            if (size_ == data_.size()) throw std::length_error("Matroska header too large");
            data_[size_++] = static_cast<char>(value & 0xff);
        }

        std::array<char, 256> data_{};
        size_t size_ = 0;
    };

    static uint32_t frame_size(uint32_t width, uint32_t height) {
        const auto max_dimension = static_cast<uint32_t>((std::numeric_limits<int32_t>::max)());
        if (!width || !height || width > max_dimension || height > max_dimension) {
            throw std::invalid_argument("Invalid Matroska video dimensions");
        }
        const auto size = uint64_t(width) * height * 4;
        if (size > (std::numeric_limits<uint32_t>::max)()
            || size > static_cast<uint64_t>((std::numeric_limits<std::streamsize>::max)())) {
            throw std::invalid_argument("Matroska video frame is too large");
        }
        return static_cast<uint32_t>(size);
    }

    void write(std::string_view value) {
        output_.write(value.data(), static_cast<std::streamsize>(value.size()));
        if (!output_) throw std::runtime_error("Writing Matroska to the encoder failed");
    }

    void flush() {
        output_.flush();
        if (!output_) throw std::runtime_error("Flushing Matroska to the encoder failed");
    }

    std::ostream& output_;
    uint32_t frame_bytes_;
    std::optional<uint64_t> last_pts_;
};
