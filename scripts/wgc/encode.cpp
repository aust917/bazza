#define NOMINMAX
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <winrt/base.h>
#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <iostream>
#include <memory>
#include <stdexcept>
#include <streambuf>
#include <string_view>
#include <thread>
#include <vector>
#include "matroska.hpp"

int raw_wgc_main(int argc, wchar_t** argv);
namespace {
using Handle = winrt::handle;
using namespace std::chrono_literals;

struct Pipe {
    Handle read, write;
    Pipe() {
        HANDLE r{}, w{};
        winrt::check_bool(CreatePipe(&r, &w, nullptr, 0));
        read.attach(r);
        write.attach(w);
    }
};

Handle inheritable(HANDLE original) {
    HANDLE copy{};
    winrt::check_bool(DuplicateHandle(GetCurrentProcess(), original, GetCurrentProcess(),
                                     &copy, 0, TRUE, DUPLICATE_SAME_ACCESS));
    return Handle(copy);
}

std::wstring quote(std::wstring const& arg) {
    std::wstring out = L"\"";
    size_t backslashes = 0;
    for (auto c : arg) {
        if (c == L'\\') {
            ++backslashes;
            continue;
        }
        out.append(c == L'"' ? backslashes * 2 + 1 : backslashes, L'\\');
        out.push_back(c);
        backslashes = 0;
    }
    out.append(backslashes * 2, L'\\');
    out.push_back(L'"');
    return out;
}

struct Child {
    Handle process;
    ~Child() {
        if (process && WaitForSingleObject(process.get(), 0) == WAIT_TIMEOUT) {
            TerminateProcess(process.get(), 1);
            WaitForSingleObject(process.get(), 1000);
        }
    }
    DWORD wait(DWORD timeout) const {
        if (WaitForSingleObject(process.get(), timeout) != WAIT_OBJECT_0)
            throw std::runtime_error("video worker did not exit before timeout");
        DWORD status{};
        winrt::check_bool(GetExitCodeProcess(process.get(), &status));
        return status;
    }
};

Handle spawn(std::vector<std::wstring> const& args, HANDLE input, HANDLE output, HANDLE error) {
    auto i = inheritable(input), o = inheritable(output), e = inheritable(error);
    std::array<HANDLE, 3> inherited{i.get(), o.get(), e.get()};
    SIZE_T size{};
    InitializeProcThreadAttributeList(nullptr, 1, 0, &size);
    // Heap storage is aligned for the Win32 attribute list.
    auto memory = std::make_unique<std::byte[]>(size);
    auto* attributes = reinterpret_cast<LPPROC_THREAD_ATTRIBUTE_LIST>(memory.get());
    winrt::check_bool(InitializeProcThreadAttributeList(attributes, 1, 0, &size));
    struct Attributes {
        LPPROC_THREAD_ATTRIBUTE_LIST value;
        ~Attributes() { DeleteProcThreadAttributeList(value); }
    } attributes_guard{attributes};
    winrt::check_bool(UpdateProcThreadAttribute(attributes, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                                                inherited.data(), sizeof(inherited), nullptr, nullptr));
    STARTUPINFOEXW info{};
    info.StartupInfo.cb = sizeof(info);
    info.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    info.StartupInfo.hStdInput = i.get();
    info.StartupInfo.hStdOutput = o.get();
    info.StartupInfo.hStdError = e.get();
    info.lpAttributeList = attributes;
    std::wstring command;
    for (auto const& arg : args) {
        if (!command.empty()) command += L' ';
        command += quote(arg);
    }
    PROCESS_INFORMATION child{};
    winrt::check_bool(CreateProcessW(args[0].c_str(), command.data(), nullptr, nullptr, TRUE,
                                    CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT, nullptr,
                                    nullptr, &info.StartupInfo, &child));
    CloseHandle(child.hThread);
    return Handle(child.hProcess);
}

bool read_exact(HANDLE pipe, void* data, size_t length) {
    auto* bytes = static_cast<uint8_t*>(data);
    for (size_t offset = 0; offset < length;) {
        DWORD count{};
        if (!ReadFile(pipe, bytes + offset, static_cast<DWORD>(length - offset), &count, nullptr)) {
            if (GetLastError() != ERROR_BROKEN_PIPE) winrt::throw_last_error();
            count = 0;
        }
        if (!count) {
            if (!offset) return false;
            throw std::runtime_error("partial WGC frame received");
        }
        offset += count;
    }
    return true;
}

struct PipeBuffer final : std::streambuf {
    PipeBuffer(HANDLE output, std::atomic<uint64_t>& write_started)
        : pipe(output), started(write_started) {}
    HANDLE pipe;
    std::atomic<uint64_t>& started;
    std::streamsize xsputn(char const* data, std::streamsize length) override {
        started.store(GetTickCount64());
        struct WriteEnd {
            std::atomic<uint64_t>& started;
            ~WriteEnd() { started.store(0); }
        } end{started};
        std::streamsize offset = 0;
        while (offset < length) {
            DWORD count{};
            const DWORD size = static_cast<DWORD>(length - offset);
            if (!WriteFile(pipe, data + offset, size, &count, nullptr) || !count) break;
            offset += count;
        }
        return offset;
    }
    int_type overflow(int_type c) override {
        if (traits_type::eq_int_type(c, traits_type::eof())) return traits_type::not_eof(c);
        const char value = traits_type::to_char_type(c);
        return xsputn(&value, 1) == 1 ? c : traits_type::eof();
    }
};

int64_t qpc_100ns(int64_t frequency) {
    LARGE_INTEGER value{};
    winrt::check_bool(QueryPerformanceCounter(&value));
    return value.QuadPart / frequency * 10'000'000 +
           value.QuadPart % frequency * 10'000'000 / frequency;
}

struct Clock {
    int64_t frequency{};
    int64_t qpc{}, utc_100ns{};
    static Clock sample() {
        LARGE_INTEGER frequency{};
        winrt::check_bool(QueryPerformanceFrequency(&frequency));
        Clock result{frequency.QuadPart};
        int64_t tightest = 10'000;  // 1 ms
        for (int i = 0; i < 8; ++i) {
            const auto before = qpc_100ns(result.frequency);
            FILETIME time{};
            GetSystemTimePreciseAsFileTime(&time);
            const auto after = qpc_100ns(result.frequency);
            if (after >= before && after - before < tightest) {
                tightest = after - before;
                result.qpc = before + tightest / 2;
                result.utc_100ns = static_cast<int64_t>(
                    (static_cast<uint64_t>(time.dwHighDateTime) << 32) | time.dwLowDateTime
                ) - 116444736000000000LL;
            }
        }
        if (!result.qpc || result.utc_100ns <= 0)
            throw std::runtime_error("could not pair the WGC and recording clocks");
        return result;
    }
    int64_t unix_us(int64_t first) const {
        if (first < qpc - 10'000 || first > qpc_100ns(frequency) + 10'000)
            throw std::runtime_error("first WGC frame has an invalid QPC timestamp");
        return (utc_100ns + first - qpc) / 10;
    }
};

void clock_marker(wchar_t const* path, int64_t unix_us) {
    Handle file(CreateFileW(path, GENERIC_WRITE, 0, nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr));
    if (!file) winrt::throw_last_error();
    const std::string marker = "frame:0    pts:" + std::to_string(unix_us) + "\nbazza.clock=1\n";
    DWORD written{};
    winrt::check_bool(WriteFile(file.get(), marker.data(), static_cast<DWORD>(marker.size()), &written, nullptr));
    if (written != marker.size()) throw std::runtime_error("incomplete WGC clock marker");
}

void relay_stop(std::stop_token stop, HANDLE input, HANDLE target) {
    while (!stop.stop_requested()) {
        DWORD available{};
        if (!PeekNamedPipe(input, nullptr, 0, nullptr, &available, nullptr)) break;
        if (available) {
            char data[64]{};
            DWORD read{};
            if (!ReadFile(input, data, std::min<DWORD>(available, sizeof(data)), &read, nullptr) ||
                !read || std::string_view(data, read).find('q') != std::string_view::npos)
                break;
        }
        std::this_thread::sleep_for(30ms);
    }
    if (!stop.stop_requested()) {
        DWORD written{};
        WriteFile(target, "q", 1, &written, nullptr);
    }
}

unsigned number(wchar_t const* text, unsigned low, unsigned high) {
    std::wstring value(text);
    if (value.empty() || value.find_first_not_of(L"0123456789") != std::wstring::npos)
        throw std::invalid_argument("expected a positive decimal argument");
    const auto n = std::stoul(value);
    if (n < low || n > high) throw std::invalid_argument("argument outside supported range");
    return n;
}

int encode(int argc, wchar_t** argv) {
    if (argc != 9) throw std::invalid_argument("usage: bazza-wgc --encode HWND PID FPS MAX_HEIGHT FFMPEG MP4 CLOCK");
    const unsigned fps = number(argv[4], 1, 120), height_limit = number(argv[5], 2, 4320);
    if (!std::filesystem::path(argv[6]).is_absolute())
        throw std::invalid_argument("the packaged FFmpeg path must be absolute");
    const auto clock = Clock::sample();

    wchar_t self[32768]{};
    const auto self_size = GetModuleFileNameW(nullptr, self, static_cast<DWORD>(std::size(self)));
    if (!self_size || self_size >= std::size(self)) winrt::throw_last_error();
    const auto error = GetStdHandle(STD_ERROR_HANDLE);
    Handle nul(CreateFileW(L"NUL", GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE,
                           nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr));
    if (!nul) winrt::throw_last_error();
    Pipe control, frames;
    Child raw{spawn({self, L"--hwnd", argv[2], L"--pid", argv[3], L"--fps", argv[4]},
                    control.read.get(), frames.write.get(), error)};
    control.read.close();
    frames.write.close();
    std::jthread stop(relay_stop, GetStdHandle(STD_INPUT_HANDLE), control.write.get());

    struct Header { std::array<char, 4> magic; uint32_t width, height, reserved; };
    static_assert(sizeof(Header) == 16);
    Header h{};
    if (!read_exact(frames.read.get(), &h, sizeof(h))) {
        const auto status = raw.wait(1500);
        return status ? static_cast<int>(status) : 9;
    }
    if (h.magic != std::array{'W', 'G', 'C', '1'} || h.reserved ||
        h.width < 2 || h.width > 16'384 || h.height < 2 || h.height > 16'384 ||
        static_cast<uint64_t>(h.width) * h.height * 4 > 128ull * 1024 * 1024 ||
        h.width % 2 || h.height % 2)
        throw std::runtime_error("invalid WGC stream header");

    Pipe pixels;
    Child encoder{spawn({
        argv[6], L"-hide_banner", L"-nostdin", L"-loglevel", L"error", L"-xerror", L"-n",
        L"-probesize", L"32768", L"-analyzeduration", L"1",
        L"-f", L"matroska", L"-i", L"pipe:0", L"-map", L"0:v:0", L"-an",
        L"-vf", L"scale=-2:'max(2,trunc(min(" + std::to_wstring(height_limit) + L",ih)/2)*2)'",
        L"-fps_mode", L"passthrough", L"-enc_time_base", L"1/1000000",
        L"-c:v", L"libx264", L"-preset", L"veryfast", L"-tune", L"zerolatency", L"-crf", L"28",
        L"-pix_fmt", L"yuv420p", L"-bf", L"0", L"-video_track_timescale", L"1000000",
        L"-frag_duration", L"1000000", L"-flush_packets", L"1", L"-movflags", L"+frag_keyframe+empty_moov+default_base_moof",
        L"-f", L"mp4", argv[7],
    }, pixels.read.get(), nul.get(), error)};
    pixels.read.close();
    std::atomic<uint64_t> encoder_write_started{0};
    std::jthread encoder_watchdog([&](std::stop_token stop) {
        while (!stop.stop_requested()) {
            const auto started = encoder_write_started.load();
            if (started && GetTickCount64() - started >= 5000) {
                std::cerr << "wgc_encoder_error=encoder input stalled\n";
                TerminateProcess(encoder.process.get(), 9);
                return;
            }
            std::this_thread::sleep_for(30ms);
        }
    });
    PipeBuffer buffer(pixels.write.get(), encoder_write_started);
    std::ostream stream(&buffer);
    stream.exceptions(std::ios::badbit | std::ios::failbit);
    Matroska matroska(stream, h.width, h.height);
    std::vector<uint8_t> frame(static_cast<size_t>(h.width) * h.height * 4);
    int64_t first{}, last{}, slot{-1};
    int64_t qpc{};
    while (read_exact(frames.read.get(), &qpc, sizeof(qpc))) {
        if (qpc <= last || !read_exact(frames.read.get(), frame.data(), frame.size()))
            throw std::runtime_error("invalid WGC timestamp or truncated frame");
        last = qpc;
        const auto first_frame = first == 0;
        if (first_frame) first = qpc;
        const auto elapsed = qpc - first;
        // Admit one real frame per configured cadence slot, retaining its source timestamp.
        const auto next_slot = (elapsed / 10'000'000 * fps) +
            (elapsed % 10'000'000 * fps + 5000 * fps) / 10'000'000;
        if (next_slot <= slot) continue;
        matroska.frame(static_cast<uint64_t>(elapsed / 10), frame.data(), frame.size());
        if (first_frame) clock_marker(argv[8], clock.unix_us(first));
        slot = next_slot;
    }
    pixels.write.close();  // EOF lets FFmpeg flush the last fragmented MP4 packet.
    const auto source_status = raw.wait(2000);
    const auto encoder_status = encoder.wait(5500);
    if (source_status) return static_cast<int>(source_status);
    if (encoder_status || !first) throw std::runtime_error("FFmpeg could not finalize the WGC video");
    return 0;
}
}  // namespace

int wmain(int argc, wchar_t** argv) {
    if (argc < 2 || std::wstring_view(argv[1]) != L"--encode")
        return raw_wgc_main(argc, argv);
    try {
        return encode(argc, argv);
    } catch (winrt::hresult_error const& error) {
        std::wcerr << L"wgc_encoder_error=" << error.message().c_str() << L'\n';
    } catch (std::exception const& error) {
        std::cerr << "wgc_encoder_error=" << error.what() << '\n';
    }
    return 9;
}
