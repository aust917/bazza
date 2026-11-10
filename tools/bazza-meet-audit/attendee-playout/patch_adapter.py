"""Stage opt-in timing logs on the generated Attendee audit adapter."""

import argparse
import ast
import hashlib
from pathlib import Path


PINNED = "2d1ad7001ab41288bc2bfda499dfc9d8a46b9aecd8caedf18d94818cdcd292ef"
LOGGER = "logger = logging.getLogger(__name__)\n"
DISPATCH = '                    if json_data_is_dict and json_data.get("type") == "BazzaMeetAuditBrowser":\n'
SUPPORT = '''
_BAZZA_TIMING_CLOCK = ("browserEpochMs", "perfMs")
_BAZZA_TIMING_PLAYOUT_CLOCK = (
    "sequence", "contextMs", "contextRateHz", "contextState", "outputContextMs",
    "outputPerfMs", "baseLatencyMs", "outputLatencyMs",
)
_BAZZA_TIMING_RTP_COUNTERS = (
    "totalSamplesReceived", "insertedSamplesForDeceleration", "removedSamplesForAcceleration",
    "concealedSamples", "jitterBufferEmittedCount",
)
_BAZZA_TIMING_STAGE_FIELDS = {
    "queued": _BAZZA_TIMING_PLAYOUT_CLOCK + (
        "inputSamples", "inputRateHz", "inputChannels", "bufferMs", "queueDepth",
    ),
    "scheduled": _BAZZA_TIMING_PLAYOUT_CLOCK + (
        "schedulerContextMs", "priorEndMs", "lateResetMs", "startMs", "endMs", "bufferMs",
        "frames", "rateHz", "playbackRate", "queueRemaining",
    ),
    "ended": _BAZZA_TIMING_PLAYOUT_CLOCK + ("startMs", "endMs"),
    "inbound-rtp": ("pollDurationMs", "receivers", "receiverErrors", "inboundAudioReports") +
        _BAZZA_TIMING_RTP_COUNTERS + tuple(key + "Reports" for key in _BAZZA_TIMING_RTP_COUNTERS),
}


def _log_bazza_meet_audit_timing(payload):
    if os.environ.get("BAZZA_MEET_AUDIT_TIMING") != "1":
        return
    stage = payload.get("stage")
    if type(stage) is not str or stage not in _BAZZA_TIMING_STAGE_FIELDS:
        return
    record = {"type": "BazzaPlayoutAudit", "stage": stage}
    for key in _BAZZA_TIMING_CLOCK + _BAZZA_TIMING_STAGE_FIELDS[stage]:
        value = payload.get(key)
        lower = -1 if key == "contextState" else 0
        if type(value) in (int, float) and lower <= value <= 2**53 - 1:
            record[key] = value
    if not all(key in record for key in _BAZZA_TIMING_CLOCK):
        return
    if stage == "inbound-rtp":
        if not all(key in record for key in ("pollDurationMs", "receivers", "inboundAudioReports")):
            return
    else:
        sequence = record.get("sequence")
        if type(sequence) is not int or not 1 <= sequence <= 128:
            return
    logger.warning("BAZZA_MEET_AUDIT_TIMING %s", json.dumps(record, separators=(",", ":"), allow_nan=False))
'''


def patch(source):
    if hashlib.sha256(source).hexdigest() != PINNED:
        raise ValueError("Input does not match the generated baseline/candidate audit adapter")
    original = source.decode("utf-8")
    if original.count(LOGGER) != 1 or original.count(DISPATCH) != 1:
        raise ValueError("Pinned logging or dispatch anchor missing or ambiguous")
    output = original.replace(LOGGER, LOGGER + SUPPORT + "\n")
    output = output.replace(DISPATCH, '''                    if json_data_is_dict and json_data.get("type") == "BazzaPlayoutAudit":
                        _log_bazza_meet_audit_timing(json_data)
                        continue
''' + DISPATCH)
    ast.parse(output)
    return output.encode("utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = patch(args.input.read_bytes())
    with args.output.open("xb") as file:
        file.write(result)


if __name__ == "__main__":
    main()
