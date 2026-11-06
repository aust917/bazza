#define NOMINMAX
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <Windows.Graphics.Capture.Interop.h>
#include <windows.graphics.directx.direct3d11.interop.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Graphics.Capture.h>
#include <winrt/Windows.Graphics.DirectX.Direct3D11.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <bit>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <thread>
#include <vector>

namespace {
namespace wgc = winrt::Windows::Graphics::Capture;
namespace dx = winrt::Windows::Graphics::DirectX;
namespace d3d = winrt::Windows::Graphics::DirectX::Direct3D11;
using Size = winrt::Windows::Graphics::SizeInt32;
using SurfaceAccess = ::Windows::Graphics::DirectX::Direct3D11::IDirect3DDxgiInterfaceAccess;
using Clock = std::chrono::steady_clock;
using namespace std::chrono_literals;

static_assert(std::endian::native == std::endian::little);
constexpr uint64_t max_frame_bytes = 128ull * 1024 * 1024;
constexpr int pool_buffers = 3;
constexpr auto pixel_format = dx::DirectXPixelFormat::B8G8R8A8UIntNormalized;
constexpr GUID console_display{0x6fe69556, 0x704a, 0x47a0, {0x8f,0x24,0xc2,0x8d,0x93,0x6f,0xda,0x47}};
constexpr GUID session_display{0x2b84c20e, 0xad23, 0x4ddf, {0x93,0xdb,0x05,0xff,0xbd,0x7e,0xfc,0xa5}};

enum class Exit : int {
    none = -1, stopped = 0, invalid_args = 2, target_closed = 3, stalled = 4,
    setup_failed = 5, capture_failed = 6, stdout_closed = 7, display_off = 8,
};

const char* exit_name(Exit result) {
    switch (result) {
    case Exit::stopped: return "stopped";
    case Exit::invalid_args: return "invalid_args";
    case Exit::target_closed: return "target_closed";
    case Exit::stalled: return "stalled";
    case Exit::setup_failed: return "setup_failed";
    case Exit::stdout_closed: return "stdout_closed";
    case Exit::display_off: return "display_off";
    default: return "capture_failed";
    }
}

struct Window {
    HWND handle{};
    DWORD pid{};
    DWORD tid{};

    bool current() const {
        DWORD owner{};
        return IsWindow(handle) && GetWindowThreadProcessId(handle, &owner) == tid && owner == pid;
    }
};

struct Options {
    Window target;
    DWORD stall_ms = 5000;
    uint64_t fps = 60;
};

uint64_t number(std::wstring_view input, uint64_t maximum, bool allow_hex = false) {
    int base = 10;
    if (allow_hex && (input.starts_with(L"0x") || input.starts_with(L"0X"))) {
        input.remove_prefix(2);
        base = 16;
    }
    if (input.empty()) {
        throw std::invalid_argument("missing number");
    }
    uint64_t value = 0;
    for (const auto ch : input) {
        int digit = -1;
        if (ch >= L'0' && ch <= L'9') digit = ch - L'0';
        if (base == 16 && ch >= L'a' && ch <= L'f') digit = ch - L'a' + 10;
        if (base == 16 && ch >= L'A' && ch <= L'F') digit = ch - L'A' + 10;
        if (digit < 0 || value > (maximum - static_cast<uint64_t>(digit)) / static_cast<uint64_t>(base)) {
            throw std::invalid_argument("invalid number");
        }
        value = value * static_cast<uint64_t>(base) + static_cast<uint64_t>(digit);
    }
    if (!value || value > maximum) throw std::invalid_argument("number outside bounds");
    return value;
}

Options parse(int argc, wchar_t** argv) {
    Options options;
    bool stall_seen = false, fps_seen = false;
    for (int index = 1; index < argc; ++index) {
        const std::wstring_view name = argv[index];
        if (++index >= argc) throw std::invalid_argument("missing option value");
        const std::wstring_view value = argv[index];
        if (name == L"--hwnd" && !options.target.handle) {
            options.target.handle = reinterpret_cast<HWND>(number(value, UINTPTR_MAX, true));
        } else if (name == L"--pid" && !options.target.pid) {
            options.target.pid = static_cast<DWORD>(number(value, MAXDWORD));
        } else if (name == L"--fps" && !fps_seen) {
            options.fps = number(value, 120);
            fps_seen = true;
        } else if (name == L"--stall-ms" && !stall_seen) {
            options.stall_ms = static_cast<DWORD>(number(value, 120000));
            stall_seen = true;
            if (options.stall_ms < 500) throw std::invalid_argument("stall timeout too short");
        } else {
            throw std::invalid_argument("unknown or duplicate option");
        }
    }
    if (!options.target.handle || !options.target.pid) throw std::invalid_argument("missing window identity");
    DWORD owner{};
    options.target.tid = GetWindowThreadProcessId(options.target.handle, &owner);
    if (!options.target.tid || owner != options.target.pid || !options.target.current()) {
        throw std::invalid_argument("window identity mismatch");
    }
    return options;
}

winrt::handle event(bool manual) {
    winrt::handle result{CreateEventW(nullptr, manual ? TRUE : FALSE, FALSE, nullptr)};
    if (!result) winrt::throw_last_error();
    return result;
}

struct DisplayState {
    DWORD console = MAXDWORD;
    DWORD session = MAXDWORD;
    HWND window{};
    HPOWERNOTIFY console_notice{};
    HPOWERNOTIFY session_notice{};

    static LRESULT CALLBACK listen(HWND hwnd, UINT message, WPARAM w, LPARAM l) {
        if (message == WM_NCCREATE) {
            const auto* create = reinterpret_cast<const CREATESTRUCTW*>(l);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(create->lpCreateParams));
        }
        auto* self = reinterpret_cast<DisplayState*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
        if (self && message == WM_POWERBROADCAST && w == PBT_POWERSETTINGCHANGE && l) {
            const auto* setting = reinterpret_cast<const POWERBROADCAST_SETTING*>(l);
            if (setting->DataLength == sizeof(DWORD)) {
                DWORD value{};
                std::memcpy(&value, setting->Data, sizeof value);
                if (setting->PowerSetting == console_display) self->console = value;
                if (setting->PowerSetting == session_display) self->session = value;
            }
            return TRUE;
        }
        return DefWindowProcW(hwnd, message, w, l);
    }

    DisplayState() {
        const auto module = GetModuleHandleW(nullptr);
        WNDCLASSW cls{};
        cls.lpfnWndProc = listen;
        cls.hInstance = module;
        cls.lpszClassName = L"BazzaWgcRawDisplayState";
        if (!RegisterClassW(&cls) && GetLastError() != ERROR_CLASS_ALREADY_EXISTS) return;
        window = CreateWindowExW(0, cls.lpszClassName, L"", 0, 0, 0, 0, 0, HWND_MESSAGE,
                                 nullptr, module, this);
        if (window) {
            console_notice = RegisterPowerSettingNotification(window, &console_display, DEVICE_NOTIFY_WINDOW_HANDLE);
            session_notice = RegisterPowerSettingNotification(window, &session_display, DEVICE_NOTIFY_WINDOW_HANDLE);
        }
    }

    ~DisplayState() {
        if (console_notice) UnregisterPowerSettingNotification(console_notice);
        if (session_notice) UnregisterPowerSettingNotification(session_notice);
        if (window) DestroyWindow(window);
    }

    bool off() {
        MSG message{};
        while (window && PeekMessageW(&message, window, 0, 0, PM_REMOVE)) DispatchMessageW(&message);
        return (session == MAXDWORD ? console : session) == 0;
    }
};

constexpr bool display_loss_confirmed(uint64_t now, uint64_t off_since, uint64_t capture_started,
                                      uint64_t fresh_frame, DWORD stall_ms) {
    if (!off_since || !capture_started) return false;
    const auto latest = std::max({off_since, capture_started, fresh_frame});
    return now >= latest && now - latest >= stall_ms;
}

static_assert(!display_loss_confirmed(10000, 1000, 0, 0, 5000));
static_assert(!display_loss_confirmed(1500, 1000, 1000, 1444, 5000));
static_assert(!display_loss_confirmed(10000, 1000, 1200, 9990, 5000));
static_assert(display_loss_confirmed(10000, 1000, 1200, 5000, 5000));
static_assert(display_loss_confirmed(10000, 1000, 5000, 0, 5000));

struct Control {
    Window target;
    DWORD stall_ms;
    winrt::handle finished = event(true);
    winrt::handle stop = event(true);
    winrt::handle writer_thread;
    std::atomic<Exit> requested{Exit::none};
    std::atomic<uint64_t> write_started{0};
    std::atomic<bool> write_timed_out{false};
    std::atomic<uint64_t> capture_started_at{0};
    std::atomic<uint64_t> last_fresh_frame_at{0};
    std::thread watcher;

    explicit Control(Options const& options) : target(options.target), stall_ms(options.stall_ms) {
        HANDLE thread{};
        if (!DuplicateHandle(GetCurrentProcess(), GetCurrentThread(), GetCurrentProcess(),
                             &thread, 0, FALSE, DUPLICATE_SAME_ACCESS)) {
            winrt::throw_last_error();
        }
        writer_thread.attach(thread);
        watcher = std::thread([this] { watch(); });
    }

    ~Control() {
        SetEvent(finished.get());
        watcher.join();
    }

    bool request(Exit reason) {
        auto empty = Exit::none;
        if (!requested.compare_exchange_strong(empty, reason)) return false;
        SetEvent(stop.get());
        return true;
    }

    Exit reason() const { return requested.load(); }

    void watch() {
        DisplayState display;
        uint64_t off_since = 0;
        const HANDLE input = GetStdHandle(STD_INPUT_HANDLE);
        const bool use_pipe = GetFileType(input) == FILE_TYPE_PIPE;
        while (WaitForSingleObject(finished.get(), 20) == WAIT_TIMEOUT) {
            if (!target.current()) request(Exit::target_closed);
            if (use_pipe && reason() == Exit::none) {
                DWORD available{};
                if (!PeekNamedPipe(input, nullptr, 0, nullptr, &available, nullptr)) {
                    request(Exit::stopped);
                } else if (available) {
                    char command{};
                    DWORD received{};
                    if (!ReadFile(input, &command, 1, &received, nullptr) || !received || command == 'q') {
                        request(Exit::stopped);
                    }
                }
            }
            const auto started = write_started.load();
            if (started && GetTickCount64() - started >= stall_ms) {
                write_timed_out.store(true);
                request(Exit::stalled);
                CancelSynchronousIo(writer_thread.get());
            }
            if (reason() != Exit::none) continue;
            const bool off = display.off();
            const auto capture_started = capture_started_at.load();
            const auto fresh_frame = last_fresh_frame_at.load();
            const auto now = GetTickCount64();
            off_since = off ? (off_since ? off_since : now) : 0;
            if (display_loss_confirmed(now, off_since, capture_started, fresh_frame, stall_ms)) {
                if (request(Exit::display_off)) {
                    std::fprintf(stderr,
                        "wgc_display_evidence source=%s status=off(0) console=%lu session=%lu "
                        "fresh_frame_seen=%u frame_age_ms=%llu\n",
                        display.session == MAXDWORD ? "console" : "session",
                        display.console, display.session, fresh_frame ? 1u : 0u,
                        static_cast<unsigned long long>(now - (fresh_frame ? fresh_frame : capture_started)));
                }
            } else if (capture_started && !fresh_frame && now - capture_started >= stall_ms) {
                request(Exit::stalled);
            }
        }
    }

    Exit write(HANDLE output, const void* data, size_t size) {
        const auto* next = static_cast<const uint8_t*>(data);
        write_started.store(GetTickCount64());
        struct EndWrite {
            std::atomic<uint64_t>& started;
            ~EndWrite() { started.store(0); }
        } end_write{write_started};
        while (size) {
            if (write_timed_out.load()) return Exit::stalled;
            const auto length = static_cast<DWORD>(std::min<size_t>(size, 256 * 1024));
            DWORD written{};
            const BOOL success = WriteFile(output, next, length, &written, nullptr);
            if (write_timed_out.load()) return Exit::stalled;
            if (!success || !written) return Exit::stdout_closed;
            next += written;
            size -= written;
        }
        return Exit::none;
    }
};

struct Apartment {
    Apartment() { winrt::init_apartment(winrt::apartment_type::multi_threaded); }
    ~Apartment() { winrt::uninit_apartment(); }
};

struct CaptureCloser {
    wgc::GraphicsCaptureSession session;
    wgc::Direct3D11CaptureFramePool pool;
    ~CaptureCloser() {
        try { session.Close(); } catch (...) {}
        try { pool.Close(); } catch (...) {}
    }
};

bool valid_size(Size size) {
    return size.Width >= 2 && size.Height >= 2 &&
           size.Width <= D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION &&
           size.Height <= D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION &&
           static_cast<uint64_t>(size.Width) * static_cast<uint64_t>(size.Height) * 4 <= max_frame_bytes;
}

class ReadbackWait {
public:
    void wait(HANDLE stop) {
        if (timer) {
            LARGE_INTEGER due{};
            due.QuadPart = -20'000; // Relative 2 ms in 100 ns units.
            if (SetWaitableTimer(timer.get(), &due, 0, nullptr, nullptr, FALSE)) {
                HANDLE signals[]{stop, timer.get()};
                const auto result = WaitForMultipleObjects(2, signals, FALSE, 20);
                if (result == WAIT_OBJECT_0 || result == WAIT_OBJECT_0 + 1) return;
                if (result == WAIT_TIMEOUT) {
                    timer.close();
                    return;
                }
            }
            timer.close();
        }
        WaitForSingleObject(stop, 2);
    }

private:
    // A coarse 2 ms timeout can stall the capture loop for a full timer tick.
    winrt::handle timer{CreateWaitableTimerExW(nullptr, nullptr, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                                              TIMER_MODIFY_STATE | SYNCHRONIZE)};
};

struct Canvas {
    uint32_t width;
    uint32_t height;
    winrt::com_ptr<ID3D11Texture2D> staging;
    std::vector<uint8_t> record;
    ReadbackWait readback_wait;

    Canvas(ID3D11Device* device, Size size)
        : width(static_cast<uint32_t>(size.Width) & ~1u),
          height(static_cast<uint32_t>(size.Height) & ~1u),
          record(8 + static_cast<size_t>(width) * height * 4) {
        D3D11_TEXTURE2D_DESC desc{};
        desc.Width = width;
        desc.Height = height;
        desc.MipLevels = 1;
        desc.ArraySize = 1;
        desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
        desc.SampleDesc.Count = 1;
        desc.Usage = D3D11_USAGE_STAGING;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        winrt::check_hresult(device->CreateTexture2D(&desc, nullptr, staging.put()));
    }

    Exit header(Control& control, HANDLE output) const {
        std::array<uint8_t, 16> bytes{'W', 'G', 'C', '1'};
        std::memcpy(bytes.data() + 4, &width, sizeof width);
        std::memcpy(bytes.data() + 8, &height, sizeof height);
        return control.write(output, bytes.data(), bytes.size());
    }

    Exit copy_frame(
        ID3D11DeviceContext* context, wgc::Direct3D11CaptureFrame const& frame,
        Size size, int64_t timestamp, Control& control, Clock::time_point deadline
    ) {
        winrt::com_ptr<ID3D11Texture2D> source;
        winrt::check_hresult(frame.Surface().as<SurfaceAccess>()->GetInterface(
            __uuidof(ID3D11Texture2D), source.put_void()));
        D3D11_TEXTURE2D_DESC source_desc{};
        source->GetDesc(&source_desc);
        if (source_desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM) return Exit::capture_failed;
        const auto copy_width = std::min({width, source_desc.Width, static_cast<uint32_t>(size.Width)});
        const auto copy_height = std::min({height, source_desc.Height, static_cast<uint32_t>(size.Height)});
        if (!copy_width || !copy_height) return Exit::capture_failed;
        const D3D11_BOX region{0, 0, 0, copy_width, copy_height, 1};
        context->CopySubresourceRegion(staging.get(), 0, 0, 0, 0, source.get(), 0, &region);
        context->Flush();

        D3D11_MAPPED_SUBRESOURCE mapped{};
        for (;;) {
            if (control.reason() != Exit::none) return control.reason();
            const auto mapped_result = context->Map(staging.get(), 0, D3D11_MAP_READ, D3D11_MAP_FLAG_DO_NOT_WAIT, &mapped);
            if (mapped_result != DXGI_ERROR_WAS_STILL_DRAWING) {
                winrt::check_hresult(mapped_result);
                break;
            }
            if (Clock::now() >= deadline) return Exit::stalled;
            readback_wait.wait(control.stop.get());
        }
        struct Unmap {
            ID3D11DeviceContext* context;
            ID3D11Texture2D* staging;
            ~Unmap() { context->Unmap(staging, 0); }
        } unmap{context, staging.get()};
        if (!mapped.pData || mapped.RowPitch < copy_width * 4) return Exit::capture_failed;
        std::memcpy(record.data(), &timestamp, sizeof timestamp);
        const auto* mapped_pixels = static_cast<const uint8_t*>(mapped.pData);
        auto* packed = record.data() + 8;
        const size_t row_bytes = static_cast<size_t>(width) * 4;
        const size_t copied_row_bytes = static_cast<size_t>(copy_width) * 4;
        for (uint32_t row = 0; row < height; ++row) {
            auto* destination = packed + row * row_bytes;
            size_t copied = 0;
            if (row < copy_height) {
                std::memcpy(destination, mapped_pixels + static_cast<size_t>(row) * mapped.RowPitch, copied_row_bytes);
                copied = copied_row_bytes;
            }
            if (copied < row_bytes) {
                std::memset(destination + copied, 0, row_bytes - copied);
                for (size_t alpha = copied + 3; alpha < row_bytes; alpha += 4) destination[alpha] = 255;
            }
        }
        return Exit::none;
    }
};

Exit capture(Options const& options, HANDLE output, Exit& exception_reason) {
    Apartment apartment;
    Control control(options);
    if (!wgc::GraphicsCaptureSession::IsSupported()) return Exit::setup_failed;
    if (!options.target.current()) return Exit::target_closed;

    winrt::com_ptr<ID3D11Device> device;
    winrt::com_ptr<ID3D11DeviceContext> context;
    winrt::check_hresult(D3D11CreateDevice(
        nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        nullptr, 0, D3D11_SDK_VERSION, device.put(), nullptr, context.put()));
    winrt::com_ptr<::IInspectable> inspectable;
    winrt::check_hresult(CreateDirect3D11DeviceFromDXGIDevice(
        device.as<IDXGIDevice>().get(), inspectable.put()));
    auto rt_device = inspectable.as<d3d::IDirect3DDevice>();
    auto interop = winrt::get_activation_factory<wgc::GraphicsCaptureItem, IGraphicsCaptureItemInterop>();
    wgc::GraphicsCaptureItem item{nullptr};
    winrt::check_hresult(interop->CreateForWindow(
        options.target.handle, winrt::guid_of<wgc::GraphicsCaptureItem>(), winrt::put_abi(item)));
    if (!options.target.current()) return Exit::target_closed;
    auto size = item.Size();
    if (!valid_size(size)) return Exit::setup_failed;
    Canvas canvas(device.get(), size);

    auto pool = wgc::Direct3D11CaptureFramePool::CreateFreeThreaded(rt_device, pixel_format, pool_buffers, size);
    auto session = pool.CreateCaptureSession(item);
    CaptureCloser closer{session, pool};
    if (auto cursor = session.try_as<wgc::IGraphicsCaptureSession2>()) {
        cursor.IsCursorCaptureEnabled(true);
    }
    if (auto interval = session.try_as<wgc::IGraphicsCaptureSession5>()) {
        interval.MinUpdateInterval(winrt::Windows::Foundation::TimeSpan(9'500'000 / options.fps));
    }
    auto arrived_signal = std::make_shared<winrt::handle>(event(false));
    auto closed_signal = std::make_shared<winrt::handle>(event(true));
    auto arrived = pool.FrameArrived(winrt::auto_revoke, [arrived_signal](auto const&, auto const&) {
        SetEvent(arrived_signal->get());
    });
    auto closed = item.Closed(winrt::auto_revoke, [closed_signal](auto const&, auto const&) {
        SetEvent(closed_signal->get());
    });

    if (control.reason() != Exit::none) return control.reason();
    session.StartCapture();
    exception_reason = Exit::capture_failed;
    if (control.reason() != Exit::none) return control.reason();
    if (auto result = canvas.header(control, output); result != Exit::none) return result;

    const auto timeout = std::chrono::milliseconds(options.stall_ms);
    control.capture_started_at.store(GetTickCount64());
    int64_t first_timestamp = 0, last_timestamp = 0, last_source_timestamp = 0, last_slot = -1;
    HANDLE signals[]{control.stop.get(), closed_signal->get(), arrived_signal->get()};
    for (;;) {
        if (control.reason() != Exit::none) return control.reason();
        if (WaitForSingleObject(closed_signal->get(), 0) == WAIT_OBJECT_0 || !options.target.current()) {
            return Exit::target_closed;
        }
        auto frame = pool.TryGetNextFrame();
        if (!frame) {
            if (WaitForMultipleObjects(3, signals, FALSE, 25) == WAIT_FAILED) winrt::throw_last_error();
            continue;
        }
        const auto content = frame.ContentSize();
        if (content.Width <= 0 || content.Height <= 0) continue;
        if (!valid_size(content)) return Exit::capture_failed;
        const auto timestamp = frame.SystemRelativeTime().count();
        if (timestamp > last_source_timestamp) {
            last_source_timestamp = timestamp;
            control.last_fresh_frame_at.store(GetTickCount64());
        }
        const bool resized = content.Width != size.Width || content.Height != size.Height;
        if (!first_timestamp && timestamp > 0) first_timestamp = timestamp;
        const auto elapsed = timestamp >= first_timestamp ? timestamp - first_timestamp : 0;
        const auto fps = static_cast<int64_t>(options.fps);
        const auto slot = elapsed / 10'000'000 * fps +
            (elapsed % 10'000'000 * fps + 5000 * fps) / 10'000'000;
        const bool fresh = timestamp > last_timestamp && slot > last_slot;
        if (fresh) {
            if (auto result = canvas.copy_frame(context.get(), frame, content, timestamp, control, Clock::now() + timeout);
                result != Exit::none) return result;
        }
        frame.Close();
        frame = nullptr;
        if (resized) {
            pool.Recreate(rt_device, pixel_format, pool_buffers, content);
            size = content;
        }
        if (!fresh) continue;
        if (control.reason() != Exit::none) return control.reason();
        if (auto result = control.write(output, canvas.record.data(), canvas.record.size());
            result != Exit::none) return result;
        last_timestamp = timestamp;
        last_slot = slot;
    }
}
} // namespace

int raw_wgc_main(int argc, wchar_t** argv) {
    Exit result = Exit::invalid_args;
    try {
        const auto options = parse(argc, argv);
        const HANDLE output = GetStdHandle(STD_OUTPUT_HANDLE);
        const auto output_type = GetFileType(output);
        if (output_type == FILE_TYPE_PIPE || output_type == FILE_TYPE_DISK) {
            result = Exit::setup_failed;
            result = capture(options, output, result);
        }
    } catch (...) {
        // Only the fixed protocol is sent to stderr. No titles, media, or API messages.
    }
    std::fprintf(stderr, "wgc_exit=%s\n", exit_name(result));
    return static_cast<int>(result);
}

#ifndef BAZZA_WGC_INTEGRATED
int wmain(int argc, wchar_t** argv) { return raw_wgc_main(argc, argv); }
#endif
