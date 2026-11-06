import copy
import unittest

from measure import score
from media_evidence import RATE, locate_phrase

try:
    import numpy as np
except ImportError:
    np = None


class AcceptanceScoreTest(unittest.TestCase):
    def setUp(self):
        self.manifest = {
            "schema_version": 1,
            "duration_ms": 30_000,
            "participants": [{"id": "one", "name": "Ольга"}, {"id": "two", "name": "Семён"}],
            "utterances": [
                {"id": "a", "participant_id": "one", "text": "Поставьте окно справа"},
                {"id": "b", "participant_id": "two", "text": "Смета выросла вдвое"},
                {"id": "c", "participant_id": "one", "text": "Обсудим срок после обеда"},
            ],
        }
        self.evidence = {
            "markers": [
                {"id": "av1", "audio_ms": 110, "video_ms": 100},
                {"id": "av2", "audio_ms": 610, "video_ms": 600},
                {"id": "av3", "audio_ms": 910, "video_ms": 900},
            ],
            "phrases": [
                {"id": "a", "start_ms": 1000, "end_ms": 1400},
                {"id": "b", "start_ms": 1100, "end_ms": 1600},
                {"id": "c", "start_ms": 2600, "end_ms": 2900},
            ],
            "seeks": [{"id": "a", "seek_ms": 1040}, {"id": "b", "seek_ms": 1110}, {"id": "c", "seek_ms": 2650}],
        }
        self.transcript = {
            "timing": "recording",
            "segments": [
                {"start_ms": 1020, "end_ms": 1410, "speaker_id": "11", "text": "Поставьте окно справа"},
                {"start_ms": 1150, "end_ms": 1640, "speaker_id": "22", "text": "Смета выросла вдвое"},
                {"start_ms": 2610, "end_ms": 2700, "speaker_id": "11", "text": "Обсудим срок"},
                {"start_ms": 2720, "end_ms": 2960, "speaker_id": "11", "text": "после обеда"},
            ],
        }
        self.speaker_map = {"11": "one", "22": "two"}

    def run_score(self):
        return score("meet", self.manifest, self.evidence, self.transcript, self.speaker_map)

    def test_scores_overlapping_speakers_and_split_phrase(self):
        result = self.run_score()
        self.assertEqual(result["result"], "pass")
        self.assertEqual(result["gates"]["phrase_start"]["p95_absolute_ms"], 50)
        self.assertEqual(result["speech"]["word_error_rate"], 0)
        self.assertEqual(result["utterances"][2]["segment_indices"], (2, 3))

    def test_without_independent_playback_observation_cannot_pass(self):
        self.evidence.pop("phrases")
        result = self.run_score()
        self.assertEqual(result["result"], "inconclusive")
        self.assertEqual(result["gates"]["phrase_start"]["count"], 0)

    def test_relative_bot_timestamps_and_missing_seeks_cannot_pass(self):
        self.transcript["timing"] = "relative"
        self.evidence.pop("seeks")
        result = self.run_score()
        self.assertEqual(result["result"], "inconclusive")
        self.assertEqual(result["gates"]["phrase_start"]["status"], "inconclusive")
        self.assertEqual(result["gates"]["playback_seek"]["status"], "inconclusive")

    def test_scores_recording_alignment_and_preserves_bot_relative_times(self):
        self.transcript["timing"] = "relative"
        for row in self.transcript["segments"]:
            row["playback_start_ms"] = row["start_ms"]
            row["playback_end_ms"] = row["end_ms"]
            row["start_ms"] += 1200
            row["end_ms"] += 1200
        result = self.run_score()
        self.assertEqual(result["result"], "pass")
        self.assertEqual(result["gates"]["phrase_start"]["p95_absolute_ms"], 50)

        # Losing the second half of a split phrase must not look complete.
        self.transcript["segments"][-1]["playback_start_ms"] = None
        result = self.run_score()
        self.assertEqual(result["result"], "inconclusive")
        self.assertEqual(result["gates"]["playback_mapping"]["unaligned_segment_indices"], [3])

    def test_uncertain_audio_onset_cannot_hide_borderline_timestamp_error(self):
        self.evidence["phrases"][0].update(start_low_ms=670, start_high_ms=1020)
        result = self.run_score()
        self.assertEqual(result["gates"]["phrase_start"]["status"], "inconclusive")
        self.assertEqual(result["gates"]["phrase_start"]["p95_absolute_interval_ms"], [50, 350])

    def test_invalid_measurement_is_rejected(self):
        self.evidence["phrases"][0]["start_ms"] = float("nan")
        with self.assertRaisesRegex(ValueError, "invalid nonnegative timestamp"):
            self.run_score()

    def test_offset_and_wrong_speaker_fail_without_refitting(self):
        for marker in self.evidence["markers"]:
            marker["audio_ms"] += 300
        self.transcript["segments"][1].update(speaker_id="11", start_ms=1580, end_ms=1700)
        result = self.run_score()
        self.assertEqual(result["result"], "fail")
        self.assertEqual(result["gates"]["av_offset"]["p95_absolute_ms"], 310)
        self.assertEqual(result["gates"]["phrase_start"]["p95_absolute_ms"], 480)
        self.assertEqual(result["gates"]["speaker_attribution"]["incorrect"], 1)

    def test_duplicate_does_not_pass(self):
        duplicate = copy.deepcopy(self.transcript["segments"][0])
        duplicate.update(start_ms=1510, end_ms=1710)
        self.transcript["segments"].append(duplicate)
        self.assertEqual(self.run_score()["gates"]["track_coverage"]["status"], "fail")

    def test_drift_uses_early_and_late_windows_and_requires_a_full_span(self):
        self.manifest["duration_ms"] = 1_800_000
        self.evidence["markers"] = [
            {"id": str(i), "audio_ms": time + (70 if time > 1_000_000 else 0), "video_ms": time}
            for i, time in enumerate([1000, 4000, 8000, 895000, 900000, 905000, 1786000, 1790000, 1794000])
        ]
        self.assertEqual(self.run_score()["gates"]["av_drift"]["status"], "fail")
        self.evidence["markers"] = self.evidence["markers"][:6]
        self.assertEqual(self.run_score()["gates"]["av_drift"]["status"], "inconclusive")


@unittest.skipIf(np is None, "NumPy is needed for waveform evidence")
class AudioEvidenceTest(unittest.TestCase):
    def test_multiple_clip_sections_can_recover_onset_under_overlap(self):
        random = np.random.default_rng(42)
        waveform = random.standard_normal(3 * RATE)
        recorded = random.standard_normal(5 * RATE) * 2
        start = 5731
        recorded[start : start + len(waveform)] += waveform
        measured, _, method = locate_phrase(recorded, waveform, np)
        self.assertEqual(measured, start)
        self.assertEqual(method, "matched_waveform_parts")


if __name__ == "__main__":
    unittest.main()
