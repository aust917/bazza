"""Prepare short, distinguishable speech cues from the existing synthetic corpus."""

import argparse
import json
from pathlib import Path
import wave


CUE_MS = 2_400
EARLY_MS = (300, 3_700)
REPEAT_MS = (8_000, 11_400)
OVERLAP_MS = (16_000, 16_350)


def excerpt(source, target, speech_start_ms):
    with wave.open(str(source), "rb") as incoming:
        rate = incoming.getframerate()
        start = max(0, (speech_start_ms - 50) * rate // 1000)
        frames = CUE_MS * rate // 1000
        incoming.setpos(start)
        samples = incoming.readframes(frames)
        if len(samples) != frames * incoming.getsampwidth() * incoming.getnchannels():
            raise ValueError(f"Short cue source: {source.name}")
        with wave.open(str(target), "wb") as outgoing:
            outgoing.setparams(incoming.getparams())
            outgoing.writeframes(samples)


def prepare(source_path, target, speaker_count):
    source_path = source_path.resolve()
    source = json.loads(source_path.read_text(encoding="utf-8-sig"))
    fixtures = {}
    for utterance in source["utterances"]:
        if utterance["speech_end_ms"] - utterance["speech_start_ms"] >= CUE_MS:
            fixtures.setdefault(utterance["participant_id"], utterance)
    participants = [row for row in source["participants"] if row["id"] in fixtures][:speaker_count]
    if len(participants) != speaker_count:
        raise ValueError("Insufficient synthetic speakers in the source scenario")

    target.mkdir(exist_ok=False)
    paths = {}
    for row in participants:
        speaker = row["id"]
        utterance = fixtures[speaker]
        source_wav = (source_path.parent / utterance["wav"]).resolve()
        if not source_wav.is_relative_to(source_path.parent) or source_wav.suffix.lower() != ".wav":
            raise ValueError("Cue must be a WAV under the synthetic scenario directory")
        name = f"cue-{len(paths) + 1:02}.wav"
        excerpt(source_wav, target / name, utterance["speech_start_ms"])
        paths[speaker] = name

    schedule = []
    for stage, starts in (("early", EARLY_MS), ("repeat", REPEAT_MS), ("overlap", OVERLAP_MS)):
        for row, at in zip(participants[:2], starts, strict=True):
            schedule.append((row["id"], stage, at))
    for index, row in enumerate(participants[2:]):
        schedule.append((row["id"], "roster", 21_000 + index * 3_100))

    payload = {
        "schema_version": 1,
        "duration_ms": max(at for _, _, at in schedule) + CUE_MS + 3_000,
        "participants": participants,
        "events": [{"at_ms": 0, "participant_id": row["id"], "action": "join"} for row in participants],
        "utterances": [
            {"id": f"{stage}-{speaker}", "participant_id": speaker, "at_ms": at, "duration_ms": CUE_MS,
             "speech_start_ms": 0, "speech_end_ms": CUE_MS, "wav": paths[speaker], "text": "Synthetic excerpt"}
            for speaker, stage, at in schedule
        ],
    }
    (target / "scenario.json").write_text(json.dumps(payload, indent=2, ensure_ascii=False), encoding="utf-8")
    return target / "scenario.json"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source_scenario", type=Path)
    parser.add_argument("fresh_output_dir", type=Path)
    parser.add_argument("--speakers", type=int, choices=(2, 8), default=2)
    args = parser.parse_args()
    print(prepare(args.source_scenario, args.fresh_output_dir, args.speakers))


if __name__ == "__main__":
    main()
