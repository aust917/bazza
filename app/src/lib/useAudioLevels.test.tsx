import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAudioLevels } from "./useAudioLevels";

const backend = vi.hoisted(() => ({ sampleLevels: vi.fn(), recordingStatus: vi.fn() }));
vi.mock("./ipc", () => ({ cmd: backend }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  backend.sampleLevels.mockResolvedValue([-30, -40]);
  backend.recordingStatus.mockResolvedValue({ state: "idle" });
});
afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); });

describe("audio meter polling", () => {
  it("serializes slow probes and leaves a gap after completion", async () => {
    const pending = deferred<[number, number]>();
    backend.sampleLevels.mockReturnValueOnce(pending.promise);
    const { result } = renderHook(() => useAudioLevels({ enabled: true }));
    await act(() => vi.advanceTimersByTimeAsync(5000));
    expect(backend.sampleLevels).toHaveBeenCalledTimes(1);

    await act(async () => { pending.resolve([-20, -25]); });
    expect(result.current).toEqual([-20, -25]);
    await act(() => vi.advanceTimersByTimeAsync(599));
    expect(backend.sampleLevels).toHaveBeenCalledTimes(1);
    await act(() => vi.advanceTimersByTimeAsync(1));
    expect(backend.sampleLevels).toHaveBeenCalledTimes(2);
  });

  it("paces retries when a device rejects immediately", async () => {
    backend.sampleLevels.mockRejectedValue(new Error("Device unavailable"));
    renderHook(() => useAudioLevels({ enabled: true }));
    await act(async () => {});
    expect(backend.sampleLevels).toHaveBeenCalledTimes(1);
    await act(() => vi.advanceTimersByTimeAsync(599));
    expect(backend.sampleLevels).toHaveBeenCalledTimes(1);
    await act(() => vi.advanceTimersByTimeAsync(1));
    expect(backend.sampleLevels).toHaveBeenCalledTimes(2);
  });

  it("discards an old device result without overlapping the replacement probe", async () => {
    const pending = deferred<[number, number]>();
    backend.sampleLevels.mockReturnValueOnce(pending.promise);
    const { result, rerender } = renderHook(({ mic }) => useAudioLevels({ enabled: true, mic }), { initialProps: { mic: "old" } });
    rerender({ mic: "new" });
    await act(() => vi.advanceTimersByTimeAsync(1200));
    expect(backend.sampleLevels).toHaveBeenCalledTimes(1);
    await act(async () => { pending.resolve([-10, -15]); });
    expect(result.current).toEqual([-100, -100]);
    await act(() => vi.advanceTimersByTimeAsync(600));
    expect(backend.sampleLevels).toHaveBeenLastCalledWith("new", null);
    expect(backend.sampleLevels).toHaveBeenCalledTimes(2);
  });

  it("does not enqueue an audio probe after an unmounted status request resolves", async () => {
    const pending = deferred<{ state: "idle" }>();
    backend.recordingStatus.mockReturnValue(pending.promise);
    const { unmount } = renderHook(() => useAudioLevels({ enabled: true, onlyWhenIdle: true }));
    unmount();
    await act(async () => { pending.resolve({ state: "idle" }); });
    await act(() => vi.advanceTimersByTimeAsync(5000));
    expect(backend.sampleLevels).not.toHaveBeenCalled();
    expect(backend.recordingStatus).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("leaves devices to the recorder while recording", async () => {
    backend.recordingStatus.mockResolvedValueOnce({ state: "recording" });
    renderHook(() => useAudioLevels({ enabled: true, onlyWhenIdle: true }));
    await act(async () => {});
    expect(backend.sampleLevels).not.toHaveBeenCalled();
    await act(() => vi.advanceTimersByTimeAsync(600));
    expect(backend.sampleLevels).toHaveBeenCalledTimes(1);
  });

  it("pauses hidden windows and ignores results arriving after they hide", async () => {
    let hidden = true;
    vi.spyOn(document, "hidden", "get").mockImplementation(() => hidden);
    const pending = deferred<[number, number]>();
    backend.sampleLevels.mockReturnValueOnce(pending.promise);
    const { result } = renderHook(() => useAudioLevels({ enabled: true }));
    expect(backend.sampleLevels).not.toHaveBeenCalled();
    hidden = false;
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    await act(() => vi.advanceTimersByTimeAsync(0));
    expect(backend.sampleLevels).toHaveBeenCalledTimes(1);
    hidden = true;
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    await act(async () => { pending.resolve([-10, -15]); });
    expect(result.current).toEqual([-100, -100]);
    await act(() => vi.advanceTimersByTimeAsync(5000));
    expect(backend.sampleLevels).toHaveBeenCalledTimes(1);
    hidden = false;
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    await act(() => vi.advanceTimersByTimeAsync(0));
    expect(backend.sampleLevels).toHaveBeenCalledTimes(2);
  });
});
