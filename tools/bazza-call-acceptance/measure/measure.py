"""Score one Bazza call against measured media and a scripted conversation."""

import argparse
from collections import Counter, defaultdict
from difflib import SequenceMatcher
import json
import math
from pathlib import Path
import re
from statistics import median


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def write_json(path, value):
    report = json.dumps(value, ensure_ascii=False, indent=2) + "\n"
    if path:
        path.write_text(report, encoding="utf-8")
    else:
        print(report, end="")


def index(rows, key="id"):
    values = {row[key]: row for row in rows}
    if len(values) != len(rows):
        raise ValueError(f"duplicate {key} in evidence")
    return values


def checked_ms(value):
    if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
        raise ValueError(f"invalid nonnegative timestamp or duration {value!r}")
    return value


def words(text):
    return re.findall(r"[^\W_]+", text.casefold(), flags=re.UNICODE)


def edits(reference, actual):
    previous = list(range(len(actual) + 1))
    for i, word in enumerate(reference, 1):
        current = [i]
        for j, other in enumerate(actual, 1):
            current.append(min(current[-1] + 1, previous[j] + 1, previous[j - 1] + (word != other)))
        previous = current
    return previous[-1]


def percentile95(values):
    if values:
        ordered = sorted(values)
        return round(ordered[math.ceil(0.95 * len(ordered)) - 1], 3)
    return None


def metric(values, limit, *, complete=True, bounds=None):
    p95 = percentile95([abs(value) for value in values])
    if bounds:
        lower = percentile95([max(0, low, -high) for low, high in bounds])
        upper = percentile95([max(abs(low), abs(high)) for low, high in bounds])
    else:
        lower = upper = p95
    if lower is not None and lower > limit:
        status = "fail"
    elif upper is None or upper > limit or not complete:
        status = "inconclusive"
    else:
        status = "pass"
    report = {"status": status, "count": len(values), "p95_absolute_ms": p95, "limit_ms": limit}
    if bounds:
        report["p95_absolute_interval_ms"] = [lower, upper]
    return report


def score_av(evidence, duration_ms):
    markers = sorted(index(evidence.get("markers", [])).values(), key=lambda item: checked_ms(item["video_ms"]))
    samples = []
    for marker in markers:
        delta = checked_ms(marker["audio_ms"]) - marker["video_ms"]
        previous = checked_ms(marker.get("video_previous_ms", marker["video_ms"]))
        uncertainty = checked_ms(marker.get("audio_uncertainty_ms", 0))
        if previous > marker["video_ms"]:
            raise ValueError(f"invalid uncertainty for marker {marker['id']}")
        samples.append(
            {
                "id": marker["id"],
                "video_ms": marker["video_ms"],
                "delta_ms": round(delta, 3),
                "delta_low_ms": round(delta - uncertainty, 3),
                "delta_high_ms": round(marker["audio_ms"] - previous + uncertainty, 3),
            }
        )
    complete = len(samples) >= 3 and not evidence.get("marker_issues")
    offset = metric(
        [sample["delta_ms"] for sample in samples], 80, complete=complete,
        bounds=[(sample["delta_low_ms"], sample["delta_high_ms"]) for sample in samples],
    )
    offset["samples"] = samples
    offset["issues"] = evidence.get("marker_issues", [])
    if not complete:
        offset["status"] = "inconclusive"

    drift = {"status": "not_required", "limit_ms_per_30min": 50}
    if duration_ms < 1_800_000:
        return offset, drift
    drift["status"] = "inconclusive"
    if not complete or len(samples) < 9:
        return offset, drift

    early = [sample for sample in samples if sample["video_ms"] <= samples[0]["video_ms"] + 30_000]
    late = [sample for sample in samples if sample["video_ms"] >= samples[-1]["video_ms"] - 30_000]
    span = median(s["video_ms"] for s in late) - median(s["video_ms"] for s in early)
    if len(early) < 3 or len(late) < 3 or span < 1_740_000:
        drift["span_ms"] = span
        return offset, drift

    def group_interval(group):
        low = max(s["delta_low_ms"] for s in group)
        high = min(s["delta_high_ms"] for s in group)
        if low <= high:
            return low, high
        # When the window is not consistent with a fixed offset, keep the
        # wider bounds of the window median rather than hiding that variation.
        return median(s["delta_low_ms"] for s in group), median(s["delta_high_ms"] for s in group)

    early_low, early_high = group_interval(early)
    late_low, late_high = group_interval(late)
    scale = 1_800_000 / span
    low = (late_low - early_high) * scale
    high = (late_high - early_low) * scale
    if max(abs(low), abs(high)) <= 50:
        status = "pass"
    elif low > 50 or high < -50:
        status = "fail"
    else:
        status = "inconclusive"
    drift.update(
        status=status,
        span_ms=span,
        early_count=len(early),
        late_count=len(late),
        interval_ms_per_30min=[round(low, 3), round(high, 3)],
    )
    return offset, drift


def assign_segments(utterances, segments, speaker_ids, assignments):
    by_speaker = defaultdict(list)
    for i, segment in enumerate(segments):
        by_speaker[speaker_ids[i]].append(i)
    candidates = []
    for ids in by_speaker.values():
        ids.sort(key=lambda i: segments[i]["start_ms"])
        for position in range(len(ids)):
            for size in range(1, min(8, len(ids) - position) + 1):
                group = ids[position : position + size]
                if any(
                    segments[right]["start_ms"] - segments[left]["end_ms"] > 2_000
                    for left, right in zip(group, group[1:])
                ):
                    break
                content = words(" ".join(segments[i]["text"] for i in group))
                if content:
                    candidates.append((tuple(group), content, set(content)))

    assigned = {}
    used = set()
    for row in assignments:
        phrase_id, indices = row["id"], row["segment_indices"]
        if phrase_id not in utterances or phrase_id in assigned or not indices:
            raise ValueError(f"invalid assignment for {phrase_id}")
        if any(type(i) is not int or not 0 <= i < len(segments) or i in used for i in indices):
            raise ValueError(f"invalid or reused segment index for {phrase_id}")
        if len(set(indices)) != len(indices):
            raise ValueError(f"duplicate segment index for {phrase_id}")
        assigned[phrase_id] = indices, "audited"
        used.update(indices)

    choices = []
    for phrase_id, phrase in utterances.items():
        if phrase_id in assigned:
            continue
        reference = words(phrase["text"])
        vocabulary = set(reference)
        options = []
        for indices, content, vocabulary_actual in candidates:
            if used.intersection(indices) or not vocabulary.intersection(vocabulary_actual):
                continue
            if min(len(reference), len(content)) / max(len(reference), len(content)) < 0.7:
                continue
            confidence = SequenceMatcher(None, reference, content, autojunk=False).ratio()
            if confidence >= 0.7:
                options.append((confidence, indices))
        options.sort(reverse=True)
        if not options:
            continue
        confidence, indices = options[0]
        if len(options) > 1 and confidence - options[1][0] < 0.08:
            continue
        choices.append((confidence, phrase_id, indices))
    # Competing phrases are not assigned by timestamp. That would hide offsets.
    counts = Counter(i for _, _, ids in choices for i in ids)
    for _, phrase_id, indices in choices:
        if all(counts[i] == 1 for i in indices):
            assigned[phrase_id] = indices, "text_match"
    return assigned


def score(provider, manifest, evidence, transcript, mapping):
    if manifest.get("schema_version") != 1:
        raise ValueError("expected corpus schema_version 1")
    participants = index(manifest["participants"])
    utterances = index(manifest["utterances"])
    segments = transcript["segments"]
    playback = index(evidence.get("phrases", []))
    seeks = index(evidence.get("seeks", []))
    expected_absent = set(evidence.get("expected_bot_absent_ids", []))
    for row_id in set(playback) | set(seeks) | expected_absent:
        if row_id not in utterances:
            raise ValueError(f"unknown utterance {row_id}")
    for phrase in utterances.values():
        if phrase["participant_id"] not in participants or not words(phrase["text"]):
            raise ValueError(f"invalid corpus utterance {phrase['id']}")
    for segment in segments:
        if checked_ms(segment["end_ms"]) < checked_ms(segment["start_ms"]):
            raise ValueError("transcript contains an inverted timestamp range")
        mapped_start = segment.get("playback_start_ms")
        mapped_end = segment.get("playback_end_ms")
        if mapped_start is not None:
            checked_ms(mapped_start)
        if mapped_end is not None:
            checked_ms(mapped_end)
        if mapped_start is not None and mapped_end is not None and mapped_end < mapped_start:
            raise ValueError("transcript contains an inverted playback timestamp range")
    for row in playback.values():
        start = checked_ms(row["start_ms"])
        if "end_ms" in row and checked_ms(row["end_ms"]) < start:
            raise ValueError(f"playback contains an inverted timestamp range for {row['id']}")
        if not checked_ms(row.get("start_low_ms", start)) <= start <= checked_ms(row.get("start_high_ms", start)):
            raise ValueError(f"playback contains an invalid onset estimate for {row['id']}")
    for seek in seeks.values():
        checked_ms(seek["seek_ms"])

    unique_names = Counter(participant["name"] for participant in participants.values())
    by_name = {
        participant["name"]: pid
        for pid, participant in participants.items()
        if unique_names[participant["name"]] == 1
    }
    if any(pid not in participants for pid in mapping.values()):
        raise ValueError("speaker map contains an unknown fixture participant")
    speaker_ids = [
        mapping.get(row.get("speaker_id"), by_name.get(row.get("speaker_name")))
        for row in segments
    ]
    assigned = assign_segments(utterances, segments, speaker_ids, evidence.get("assignments", []))
    bot_relative = transcript.get("timing") in {"approximate", "relative"} or any(
        "playback_start_ms" in row or "playback_end_ms" in row for row in segments
    )

    def playback_bounds(row):
        if bot_relative:
            return row.get("playback_start_ms"), row.get("playback_end_ms")
        return row["start_ms"], row["end_ms"]

    details = []
    starts, ends, seek_errors = [], [], []
    start_bounds, seek_bounds = [], []
    mismatched_speakers = 0
    unknown_speakers = 0
    matched = set()
    unaligned_segments = set()
    expected_words = 0
    word_errors = 0
    for phrase_id, phrase in utterances.items():
        observed = playback.get(phrase_id)
        detail = {"id": phrase_id, "expected_speaker": phrase["participant_id"]}
        if phrase_id not in expected_absent:
            expected_words += len(words(phrase["text"]))
        if phrase_id in assigned:
            indices, method = assigned[phrase_id]
            matched.update(indices)
            actual = [segments[i] for i in indices]
            if phrase_id not in expected_absent:
                unaligned_segments.update(i for i in indices if None in playback_bounds(segments[i]))
                word_errors += edits(words(phrase["text"]), words(" ".join(row["text"] for row in actual)))
            actual_speakers = {speaker_ids[i] for i in indices}
            detail.update(segment_indices=indices, method=method)
            if actual_speakers == {phrase["participant_id"]}:
                detail["speaker"] = "correct"
            elif actual_speakers - {None, phrase["participant_id"]}:
                detail["speaker"] = "incorrect"
                mismatched_speakers += 1
            else:
                detail["speaker"] = "unknown"
                unknown_speakers += 1
            if observed is not None and phrase_id not in expected_absent:
                first = min(actual, key=lambda row: row["start_ms"])
                last = max(actual, key=lambda row: row["end_ms"])
                actual_start, first_end = playback_bounds(first)
                last_start, actual_end = playback_bounds(last)
                onset_low = observed.get("start_low_ms", observed["start_ms"])
                onset_high = observed.get("start_high_ms", observed["start_ms"])
                if actual_start is not None and first_end is not None:
                    start = actual_start - observed["start_ms"]
                    start_bounds.append((actual_start - onset_high, actual_start - onset_low))
                    detail["start_error_ms"] = round(start, 3)
                    detail["start_error_interval_ms"] = [round(bound, 3) for bound in start_bounds[-1]]
                    starts.append(start)
                if "end_ms" in observed and last_start is not None and actual_end is not None:
                    end = actual_end - observed["end_ms"]
                    detail["end_error_ms"] = round(end, 3)
                    ends.append(end)
                if phrase_id in seeks:
                    actual_seek = seeks[phrase_id]["seek_ms"]
                    seek = actual_seek - observed["start_ms"]
                    seek_bounds.append((actual_seek - onset_high, actual_seek - onset_low))
                    detail["seek_error_ms"] = round(seek, 3)
                    seek_errors.append(seek)
        elif phrase_id not in expected_absent:
            word_errors += len(words(phrase["text"]))
            detail["method"] = "unmatched"
        if phrase_id in expected_absent:
            detail["expected_bot_absent"] = True
        details.append(detail)

    unmatched = [i for i in range(len(segments)) if i not in matched]
    word_errors += sum(len(words(segments[i]["text"])) for i in unmatched)
    # Report exact replays separately from all other unassigned material.
    seen_text = {
        (speaker_ids[i], tuple(words(segments[i]["text"])))
        for i in matched
    }
    by_spoken_text = defaultdict(list)
    for i, segment in enumerate(segments):
        by_spoken_text[speaker_ids[i], tuple(words(segment["text"]))].append(i)
    expected_text = Counter(
        (phrase["participant_id"], tuple(words(phrase["text"])))
        for phrase in utterances.values()
    )
    duplicate_groups = [
        indices for key, indices in by_spoken_text.items()
        if expected_text[key] and len(indices) > expected_text[key]
    ]
    duplicates = [
        i for i in unmatched
        if (speaker_ids[i], tuple(words(segments[i]["text"]))) in seen_text
    ]
    target_count = len(utterances) - len(expected_absent)
    unexpected_presence = sorted(expected_absent & assigned.keys())
    av_offset, av_drift = score_av(evidence, checked_ms(manifest["duration_ms"]))
    timing = metric(
        starts, 300, complete=target_count > 0 and len(starts) == target_count, bounds=start_bounds,
    )
    gates = {
        "av_offset": av_offset,
        "av_drift": av_drift,
        "phrase_start": timing,
        "playback_seek": metric(
            seek_errors, 300, complete=target_count > 0 and len(seek_errors) >= min(target_count, 3),
            bounds=seek_bounds,
        ),
        "speaker_attribution": {
            "status": (
                "fail"
                if mismatched_speakers
                else "inconclusive"
                if unknown_speakers or target_count == 0 or len(assigned) - len(unexpected_presence) < target_count
                else "pass"
            ),
            "incorrect": mismatched_speakers,
            "unknown": unknown_speakers,
        },
        "track_coverage": {
            "status": (
                "fail" if duplicate_groups or unexpected_presence
                else "inconclusive" if target_count == 0 or len(assigned) < target_count
                else "pass"
            ),
        },
        "playback_mapping": {
            "status": "inconclusive" if unaligned_segments or not matched else "pass",
            "unaligned_segment_indices": sorted(unaligned_segments),
        },
    }
    statuses = {gate["status"] for gate in gates.values()}
    result = "fail" if "fail" in statuses else "inconclusive" if "inconclusive" in statuses else "pass"
    return {
        "provider": provider,
        "result": result,
        "gates": gates,
        "coverage": {
            "expected_utterances": target_count,
            "matched_utterances": len(assigned) - len(unexpected_presence),
            "measured_playback_starts": len(set(playback) - expected_absent),
            "unexpected_presence": unexpected_presence,
            "unassigned_segment_indices": unmatched,
            "duplicate_segment_indices": duplicates,
            "repeated_control_phrase_segment_groups": duplicate_groups,
        },
        "speech": {
            "reference_words": expected_words,
            "word_errors": word_errors,
            "word_error_rate": round(word_errors / expected_words, 5) if expected_words else None,
            "phrase_end_p95_absolute_ms": percentile95([abs(error) for error in ends]),
            "phrase_end_count": len(ends),
        },
        "utterances": details,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("provider", choices=["meet", "discord"])
    parser.add_argument("manifest", type=Path)
    parser.add_argument("playback_evidence", type=Path)
    parser.add_argument("transcript", type=Path)
    parser.add_argument("--speaker-map", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        mapping = read_json(args.speaker_map) if args.speaker_map else {}
        report = score(
            args.provider,
            read_json(args.manifest),
            read_json(args.playback_evidence),
            read_json(args.transcript),
            mapping,
        )
        write_json(args.output, report)
    except (KeyError, TypeError, ValueError, OSError) as error:
        parser.exit(2, f"Invalid evidence: {error}\n")
    return {"pass": 0, "fail": 1, "inconclusive": 2}[report["result"]]


if __name__ == "__main__":
    raise SystemExit(main())
