"""Generate a local A/V reference and measure evidence from a recorded MP4."""

import argparse
from array import array
import math
from pathlib import Path
import re
import subprocess
import sys
from tempfile import TemporaryDirectory
import wave

from measure import read_json, write_json

RATE = 8_000
FPS = 60
TONE_HZ = 1_000
PULSE_MS = 200
WINDOW_MS = 5


def run_ffmpeg(ffmpeg, arguments):
    result = subprocess.run(
        [ffmpeg, "-hide_banner", "-nostdin", *arguments],
        capture_output=True,
        check=False,
    )
    if result.returncode:
        raise ValueError(f"ffmpeg exited {result.returncode}: {result.stderr.decode(errors='replace')[-1500:]}")
    return result


def marker_schedule(duration_ms):
    if duration_ms < 12_000:
        raise ValueError("marker source needs at least 12 seconds")
    if duration_ms >= 75_000:
        windows = [5_000, duration_ms // 2 - 6_000, duration_ms - 20_000]
        # Cover all four frame phases when 60 Hz reference video is sampled
        # at the native 15 FPS default. Vary gaps so order can be validated.
        frames = [
            round(start * FPS / 1000) + 112 * i + 4 * i * i + i % 4
            for start in windows for i in range(7)
        ]
    else:
        frames = [round(time * FPS / 1000) for time in (2_000, duration_ms // 2, duration_ms - 3_000)]
    return [
        {"id": f"av-{i:02d}", "frame": frame, "at_ms": frame * 1000 / FPS}
        for i, frame in enumerate(frames, 1)
    ]


def write_tones(path, duration_ms, markers):
    pulse = array(
        "h", (round(8_000 * math.sin(2 * math.pi * TONE_HZ * i / RATE)) for i in range(RATE * PULSE_MS // 1000))
    )
    if sys.byteorder != "little":
        pulse.byteswap()
    cursor = 0
    with wave.open(str(path), "wb") as audio:
        audio.setparams((1, 2, RATE, 0, "NONE", "not compressed"))
        for marker in markers:
            start = round(marker["at_ms"] * RATE / 1000)
            audio.writeframesraw(b"\0\0" * (start - cursor))
            audio.writeframesraw(pulse.tobytes())
            cursor = start + len(pulse)
        audio.writeframesraw(b"\0\0" * (round(duration_ms * RATE / 1000) - cursor))


def make_reference(ffmpeg, output, manifest, duration_ms):
    if output.exists() or manifest.exists() or output.resolve() == manifest.resolve():
        raise ValueError("choose new paths for the marker video and manifest")
    markers = marker_schedule(duration_ms)
    frame_count = FPS * PULSE_MS // 1000
    green_frames = "+".join(
        f"between(n,{marker['frame']},{marker['frame'] + frame_count - 1})" for marker in markers
    )
    with TemporaryDirectory(prefix="bazza-av-reference-") as temporary:
        wav = Path(temporary) / "markers.wav"
        write_tones(wav, duration_ms, markers)
        run_ffmpeg(
            ffmpeg,
            [
                "-v", "error", "-f", "lavfi", "-i", f"color=black:s=320x180:r={FPS}:d={duration_ms / 1000}",
                "-i", str(wav),
                "-vf", f"drawbox=color=0x00ff00:t=fill:enable='{green_frames}'",
                "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                "-c:a", "aac", "-ar", "48000", "-movflags", "+faststart",
                "-n", str(output),
            ],
        )
    write_json(manifest, {"schema_version": 1, "duration_ms": duration_ms, "tone_hz": TONE_HZ, "markers": markers})


def decode_audio(ffmpeg, media, np):
    result = run_ffmpeg(
        ffmpeg,
        [
            "-v", "error", "-copyts", "-start_at_zero", "-i", str(media), "-map", "0:a:0",
            "-af", f"aresample={RATE}:async=1:first_pts=0",
            "-ac", "1", "-c:a", "pcm_f32le", "-f", "f32le", "pipe:1",
        ],
    )
    return np.frombuffer(result.stdout, dtype="<f4")


def beep_starts(audio, np):
    size = RATE * WINDOW_MS // 1000
    windows = audio[: len(audio) // size * size].reshape(-1, size)
    phase = np.arange(size, dtype=np.float32) * (2 * math.pi * TONE_HZ / RATE)
    energy = np.einsum("ij,ij->i", windows, windows)
    power = (windows @ np.cos(phase)) ** 2 + (windows @ np.sin(phase)) ** 2
    active = (energy > size * 0.004**2) & (2 * power > 0.65 * size * energy)
    active_indices = np.flatnonzero(active)
    runs = np.split(active_indices, np.where(np.diff(active_indices) > 2)[0] + 1)
    return [int(run[0]) * WINDOW_MS for run in runs if len(run) * WINDOW_MS >= 70]


def flash_starts(ffmpeg, media, roi):
    x, y, width, height = roi
    result = run_ffmpeg(
        ffmpeg,
        [
            "-v", "info", "-copyts", "-start_at_zero", "-i", str(media), "-map", "0:v:0",
            "-vf", f"crop={width}:{height}:{x}:{y}:exact=1,format=rgb24,scale=1:1:flags=area,settb=1/1000000,showinfo",
            "-an", "-fps_mode", "passthrough", "-c:v", "rawvideo", "-pix_fmt", "rgb24",
            "-threads", "1", "-f", "rawvideo", "pipe:1",
        ],
    )
    timestamps = [int(t) / 1000 for t in re.findall(rb"showinfo[^\n]*?\bn:\s*\d+\s+pts:\s*(-?\d+)", result.stderr)]
    if not timestamps or len(result.stdout) != 3 * len(timestamps):
        raise ValueError("could not pair RGB samples with decoded video presentation timestamps")
    flashes = []
    previous = None
    active = False
    for i, time_ms in enumerate(timestamps):
        red, green, blue = result.stdout[3 * i : 3 * (i + 1)]
        green_now = green > 105 and green > red + 55 and green > blue + 55
        if green_now and not active:
            flashes.append((time_ms, previous))
        active = green_now
        previous = time_ms
    return flashes


def observe_markers(audio, flashes, expected, np):
    beeps = beep_starts(audio, np)
    summary = {"markers": [], "marker_issues": [], "detected_beeps_ms": beeps, "detected_flashes": flashes}
    if len(beeps) != len(expected) or len(flashes) != len(expected):
        summary["marker_issues"].append(
            f"Expected {len(expected)} marker pairs, saw {len(beeps)} beeps and {len(flashes)} flashes."
        )
        return summary
    if any(previous is None for _, previous in flashes):
        summary["marker_issues"].append("A flash is already visible in the first decoded frame.")
        return summary
    for i, (marker, beep, flash) in enumerate(zip(expected, beeps, flashes)):
        if i:
            interval = marker["at_ms"] - expected[i - 1]["at_ms"]
            if abs((beep - beeps[i - 1]) - interval) > 150 or abs((flash[0] - flashes[i - 1][0]) - interval) > 150:
                summary["marker_issues"].append(f"Marker spacing does not establish pairing before {marker['id']}.")
        summary["markers"].append(
            {"id": marker["id"], "audio_ms": beep, "video_ms": flash[0],
             "video_previous_ms": flash[1], "audio_uncertainty_ms": WINDOW_MS * 2}
        )
    return summary


def correlate(haystack, needle, np, *, minimum_confidence=0.48, minimum_margin=0.10):
    """Locate a clip in a small window without comparing to ASR timestamps."""
    needle = needle - np.mean(needle)
    count = len(needle)
    norm = np.linalg.norm(needle)
    if count < RATE // 5 or len(haystack) < count or norm < 1e-5:
        return None
    fft_size = 1 << (len(haystack) + count - 2).bit_length()
    numerators = np.fft.irfft(
        np.fft.rfft(haystack, fft_size) * np.fft.rfft(needle[::-1], fft_size), fft_size
    )[count - 1 : len(haystack)]
    sums = np.concatenate(([0.0], np.cumsum(haystack, dtype=np.float64)))
    energies = np.concatenate(([0.0], np.cumsum(haystack**2, dtype=np.float64)))
    power = energies[count:] - energies[:-count] - (sums[count:] - sums[:-count]) ** 2 / count
    scores = np.abs(numerators) / (norm * np.sqrt(np.maximum(power, 1e-8)))
    position = int(np.argmax(scores))
    confidence = float(scores[position])
    scores[max(0, position - RATE // 8) : position + RATE // 8 + 1] = 0
    if confidence < minimum_confidence or confidence - float(np.max(scores)) < minimum_margin:
        return None
    return position, round(confidence, 4)


def locate_phrase(haystack, reference, np):
    match = correlate(haystack, reference, np)
    if match:
        return (*match, "matched_reference_waveform")
    # A loud overlapping speaker can depress the full-clip score. Accept
    # separate weaker matches only if at least two parts agree on the onset.
    length = len(reference)
    if length < 2 * RATE or len(haystack) < length:
        return None
    divisions = (0, length // 3, 2 * length // 3, length)
    matches = []
    for start, end in zip(divisions, divisions[1:]):
        local = correlate(
            haystack[start : len(haystack) - length + end], reference[start:end], np,
            minimum_confidence=0.35, minimum_margin=0.08,
        )
        if local:
            matches.append(local)
    if len(matches) < 2 or max(index for index, _ in matches) - min(index for index, _ in matches) > RATE // 50:
        return None
    index, _ = max(matches, key=lambda match: match[1])
    return index, min(confidence for _, confidence in matches), "matched_waveform_parts"


def energy_start(waveform, np):
    size = RATE // 100
    windows = waveform[: len(waveform) // size * size].reshape(-1, size)
    if not len(windows):
        return None
    rms = np.sqrt(np.einsum("ij,ij->i", windows, windows) / size)
    active = rms > max(0.001, float(np.max(rms)) * 0.015)
    beginnings = np.flatnonzero(active[:-2] & active[1:-1] & active[2:])
    return int(beginnings[0]) * 10 if len(beginnings) else None


def observe_phrases(ffmpeg, audio, manifest_path, schedule_offset_ms, search_ms, np):
    manifest = read_json(manifest_path)
    if manifest.get("schema_version") != 1:
        raise ValueError("expected corpus schema_version 1")
    results, unresolved = [], []
    for utterance in manifest["utterances"]:
        phrase_id = utterance["id"]
        path = manifest_path.parent / utterance["wav"]
        if not path.is_file():
            unresolved.append({"id": phrase_id, "reason": "missing reference WAV"})
            continue
        waveform = decode_audio(ffmpeg, path.resolve(), np)
        start_ms = utterance["speech_start_ms"]
        end_ms = utterance["speech_end_ms"]
        clip_start = round(start_ms * RATE / 1000)
        clip_end = min(round(end_ms * RATE / 1000), len(waveform))
        if clip_start < 0 or clip_end <= clip_start:
            raise ValueError(f"invalid speech boundaries in reference {phrase_id}")
        reference = waveform[clip_start:clip_end]
        scheduled = utterance["at_ms"] + schedule_offset_ms + start_ms
        low = max(0, round((scheduled - search_ms) * RATE / 1000))
        high = round((scheduled + search_ms) * RATE / 1000) + len(reference)
        match = locate_phrase(audio[low:high], reference, np) if high > low else None
        if match is None:
            unresolved.append({"id": phrase_id, "reason": "no unique audio correlation"})
            continue
        position, confidence, method = match
        observed_start = (low + position) * 1000 / RATE
        onset = energy_start(waveform, np)
        if onset is None:
            unresolved.append({"id": phrase_id, "reason": "reference has no sustained audio onset"})
            continue
        onset_difference = onset - start_ms
        # Provider character timestamps and the clip's sustained acoustic
        # onset can disagree. Keep both estimates and leave room for the
        # 10 ms energy window and codec resampling.
        results.append(
            {
                "id": phrase_id,
                "start_ms": observed_start,
                "start_low_ms": max(0, observed_start + min(0, onset_difference) - 20),
                "start_high_ms": observed_start + max(0, onset_difference) + 20,
                "end_ms": observed_start + len(reference) * 1000 / RATE,
                "method": method,
                "correlation": confidence,
                "reference_sustained_audio_start_ms": onset,
            }
        )
    return {"phrases": results, "unresolved_phrases": unresolved}


def roi_value(value):
    try:
        x, y, width, height = map(int, value.split(","))
        if min(x, y) < 0 or min(width, height) < 4:
            raise ValueError
    except ValueError:
        raise argparse.ArgumentTypeError("ROI must be x,y,width,height in recorded pixels, with width and height >= 4")
    return x, y, width, height


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    reference = commands.add_parser("reference", help="make a synchronized local marker MP4")
    reference.add_argument("output", type=Path)
    reference.add_argument("marker_source", type=Path)
    reference.add_argument("--duration-ms", required=True, type=int)
    reference.add_argument("--ffmpeg", default="ffmpeg")
    observe = commands.add_parser("observe", help="extract independently observed marker and phrase times")
    observe.add_argument("media", type=Path)
    observe.add_argument("--marker-source", type=Path)
    observe.add_argument("--roi", type=roi_value)
    observe.add_argument("--manifest", type=Path)
    observe.add_argument("--schedule-offset-ms", type=float, default=0)
    observe.add_argument("--search-ms", type=float, default=5000)
    observe.add_argument("--output", type=Path)
    observe.add_argument("--ffmpeg", default="ffmpeg")
    args = parser.parse_args()
    try:
        if args.command == "reference":
            make_reference(args.ffmpeg, args.output, args.marker_source, args.duration_ms)
            return 0
        if bool(args.marker_source) != bool(args.roi):
            raise ValueError("use --marker-source together with --roi")
        if not args.marker_source and not args.manifest:
            raise ValueError("provide marker or phrase reference evidence")
        if not args.media.is_file() or not 0 < args.search_ms <= 60_000:
            raise ValueError("need an existing local recording and 0 < --search-ms <= 60000")
        import numpy as np

        audio = decode_audio(args.ffmpeg, args.media.resolve(), np)
        report = {"schema_version": 1, "media": str(args.media.resolve())}
        if args.marker_source:
            expected = read_json(args.marker_source)["markers"]
            flashes = flash_starts(args.ffmpeg, args.media.resolve(), args.roi)
            report.update(observe_markers(audio, flashes, expected, np))
        if args.manifest:
            report.update(
                observe_phrases(args.ffmpeg, audio, args.manifest, args.schedule_offset_ms, args.search_ms, np)
            )
        write_json(args.output, report)
        return 2 if report.get("marker_issues") or report.get("unresolved_phrases") else 0
    except (ImportError, OSError, KeyError, TypeError, ValueError) as error:
        parser.exit(2, f"Cannot collect evidence: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
