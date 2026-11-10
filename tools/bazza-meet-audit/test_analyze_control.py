"""Offline cue identity and timing check for the short-control analyzer."""

import json
from pathlib import Path
from tempfile import TemporaryDirectory
import wave

import numpy as np

from analyze_control import BROWSER_PREFIX, RATE, WSS_PREFIX, analyze


def write_wav(path, samples):
    with wave.open(str(path), "wb") as output:
        output.setparams((1, 2, RATE, 0, "NONE", "not compressed"))
        output.writeframes((samples * 32767).astype("<i2").tobytes())


def cue(frequencies, section_ms=400, sweep=False):
    time = np.arange(RATE * section_ms // 1_000) / RATE
    progress = time / (section_ms / 1_000)
    phase_time = time * (0.55 + 0.65 * progress) if sweep else time
    return np.concatenate([
        0.35 * np.sin(np.pi * progress) ** 2 * np.sin(2 * np.pi * hz * phase_time)
        for hz in frequencies
    ])


def render(pieces):
    result = np.zeros(RATE * 13)
    for at_ms, samples in pieces:
        start = at_ms * RATE // 1_000
        result[start:start + len(samples)] += samples * 0.6
    return result


def journal(path, rows):
    path.write_text("\n".join(map(json.dumps, rows)), encoding="utf-8")


def test(root):
    origin = 1_793_740_000_000_000
    first = cue([230, 345, 560, 410, 290, 600])
    second = cue([1100, 780, 1390, 1280, 950, 720])
    write_wav(root / "cue-01.wav", first)
    write_wav(root / "cue-02.wav", second)
    utterances = [
        {"id": "early-s1", "participant_id": "s1", "duration_ms": 2400, "wav": "cue-01.wav"},
        {"id": "early-s2", "participant_id": "s2", "duration_ms": 2400, "wav": "cue-02.wav"},
        {"id": "repeat-s2", "participant_id": "s2", "duration_ms": 2400, "wav": "cue-02.wav"},
    ]
    scenario = root / "scenario.json"
    scenario.write_text(json.dumps({"utterances": utterances}), encoding="utf-8")
    events = [{"event": "scenario_start", "origin_utc_ns": origin * 1000 + 1_000_000_000,
               "origin_mono_ns": 1_000_000_000}]
    for speaker in ("s1", "s2"):
        events.append({"event": "join_requested", "bot_name": speaker, "participant_id": speaker})
    for clip, dispatch in zip(utterances, (1300, 4700, 9000), strict=True):
        for name, extra in (("audio_post_started", 0), ("audio_post_ack", 90)):
            events.append({"event": name, "utterance_id": clip["id"], "mono_ns": (dispatch + extra) * 1_000_000})
    journal(root / "speakers.jsonl", events)

    directory = root / "receiver" / "attendee"
    directory.mkdir(parents=True)
    write_wav(directory / "participant-001.wav", render([(1460, first)]))
    write_wav(directory / "participant-002.wav", render([(9160, second)]))
    independent = root / "independent.wav"
    write_wav(independent, render([(1460, first), (4860, second), (9160, second)]))
    manifest = {
        "sample_rate": RATE,
        "timing": {"origin_unix_micros": origin, "excluded_pause_ms": 0,
                   "queue_dropped_packets": 0, "provider_clock_reanchors": 0},
        "tracks": [{"display_name": speaker, "file": f"participant-{i:03}.wav",
                    "coverage": {"first_pcm_sample": first_ms * RATE // 1000}}
                   for i, (speaker, first_ms) in enumerate((("s1", 1460), ("s2", 9160)), 1)],
    }
    (directory / "tracks.json").write_text(json.dumps(manifest), encoding="utf-8")
    journal(directory.parent / "ingress.jsonl", [
        {"event": "audit_start", "origin_unix_us": origin - 5_000_000, "monotonic_us": 0},
        {"event": "adopted", "origin_unix_us": origin, "monotonic_us": 5_002_000},
        {"event": "raw_text", "packet": 1, "monotonic_us": 4_900_000},
        {"event": "rejected_before_adopt", "packet": 1, "monotonic_us": 4_900_020},
        {"event": "wss_handshake_accepted", "monotonic_us": 6_440_000},
        {"event": "writer_consumed", "packet": 2, "monotonic_us": 9_000_000},
        {"event": "raw_text", "packet": 2, "monotonic_us": 6_460_000},
        {"event": "accepted_pcm", "packet": 2, "monotonic_us": 6_460_090},
        {"event": "audit_end", "monotonic_us": 16_000_000, "dropped_audit_events": 0},
    ])
    before = {"schema": 1, "pid": 10, "worker_mono_ns": 1_000_000_000, "elapsed_ms": 1_000,
              "worker_unix_ms": origin // 1000,
              "counters": {"frames_seen": 0, "frames_mapped": 0}}
    after = {**before, "worker_mono_ns": 10_100_000_000, "elapsed_ms": 10_100,
             "worker_unix_ms": origin // 1000 + 9_100,
             "counters": {"frames_seen": 30, "frames_mapped": 10}}
    other_active = {**after, "worker_mono_ns": 10_200_000_000, "elapsed_ms": 200,
                    "worker_unix_ms": origin // 1000 + 9_200,
                    "counters": {"frames_seen": 2, "frames_mapped": 0}}
    other_empty = {**before, "worker_mono_ns": 10_300_000_000, "elapsed_ms": 100,
                   "worker_unix_ms": origin // 1000 + 9_300}
    worker = {"schema": 1, "pid": 10, "mono_ns": 3, "unix_ms": origin // 1000 + 9300,
              "worker": {"counts": {"browser_per_participant_received": 10}, "first": {}},
              "clients": {"1": {"counts": {"per_participant_sent": 9}, "first": {
                  "connected": {"unix_ms": origin // 1000 + 1450, "mono_ns": 2},
              }}}}
    browser_lines = [BROWSER_PREFIX + json.dumps(row) for row in (before, after, other_active, other_empty)]
    (root / "provider-audit.log").write_text("\n".join(browser_lines + [WSS_PREFIX + json.dumps(worker)]),
                                            encoding="utf-8")

    result = analyze(scenario, root, independent, origin)
    early_1, early_2, repeat = result["cues"]
    assert early_1["dispatch_ms"] == 1300 and early_1["tracks"][0]["start_ms"] == 1460
    assert early_1["status"] == "matched_named"
    assert early_2["status"] == "unresolved" and early_2["nominal_clip_before_named_pcm"]
    assert early_2["independent_reference"]["distinct"]
    assert "late_named_tracks" not in early_2  # The known late PCM is the scheduled repeat.
    assert repeat["status"] == "matched_named"
    assert result["early_cue_status"] == {"matched_named": 1, "unresolved": 1}
    assert result["ingress"]["early_packet_events"] == {"raw_text": 1, "accepted_pcm": 1, "writer_consumed": 1}
    assert result["ingress"]["packet_events"]["rejected_before_adopt"] == 1
    assert result["provider"][0]["browser_early_bracket"]["counts"]["frames_mapped"] == 10
    assert result["provider"][0]["browser_totals"]["frames_seen"] == 30
    assert [row["media_observed"] for row in result["provider"][0]["browser_series"]] == [True, True, False]
    assert result["provider"][0]["worker_first_event_ms"]["connected"] == 1450
    assert result["provider"][0]["worker_connected_minus_bazza_handshake_ms"] == 10
    assert result["provider"][0]["worker_early_bracket"] is None

    truncated = second.copy()
    truncated[:RATE * 8 // 10] = 0
    write_wav(directory / "participant-001.wav", render([(1460, first), (4860, second)]))
    write_wav(directory / "participant-002.wav", render([(9160, truncated)]))
    modified = analyze(scenario, root)["cues"]
    assert modified[1]["status"] == "matched_elsewhere"
    assert modified[2]["status"] == "partial_named"
    assert modified[2]["tracks"][0]["matched_section_offsets_ms"] == [800, 1200, 1600, 2000]

    frequencies = [230, 345, 560, 410, 290, 600]
    write_wav(root / "cue-01.wav", cue(frequencies, sweep=True))
    stretched = cue(frequencies, section_ms=444, sweep=True)
    write_wav(directory / "participant-001.wav", render([(1460, stretched)]))
    actual = analyze(scenario, root)["cues"][0]
    assert actual["status"] == "matched_named"
    track = actual["tracks"][0]
    assert 1.09 <= track["estimated_duration_ratio"] <= 1.13
    assert abs(track["start_ms"] - 1460) <= 20

    write_wav(root / "cue-01.wav", first)
    write_wav(directory / "participant-001.wav", render([(7260, first[:RATE * 8 // 10])]))
    manifest["tracks"][0]["coverage"]["first_pcm_sample"] = 7260 * RATE // 1_000
    (directory / "tracks.json").write_text(json.dumps(manifest), encoding="utf-8")
    late = analyze(scenario, root)["cues"][0]
    assert late["status"] == "unresolved" and late["nominal_clip_before_named_pcm"]
    assert abs(late["late_named_tracks"][0]["start_after_dispatch_ms"] - 5960) <= 40
    assert late["late_named_tracks"][0]["matched_section_offsets_ms"] == [0, 400]


if __name__ == "__main__":
    with TemporaryDirectory(dir=Path(__file__).resolve().parent) as directory:
        test(Path(directory))
    print("Offline fixture verified WAV identity, delayed and missing audio, cue timing, and aligned counters.")
