import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { createRef } from "react";
import { Player, type PlayerHandle } from "./Player";

const convert = vi.hoisted(() => vi.fn((path: string) => `https://asset.localhost/${path}`));
vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: convert }));

beforeEach(() => { convert.mockClear(); vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {}); });
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it("shows a media load failure and clears it for a different recording", () => {
  const onTime = vi.fn();
  const { container, rerender } = render(<Player audio="one.wav" video={null} onTime={onTime} />);
  fireEvent.error(container.querySelector("audio")!);
  expect(screen.getByRole("alert")).toHaveTextContent("Could not play this recording");
  rerender(<Player audio="two.wav" video={null} onTime={onTime} />);
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("handles a rejected play request without leaving a silent zero-duration player", async () => {
  vi.spyOn(HTMLMediaElement.prototype, "play").mockRejectedValue(new DOMException("File blocked", "NotSupportedError"));
  render(<Player audio="one.wav" video={null} onTime={vi.fn()} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Play" })); });
  expect(screen.getByRole("alert")).toBeInTheDocument();
});

it("ignores playback aborted by changing the source", async () => {
  vi.spyOn(HTMLMediaElement.prototype, "play").mockRejectedValue(new DOMException("Source changed", "AbortError"));
  render(<Player audio="one.wav" video={null} onTime={vi.fn()} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Play" })); });
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("ignores a late playback rejection from the previous recording", async () => {
  let reject!: (error: Error) => void;
  vi.spyOn(HTMLMediaElement.prototype, "play").mockReturnValue(new Promise<void>((_, fail) => { reject = fail; }));
  const onTime = vi.fn();
  const { rerender } = render(<Player audio="one.wav" video={null} onTime={onTime} />);
  fireEvent.click(screen.getByRole("button", { name: "Play" }));
  rerender(<Player audio="two.wav" video={null} onTime={onTime} />);
  await act(async () => { reject(new Error("Old source failed")); });
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("reads metadata that loaded before effect subscription", () => {
  vi.spyOn(HTMLMediaElement.prototype, "duration", "get").mockReturnValue(113);
  render(<Player audio="one.wav" video="screen.mp4" onTime={vi.fn()} />);
  expect(screen.getByRole("slider")).toHaveAttribute("aria-valuemax", "113000");
});

it("routes a service playback token through the private protocol", () => {
  render(<Player audio="bazza-media:opaque-audio" video={null} onTime={vi.fn()} />);
  expect(convert).toHaveBeenCalledExactlyOnceWith("opaque-audio", "bazza-media");
});

it("renders no media element, waveform or timeline when no recording is attached", () => {
  const onTime = vi.fn();
  const { container } = render(<Player audio={null} video={null} highlights={[1000]} onTime={onTime} />);
  expect(container).toBeEmptyDOMElement();
  expect(convert).not.toHaveBeenCalled();
  expect(screen.queryByRole("slider")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Play" })).not.toBeInTheDocument();
  expect(onTime).toHaveBeenLastCalledWith(0);
});

it("stops the old source, clears the seek handle and ignores its late rejection when media disappears", async () => {
  let reject!: (error: Error) => void;
  vi.spyOn(HTMLMediaElement.prototype, "play").mockReturnValue(new Promise<void>((_, fail) => { reject = fail; }));
  const onTime = vi.fn(), handle = createRef<PlayerHandle>();
  const { container, rerender } = render(<Player ref={handle} audio="old.wav" video={null} onTime={onTime} />);
  const old = container.querySelector("audio")!;
  fireEvent.click(screen.getByRole("button", { name: "Play" }));
  old.currentTime = 23;
  fireEvent.timeUpdate(old);
  expect(onTime).toHaveBeenLastCalledWith(23000);
  rerender(<Player ref={handle} audio={null} video={null} onTime={onTime} />);
  expect(old.pause).toHaveBeenCalled();
  expect(handle.current).toBeNull();
  expect(container).toBeEmptyDOMElement();
  expect(onTime).toHaveBeenLastCalledWith(0);
  await act(async () => { reject(new Error("Detached source failed")); });
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("keeps a real video playable without a separate audio path", () => {
  const { container } = render(<Player audio={null} video="screen.mp4" onTime={vi.fn()} />);
  expect(container.querySelector("video")).toHaveAttribute("src", "https://asset.localhost/screen.mp4");
  expect(container.querySelector("audio")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Play" })).toBeEnabled();
});
