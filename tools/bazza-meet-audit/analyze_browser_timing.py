"""Compare sender Web Audio clocks and receiver RTP counters in a short audit run."""

import argparse
from collections import defaultdict
import json
from pathlib import Path


RTP_METRICS = (
    "totalSamplesReceived",
    "insertedSamplesForDeceleration",
    "removedSamplesForAcceleration",
    "concealedSamples",
    "jitterBufferEmittedCount",
)


def performance_at_output(record, context_ms):
    if record.get("outputPerfMs", 0) > 0 and record.get("outputContextMs", 0) > 0:
        return record["outputPerfMs"] + context_ms - record["outputContextMs"]
    return None


def summarize_sender(rows):
    ended = {row["sequence"]: row for row in rows if row["stage"] == "ended"}
    scheduled = sorted((row for row in rows if row["stage"] == "scheduled"), key=lambda row: row["sequence"])
    chunks = []
    for row in scheduled:
        chunks.append(row)
        # The short fixtures decode into 1 s buffers and one shorter final buffer.
        if row["bufferMs"] >= 999.9:
            continue
        first, last = chunks[0], chunks[-1]
        last_ended = ended.get(last["sequence"])
        start = performance_at_output(first, first["startMs"])
        end = performance_at_output(last_ended, last["endMs"]) if last_ended else None
        resource = sum(chunk["bufferMs"] for chunk in chunks)
        span = last["endMs"] - first["startMs"]
        yield {
            "first_sequence": first["sequence"],
            "chunk_count": len(chunks),
            "resource_ms": round(resource, 3),
            "context_span_ms": round(span, 3),
            "context_gap_ms": round(span - resource, 3),
            "output_clock_span_ms": round(end - start, 3) if start is not None and end is not None else None,
            "output_start_unix_ms": round(first["browserEpochMs"] + start, 3) if start is not None else None,
            "buffer_rates_hz": sorted({chunk["rateHz"] for chunk in chunks}),
            "context_rates_hz": sorted({chunk["contextRateHz"] for chunk in chunks}),
            "playback_rates": sorted({chunk["playbackRate"] for chunk in chunks}),
        }
        chunks = []


def summarize_rtp(rows):
    rows = sorted((row for row in rows if row["stage"] == "inbound-rtp"), key=lambda row: row["perfMs"])
    totals = dict.fromkeys(RTP_METRICS, 0)
    stable_pairs = 0
    for earlier, later in zip(rows, rows[1:]):
        count = earlier["inboundAudioReports"]
        if count <= 0 or count != later["inboundAudioReports"] or earlier["receivers"] != later["receivers"]:
            continue
        deltas = {
            key: later[key] - earlier[key]
            for key in RTP_METRICS
            if earlier.get(key + "Reports") == later.get(key + "Reports") == count
            and key in earlier and key in later
        }
        if len(deltas) != len(RTP_METRICS) or min(deltas.values()) < 0:
            continue
        stable_pairs += 1
        for key, delta in deltas.items():
            totals[key] += delta
    emitted = totals["jitterBufferEmittedCount"]
    return {"stable_pairs": stable_pairs, **totals,
            "inserted_share_of_emitted": round(totals["insertedSamplesForDeceleration"] / emitted, 4) if emitted else None}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("timing_log", type=Path)
    args = parser.parse_args()
    epochs = defaultdict(list)
    for line in args.timing_log.read_text(encoding="utf-8").splitlines():
        row = json.loads(line)
        epochs[row["browserEpochMs"]].append(row)
    senders = [list(summarize_sender(rows)) for rows in epochs.values() if any(row["stage"] == "scheduled" for row in rows)]
    receivers = [summarize_rtp(rows) for rows in epochs.values() if any(row["stage"] == "inbound-rtp" for row in rows)]
    print(json.dumps({"sender_browser_cues": senders, "receiver_browser_rtp": receivers}, indent=2))


if __name__ == "__main__":
    main()
