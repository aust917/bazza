"""Align one short Meet control's cue WAVs and numeric receiver diagnostics."""

import argparse
from collections import Counter, defaultdict
import json
from pathlib import Path
import wave

import numpy as np

from audit import by_utterance, read_events, read_json


RATE = 16_000
HOP_MS = 20
HOP = RATE * HOP_MS // 1_000
FRAME = HOP * 2
SECTION = 20  # 400 ms
LEAD_MS = 200
DELAY_MS = 3_500
SECTION_SCORE = 0.75
IDENTITY_MARGIN = 0.055
IDENTITY_ALIGNMENT_MS = 80
BROWSER_ORIGIN_TOLERANCE_MS = 100
DURATION_RATIOS = [step / 100 for step in sorted(range(90, 117), key=lambda step: abs(step - 100))]
BROWSER_PREFIX = "BAZZA_MEET_AUDIT_BROWSER "
WSS_PREFIX = "BAZZA_MEET_AUDIT_WSS "


def wav_under(directory, name):
    path = (directory / name).resolve()
    if not path.is_relative_to(directory.resolve()) or path.suffix.lower() != ".wav":
        raise ValueError("Expected a WAV inside its fixture or recording directory")
    return path


def features(path):
    with wave.open(str(path), "rb") as audio:
        if audio.getsampwidth() != 2 or audio.getnframes() == 0:
            raise ValueError("Expected a nonempty PCM16 WAV")
        rate = audio.getframerate()
        samples = np.frombuffer(audio.readframes(audio.getnframes()), "<i2")
        samples = samples.reshape(-1, audio.getnchannels()).mean(axis=1) / 32768
    if rate != RATE:
        width = rate // RATE
        if width > 1:
            samples = np.convolve(samples, np.ones(width) / width, mode="same")
        positions = np.arange(round(len(samples) * RATE / rate)) * rate / RATE
        samples = np.interp(positions, np.arange(len(samples)), samples)
    samples = np.pad(samples, (0, FRAME))
    frames = np.lib.stride_tricks.sliding_window_view(samples, FRAME)[::HOP]
    rms = np.sqrt((frames * frames).mean(axis=1))
    active = rms >= max(0.001, float(rms.max()) * 0.04)
    spectrum = np.abs(np.fft.rfft(frames * np.hanning(FRAME)))
    bins = np.unique(np.round(np.geomspace(120, 3_900, 23) * FRAME / RATE).astype(int))
    bands = np.stack([spectrum[:, lo:hi].mean(axis=1) for lo, hi in zip(bins, bins[1:])], axis=1)
    bands = np.log(bands + 0.0003)
    bands -= bands.mean(axis=1, keepdims=True)
    bands /= np.maximum(np.linalg.norm(bands, axis=1, keepdims=True), 1e-8)
    return bands.astype(np.float32), active


def match(template, recording, first_ms, last_ms):
    """Slide log-spectrum patterns with a bounded affine timing adjustment."""
    reference, reference_active = template
    observed, observed_active = recording
    lo = max(0, int(np.ceil(first_ms / HOP_MS)))
    hi = min(len(observed) - 1, int(np.floor(last_ms / HOP_MS)))
    starts = np.arange(lo, hi + 1)
    if starts.size == 0:
        return None
    sections = []
    for offset in range(0, len(reference), SECTION):
        frames = np.flatnonzero(reference_active[offset:offset + SECTION]) + offset
        if len(frames) >= SECTION // 5:
            sections.append((offset * HOP_MS, frames))
    if not sections:
        return None
    best_quality = -1
    for ratio in DURATION_RATIOS:
        scores = []
        for _, frames in sections:
            locations = starts[:, None] + np.rint(frames * ratio).astype(int)
            valid = locations < len(observed)
            locations = np.minimum(locations, len(observed) - 1)
            aligned = np.einsum("wfb,fb->wf", observed[locations], reference[frames])
            aligned *= valid & observed_active[locations]
            scores.append(np.maximum(0, aligned.mean(axis=1)))
        scores = np.stack(scores)
        good = (scores >= SECTION_SCORE).sum(axis=0)
        quality = 0.65 * scores.mean(axis=0) + 0.35 * good / len(sections)
        best = int(np.argmax(quality))
        if quality[best] > best_quality:
            best_quality = float(quality[best])
            best_start = int(starts[best] * HOP_MS)
            best_ratio = ratio
            best_scores = scores[:, best]
    return {
        "start_ms": best_start,
        "estimated_duration_ratio": best_ratio,
        "quality": best_quality,
        "matched_sections": int((best_scores >= SECTION_SCORE).sum()),
        "active_sections": len(sections),
        "matched_section_offsets_ms": [offset for (offset, _), score in zip(sections, best_scores, strict=True)
                                       if score >= SECTION_SCORE],
        "section_scores": [round(float(score), 3) for score in best_scores],
    }


def named_tracks(manifest, events, directory):
    names = {}
    for event in events:
        if event["event"] == "join_requested":
            name, speaker = event["bot_name"], event["participant_id"]
            if names.setdefault(name, speaker) != speaker:
                raise ValueError("One display name was reused by different fixture speakers")
    tracks = []
    for track in manifest["tracks"]:
        tracks.append((track, names.get(track.get("display_name")), features(wav_under(directory, track["file"]))))
    return tracks


def candidate(expected, alternatives, signal, first_ms, last_ms):
    result = match(expected, signal, first_ms, last_ms)
    if result is None:
        return None
    at = result["start_ms"]
    other = max((value["quality"] for other_cue in alternatives if (value := match(
        other_cue, signal, at - IDENTITY_ALIGNMENT_MS, at + IDENTITY_ALIGNMENT_MS
    )) is not None), default=0)
    result["identity_margin"] = round(result["quality"] - other, 3)
    result["distinct"] = (result["matched_sections"] >= min(2, result["active_sections"])
                          and result["identity_margin"] >= IDENTITY_MARGIN)
    result["quality"] = round(result["quality"], 3)
    return result


def ingress_summary(events, origin_us, early_window):
    if not events:
        return None
    anchor, = [row for row in events if row["event"] == "audit_start"]
    adopted, = [row for row in events if row["event"] == "adopted"]
    if adopted.get("origin_unix_us") != origin_us:
        raise ValueError("Bazza ingress and WAV manifest belong to different recording origins")

    def at(row):
        return (anchor["origin_unix_us"] + row["monotonic_us"] - origin_us) / 1_000

    packet_times = {row["packet"]: at(row) for row in events if row["event"] in {"raw_text", "raw_binary"}}
    stages = {"raw_text", "raw_binary", "accepted_pcm", "queued_ingress", "queued_writer", "writer_consumed"}
    packet_events = [row for row in events if "packet" in row and
                     (row["event"] in stages or row["event"].startswith(("queue_", "rejected_")))]
    early = Counter(row["event"] for row in packet_events if early_window and
                    early_window[0] <= packet_times.get(row["packet"], at(row)) <= early_window[1])
    milestones = {}
    for name in ("adopted", "phase_connected", "wss_handshake_accepted", "raw_text", "accepted_pcm",
                 "queued_writer", "writer_consumed", "cleanup_confirmed"):
        times = [at(row) for row in events if row["event"] == name]
        if times:
            milestones[name] = round(min(times), 3)
    return {
        "first_event_ms": milestones,
        "packet_events": dict(Counter(row["event"] for row in packet_events)),
        "early_packet_events": dict(early),
        "dropped_audit_events": next((row.get("dropped_audit_events") for row in events
                                      if row["event"] == "audit_end"), None),
    }


def worker_counts(row):
    counts = Counter(row["worker"]["counts"])
    for client in row["clients"].values():
        counts.update(client["counts"])
    return counts


def snapshot_window(rows, early_window):
    if not early_window:
        return None
    lo, hi = early_window
    before = next(((when, counts) for when, counts in reversed(rows) if when <= lo), None)
    after = next(((when, counts) for when, counts in rows if when >= hi), None)
    if before is None or after is None:
        return None
    delta = {key: value - before[1].get(key, 0) for key, value in after[1].items()}
    if any(value < 0 for value in delta.values()):
        return None
    return {"snapshot_bounds_ms": [round(before[0], 3), round(after[0], 3)], "counts": delta}


def browser_series(rows, relative, early_window):
    """Separate likely document realms without treating a process as a single page."""
    series = []
    for row in rows:
        # Both clocks are monotonic. Their difference estimates the browser
        # realm's time origin in the worker clock, with message delivery jitter.
        origin = row["worker_mono_ns"] / 1_000_000 - row["elapsed_ms"]
        compatible = []
        for prior_origin, snapshots in series:
            previous = snapshots[-1]
            distance = abs(prior_origin - origin)
            if (distance <= BROWSER_ORIGIN_TOLERANCE_MS and row["elapsed_ms"] >= previous["elapsed_ms"]
                    and all(row["counters"].get(key, 0) >= value for key, value in previous["counters"].items())):
                compatible.append((distance, snapshots))
        if compatible:
            min(compatible, key=lambda item: item[0])[1].append(row)
        else:
            series.append((origin, [row]))

    results = []
    for _, snapshots in series:
        times = [(relative(row["worker_unix_ms"]), row["counters"]) for row in snapshots]
        totals = times[-1][1]
        results.append({
            "snapshots": len(snapshots),
            "snapshot_span_ms": [round(times[0][0], 3), round(times[-1][0], 3)],
            "media_observed": bool(totals.get("frames_seen") or totals.get("receiver_count")),
            "totals": totals,
            "early_bracket": snapshot_window(times, early_window),
        })
    return results


def provider_summary(path, origin_us, early_window, handshake_ms):
    if not path.is_file():
        return None
    by_process = defaultdict(lambda: ([], []))
    for line in path.read_text(encoding="utf-8-sig").splitlines():
        if line.startswith(BROWSER_PREFIX):
            row = json.loads(line[len(BROWSER_PREFIX):])
            if row.get("schema") == 1:
                by_process[row["pid"]][0].append(row)
        elif line.startswith(WSS_PREFIX):
            row = json.loads(line[len(WSS_PREFIX):])
            if row.get("schema") == 1:
                by_process[row["pid"]][1].append(row)
    result = []
    for browser, worker in by_process.values():
        browser.sort(key=lambda row: row["worker_mono_ns"])
        worker.sort(key=lambda row: row["mono_ns"])
        def relative(unix_ms):
            return (unix_ms * 1_000 - origin_us) / 1_000
        epochs = browser_series(browser, relative, early_window)
        active = [series for series in epochs if series["media_observed"]]
        bracketing = [series for series in active if series["early_bracket"] is not None]
        primary = bracketing[0] if len(bracketing) == 1 else active[0] if len(active) == 1 else None
        first = {}
        if worker:
            tail = worker[-1]
            for snapshot in (tail["worker"], *tail["clients"].values()):
                for key, value in snapshot["first"].items():
                    when = relative(value["unix_ms"])
                    first[key] = min(when, first.get(key, when))
        result.append({
            "browser_snapshot_count": len(browser),
            "browser_totals": primary["totals"] if primary else None,
            "browser_early_bracket": primary["early_bracket"] if primary else None,
            "browser_series": epochs,
            "worker_snapshot_count": len(worker),
            "worker_totals": dict(worker_counts(worker[-1])) if worker else None,
            "worker_first_event_ms": first,
            "worker_connected_minus_bazza_handshake_ms": round(first["connected"] - handshake_ms, 3)
            if handshake_ms is not None and "connected" in first else None,
            "worker_early_bracket": snapshot_window(
                [(relative(row["unix_ms"]), worker_counts(row)) for row in worker], early_window),
        })
    return result


def analyze(scenario_path, run, reference_path=None, reference_origin_us=None):
    scenario, events = read_json(scenario_path), read_events(run / "speakers.jsonl")
    directory = run / "receiver" / "attendee"
    manifest = read_json(directory / "tracks.json")
    timing = manifest["timing"]
    if timing.get("excluded_pause_ms", 0):
        raise ValueError("Short controls cannot align dispatches across recording pauses")
    origin_us = timing["origin_unix_micros"]
    start, = [event for event in events if event["event"] == "scenario_start"]
    offset_ns = start["origin_utc_ns"] - origin_us * 1_000 - start["origin_mono_ns"]
    starts, acks = (by_utterance(events, name) for name in ("audio_post_started", "audio_post_ack"))
    dispatch = {key: (value["mono_ns"] + offset_ns) / 1_000_000 for key, value in starts.items()}
    tracks = named_tracks(manifest, events, directory)
    source = {}
    for clip in scenario["utterances"]:
        key = (clip["participant_id"], clip["wav"])
        if key not in source:
            source[key] = features(wav_under(scenario_path.parent, clip["wav"]))
    reference = features(reference_path) if reference_path else None
    reference_offset_ms = (reference_origin_us - origin_us) / 1_000 if reference is not None else 0

    clips = []
    for clip in scenario["utterances"]:
        key, speaker = clip["id"], clip["participant_id"]
        row = {"id": key, "speaker": speaker, "status": "no_dispatch"}
        clips.append(row)
        if key not in dispatch:
            continue
        when = dispatch[key]
        finish = when + clip["duration_ms"]
        later = [dispatch[other["id"]] for other in scenario["utterances"]
                 if other["participant_id"] == speaker and other["id"] in dispatch and dispatch[other["id"]] > when]
        last_unambiguous = min(later, default=float("inf")) - clip["duration_ms"]
        last = min(when + DELAY_MS, last_unambiguous)
        first = when - LEAD_MS
        expected = source[(speaker, clip["wav"])]
        alternatives = [value for (owner, _), value in source.items() if owner != speaker]
        matches = []
        late_named = []
        for track, owner, signal in tracks:
            evidence = candidate(expected, alternatives, signal, first, last)
            if evidence is not None:
                matches.append({"file": track["file"], "named_speaker": owner, **evidence})
            if owner == speaker and (coverage := track.get("coverage")):
                first_pcm_ms = coverage["first_pcm_sample"] * 1_000 / manifest["sample_rate"]
                if last < first_pcm_ms <= last_unambiguous:
                    # Search near the first late samples without reassigning the next same-speaker cue.
                    next_frame_ms = (last // HOP_MS + 1) * HOP_MS
                    late = candidate(expected, alternatives, signal, max(next_frame_ms, first_pcm_ms - 2 * HOP_MS),
                                     min(last_unambiguous, first_pcm_ms + DELAY_MS))
                    if late is not None and late["distinct"]:
                        late_named.append({"file": track["file"], "named_speaker": owner, **late,
                                           "start_after_dispatch_ms": round(late["start_ms"] - when, 3)})
        matches.sort(key=lambda value: value["quality"], reverse=True)
        named = [item for item in matches if item["named_speaker"] == speaker and item["distinct"]]
        other = [item for item in matches if item["named_speaker"] != speaker and item["distinct"]]
        if named:
            best = named[0]
            status = "matched_named" if best["matched_sections"] == best["active_sections"] else "partial_named"
        else:
            status = "matched_elsewhere" if other else "unresolved"
        first_pcm = [track["coverage"]["first_pcm_sample"] * 1_000 / manifest["sample_rate"]
                     for track, owner, _ in tracks if owner == speaker and track.get("coverage")]
        first_observed = [track["coverage"]["first_observed_ms"] for track, owner, _ in tracks
                          if owner == speaker and track.get("coverage") and "first_observed_ms" in track["coverage"]]
        row.update(status=status, dispatch_ms=round(when, 3), dispatch_plus_duration_ms=round(finish, 3),
                   nominal_clip_before_named_pcm=finish < min(first_pcm) if first_pcm else None,
                   named_first_pcm_ms=min(first_pcm, default=None),
                   named_first_observed_ms=min(first_observed, default=None),
                   ack_ms=round((acks[key]["mono_ns"] - starts[key]["mono_ns"]) / 1_000_000, 3) if key in acks else None,
                   tracks=matches)
        if late_named:
            row["late_named_tracks"] = sorted(late_named, key=lambda value: value["quality"], reverse=True)
        if reference is not None:
            found = candidate(expected, alternatives, reference, first - reference_offset_ms, last - reference_offset_ms)
            if found is not None:
                found["start_ms"] = round(found["start_ms"] + reference_offset_ms, 3)
            row["independent_reference"] = found

    early = [row for row in clips if row["id"].startswith("early-")]
    early_dispatches = [row for row in early if "dispatch_ms" in row]
    later = [row["dispatch_ms"] for row in clips if not row["id"].startswith("early-") and "dispatch_ms" in row]
    window = [min(row["dispatch_ms"] for row in early_dispatches) - LEAD_MS,
              min(max(row["dispatch_plus_duration_ms"] for row in early_dispatches) + DELAY_MS,
                  min(later, default=float("inf")))] if early_dispatches else None
    ingress_path = run / "receiver" / "ingress.jsonl"
    ingress = ingress_summary(read_events(ingress_path), origin_us, window) if ingress_path.is_file() else None
    handshake_ms = ingress["first_event_ms"].get("wss_handshake_accepted") if ingress is not None else None
    return {
        "clock": "milliseconds from the WAV capture origin",
        "sender_scenario_start_ms": round((start["origin_utc_ns"] - origin_us * 1_000) / 1_000_000, 3),
        "early_window_ms": window,
        "early_cue_status": dict(Counter(row["status"] for row in early)),
        "cue_status": dict(Counter(row["status"] for row in clips)),
        "manifest_queue_drops": timing["queue_dropped_packets"],
        "manifest_provider_reanchors": timing["provider_clock_reanchors"],
        "ingress": ingress,
        "provider": provider_summary(run / "provider-audit.log", origin_us, window, handshake_ms),
        "cues": clips,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, epilog=
        "Matches are spectral estimates. Dispatch is not audible playout. Provider counter brackets use VM wall time and can include activity immediately outside the window.")
    parser.add_argument("scenario", type=Path)
    parser.add_argument("run_directory", type=Path)
    parser.add_argument("--reference-wav", type=Path)
    parser.add_argument("--reference-origin-us", type=int)
    args = parser.parse_args()
    if (args.reference_wav is None) != (args.reference_origin_us is None):
        parser.error("--reference-wav and --reference-origin-us must be supplied together")
    print(json.dumps(analyze(args.scenario, args.run_directory, args.reference_wav, args.reference_origin_us), indent=2))


if __name__ == "__main__":
    main()
