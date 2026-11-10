r"""Read-only RMS tempo and voiced F0 cross-check for an isolated Meet control.

Requires NumPy. Example from the isolated worktree on Windows:

    $q = 'E:\WSL\BazzaMeetAudioAuditQa'
    python -B tools/bazza-meet-audit/analyze_tempo.py "$q\control-2\scenario.json" "$q\run-bed9e9b2e9"

The named track and approximate onset come from analysis.json. This script uses
an independent time-domain comparison and does not decode or recognize speech.
"""

import argparse
from collections import Counter
import json
from pathlib import Path
import wave

import numpy as np


RATE = 16_000
HOP = RATE // 100
PITCH_HALF_FRAME = 3 * HOP
DURATION_RATIOS = np.arange(186, 233) / 200


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def pcm(directory, name):
    path = (directory / name).resolve()
    if not path.is_relative_to(directory.resolve()) or path.suffix.lower() != ".wav":
        raise ValueError("Expected a WAV inside the specified directory")
    with wave.open(str(path), "rb") as audio:
        if audio.getnchannels() != 1 or audio.getsampwidth() != 2 or not audio.getnframes():
            raise ValueError("Expected a nonempty mono PCM16 WAV")
        return np.frombuffer(audio.readframes(audio.getnframes()), "<i2").astype(float) / 32768, audio.getframerate()


def reference_at_16k(signal, rate):
    if rate == RATE:
        return signal
    if rate > RATE:
        # Apply a short anti-alias filter before interpolating the source WAV.
        taps = np.sinc(0.93 * RATE / rate * np.arange(-12, 13)) * np.hanning(25)
        signal = np.convolve(signal, taps / taps.sum(), mode="same")
    positions = np.arange(round(len(signal) * RATE / rate)) * rate / RATE
    return np.interp(positions, np.arange(len(signal)), signal)


def rms(signal):
    frames = np.lib.stride_tricks.sliding_window_view(np.pad(signal, (HOP, HOP)), 2 * HOP)[::HOP]
    return np.sqrt(np.mean(frames * frames, axis=1))


def best_envelope(reference, observed, approximate_start_ms, ratio):
    relative = np.arange(len(reference)) * ratio
    start = approximate_start_ms // 10
    starts = np.arange(max(0, start - 15), start + 16)
    starts = starts[starts + relative[-1] < len(observed)]
    if not len(starts):
        raise ValueError("No complete alignment window near the cue onset")
    observed = np.interp(starts[:, None] + relative, np.arange(len(observed)), observed)
    observed -= observed.mean(axis=1, keepdims=True)
    reference = reference - reference.mean()
    scale = np.linalg.norm(observed, axis=1) * np.linalg.norm(reference)
    scores = (observed @ reference) / np.maximum(scale, 1e-12)
    best = int(np.argmax(scores))
    return float(scores[best]), int(starts[best])


def f0(window):
    window = (window - window.mean()) * np.hanning(len(window))
    correlation = np.correlate(window, window, "full")[len(window) - 1:]
    if correlation[0] == 0:
        return 0.0, 0.0
    correlation /= correlation[0]
    peaks = np.flatnonzero((correlation[1:-1] > correlation[2:]) & (correlation[1:-1] >= correlation[:-2])) + 1
    peaks = peaks[(peaks >= 38) & (peaks <= 270)]
    if not len(peaks):
        return 0.0, 0.0
    accepted = peaks[correlation[peaks] >= max(0.4, 0.9 * correlation[peaks].max())]
    if not len(accepted):
        return 0.0, 0.0
    lag = accepted[0]
    return RATE / lag, correlation[lag]


def pitch_ratios(source, received, source_rms, received_rms, start, ratio):
    ratios = []
    for hop in range(10, len(source_rms) - 10, 3):
        at = start + hop * ratio
        if source_rms[hop] < source_rms.max() * 0.18 or received_rms[round(at)] < received_rms.max() * 0.10:
            continue
        src, dst = hop * HOP, round(at * HOP)
        if dst < PITCH_HALF_FRAME or dst + PITCH_HALF_FRAME > len(received):
            continue
        src_hz, src_confidence = f0(source[src - PITCH_HALF_FRAME:src + PITCH_HALF_FRAME])
        dst_hz, dst_confidence = f0(received[dst - PITCH_HALF_FRAME:dst + PITCH_HALF_FRAME])
        if min(src_confidence, dst_confidence) > 0.55 and 0.65 < dst_hz / src_hz < 1.55:
            ratios.append(dst_hz / src_hz)
    return ratios


def writer_gap_samples(coverage, start, end):
    if len(coverage["internal_gaps"]) != coverage["internal_gap_count"]:
        return None
    return sum(max(0, min(end, gap["end_sample"]) - max(start, gap["start_sample"])) for gap in coverage["internal_gaps"])


def compare(cue, template, track, scenario_dir, recording_dir, coverage):
    source, input_rate = pcm(scenario_dir, template["wav"])
    received, output_rate = pcm(recording_dir, track["file"])
    if output_rate != RATE:
        raise ValueError("Expected a 16 kHz recorded WAV")
    source = reference_at_16k(source, input_rate)
    source_rms, received_rms = rms(source), rms(received)
    onset = track["start_ms"]
    (score, start), ratio = max((best_envelope(source_rms, received_rms, onset, ratio), ratio) for ratio in DURATION_RATIOS)
    unstretched, _ = best_envelope(source_rms, received_rms, onset, 1.0)
    pitch = pitch_ratios(source, received, source_rms, received_rms, start, ratio)
    first = start * HOP
    return {
        "cue": cue["id"],
        "source_rate_hz": input_rate,
        "source_ms": round(len(source) * 1_000 / RATE, 1),
        "wav_rate_hz": output_rate,
        "rms_start_ms": start * 10,
        "estimated_duration_ratio": round(float(ratio), 3),
        "rms_correlation": round(score, 3),
        "rms_correlation_at_1": round(unstretched, 3),
        "voiced_f0_windows": len(pitch),
        "median_observed_to_source_f0": round(float(np.median(pitch)), 3) if pitch else None,
        "writer_internal_gap_samples_in_cue": writer_gap_samples(coverage, first, first + round(len(source) * ratio)),
    }


def analyze(scenario_path, run):
    scenario = {cue["id"]: cue for cue in read_json(scenario_path)["utterances"]}
    receiver = run / "receiver"
    recording = receiver / "attendee"
    manifest = read_json(recording / "tracks.json")
    coverage = {track["file"]: track["coverage"] for track in manifest["tracks"]}
    packet_bytes = Counter()
    consumed = 0
    with (receiver / "ingress.jsonl").open(encoding="utf-8-sig") as trace:
        for line in trace:
            row = json.loads(line)
            if row["event"] == "accepted_pcm":
                packet_bytes[row["bytes"]] += 1
            if row["event"] == "writer_consumed":
                consumed += 1
    cues = []
    for cue in read_json(run / "analysis.json")["cues"]:
        named = next((track for track in cue["tracks"] if track["named_speaker"] == cue["speaker"]), None)
        if named is not None:
            cues.append(compare(cue, scenario[cue["id"]], named, scenario_path.parent, recording, coverage[named["file"]]))
    return {
        "ingress_pcm_packet_bytes_histogram": dict(sorted(packet_bytes.items())),
        "writer_consumed_packets": consumed,
        "ingress_pcm_samples": sum((size // 2) * count for size, count in packet_bytes.items()),
        "wav_received_samples": sum(track["received_samples"] for track in manifest["tracks"]),
        "cues": cues,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", type=Path, help="synthetic control scenario JSON")
    parser.add_argument("run", type=Path, help="completed isolated run directory")
    args = parser.parse_args()
    print(json.dumps(analyze(args.scenario, args.run), indent=2))


if __name__ == "__main__":
    main()
