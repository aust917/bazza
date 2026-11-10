"""Compare a Meet probe's dispatch log with the recorder's first PCM timestamps."""

import argparse
import json
from pathlib import Path
import statistics


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def read_events(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8-sig").splitlines() if line.strip()]


def by_utterance(events, name):
    result = {}
    for event in events:
        if event["event"] != name:
            continue
        utterance_id = event["utterance_id"]
        if utterance_id in result:
            raise ValueError(f"Duplicate {name} for {utterance_id}")
        result[utterance_id] = event
    return result


def audit(scenario, events, manifest, mix_clock, diagnostics=None):
    start, = [event for event in events if event["event"] == "scenario_start"]
    timing = manifest["timing"]
    origin_us = timing["origin_unix_micros"]
    sample_rate = manifest["sample_rate"]
    scenario_offset_ms = (start["origin_utc_ns"] - origin_us * 1000) / 1_000_000
    starts = by_utterance(events, "audio_post_started")
    acks = by_utterance(events, "audio_post_ack")
    speaker_by_name = {}
    for event in events:
        if event["event"] == "join_requested":
            name, speaker = event["bot_name"], event["participant_id"]
            if speaker_by_name.setdefault(name, speaker) != speaker:
                raise ValueError("One bot display name was reused by different speakers")
    tracks_by_speaker = {}
    for track in manifest["tracks"]:
        speaker = speaker_by_name.get(track["display_name"])
        if speaker is not None:
            tracks_by_speaker.setdefault(speaker, []).append(track)

    spectral = (diagnostics or {}).get("local_spectral_presence_diagnostic", {})
    early, without_tracks, missing_dispatch = [], [], []
    for clip in scenario["utterances"]:
        clip_id, speaker = clip["id"], clip["participant_id"]
        dispatch = starts.get(clip_id)
        if dispatch is None:
            missing_dispatch.append(clip_id)
            continue
        tracks = tracks_by_speaker.get(speaker)
        if not tracks:
            without_tracks.append(clip_id)
            continue
        coverage = [track["coverage"] for track in tracks]
        first_observation = min(item["first_observed_ms"] for item in coverage)
        first_pcm = min(item["first_pcm_sample"] for item in coverage) * 1000 / sample_rate
        sent_at = scenario_offset_ms + (dispatch["mono_ns"] - start["origin_mono_ns"]) / 1_000_000
        dispatch_plus_duration = sent_at + clip["duration_ms"]
        if dispatch_plus_duration >= min(first_observation, first_pcm):
            continue
        local = spectral.get(clip_id)
        early.append({
            "id": clip_id,
            "speaker": speaker,
            "dispatch_plus_duration_ms": round(dispatch_plus_duration, 3),
            "first_observed_ms": first_observation,
            "first_pcm_ms": first_pcm,
            "gap_to_first_observation_ms": round(first_observation - dispatch_plus_duration, 3),
            "local_spectral_good_subwindows": local["good_subwindows"] if local is not None else None,
        })

    ack_ms = [(acks[key]["mono_ns"] - event["mono_ns"]) / 1_000_000
              for key, event in starts.items() if key in acks]
    return {
        "scenario_offset_ms": round(scenario_offset_ms, 3),
        "mix_origin_minus_attendee_origin_us": mix_clock["start_unix_us"] - origin_us,
        "first_recorder_observed_pcm_ms": min((t["coverage"]["first_observed_ms"]
                                              for t in manifest["tracks"]), default=None),
        "planned_clips": len(scenario["utterances"]),
        "dispatched_clips": len(starts),
        "acknowledged_clips": len(ack_ms),
        "ack_latency_ms": [round(value, 3) for value in
                           (min(ack_ms), statistics.median(ack_ms), max(ack_ms))] if ack_ms else None,
        "attendee_tracks": len(manifest["tracks"]),
        "mapped_speakers": len(tracks_by_speaker),
        "queue_dropped_packets": timing["queue_dropped_packets"],
        "provider_clock_reanchors": timing["provider_clock_reanchors"],
        "clips_ending_before_first_pcm": early,
        "early_clips_with_local_spectral_evidence": sum(
            bool(clip["local_spectral_good_subwindows"]) for clip in early
        ) if diagnostics is not None else None,
        "clips_without_mapped_track": without_tracks,
        "clips_without_dispatch": missing_dispatch,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", type=Path)
    parser.add_argument("speaker_journal", type=Path)
    parser.add_argument("recording", type=Path)
    parser.add_argument("--diagnostics", type=Path, help="existing local spectral measurement JSON")
    args = parser.parse_args()
    result = audit(
        read_json(args.scenario), read_events(args.speaker_journal),
        read_json(args.recording / "attendee" / "tracks.json"),
        read_json(args.recording / "mix.clock.json"),
        read_json(args.diagnostics) if args.diagnostics else None,
    )
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
