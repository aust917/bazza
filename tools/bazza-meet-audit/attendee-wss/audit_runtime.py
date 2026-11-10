"""Numeric-only diagnostics for one disposable Attendee recorder worker."""

import json
import logging
import os
import time
from threading import Lock


LOGGER = logging.getLogger(__name__)
ENABLED = os.environ.get("BAZZA_MEET_AUDIT") == "1"
REPORT_INTERVAL_NS = 1_000_000_000
MAX_BROWSER_NUMBER = (1 << 53) - 1
PER_PARTICIPANT_TRIGGER = "realtime_audio.per_participant"
BROWSER_COUNTERS = (
    "receiver_count",
    "csrc_snapshots",
    "roster_updates",
    "device_output_updates",
    "frames_seen",
    "frames_zero",
    "frames_nonzero",
    "frames_empty_sources",
    "frames_no_mapped_source",
    "frames_mapped",
    "frames_multiple_sources",
    "frames_multiple_mapped_sources",
    "frames_controller_inactive",
    "transform_errors",
    "send_attempts",
    "send_accepted",
    "send_socket_not_open",
    "send_media_disabled",
    "send_errors",
)


class Counters:
    def __init__(self):
        self.counts = {}
        self.first = {}

    def increment(self, key, now):
        count = self.counts.get(key, 0) + 1
        self.counts[key] = count
        if count == 1:
            self.first[key] = {"unix_ms": time.time_ns() // 1_000_000, "mono_ns": now}
        return count == 1

    def snapshot(self):
        return {"counts": dict(self.counts), "first": dict(self.first)}


class ProviderAudit:
    def __init__(self):
        self.lock = Lock()
        self.worker = Counters()
        self.clients = {}
        self.next_client_id = 0
        self.last_report_ns = None

    def new_client(self):
        if not ENABLED:
            return None
        with self.lock:
            self.next_client_id += 1
            self.clients[self.next_client_id] = Counters()
            return self.next_client_id

    def note(self, event, client_id=None, *, per_participant=False, immediate=False, pending=None):
        if not ENABLED:
            return
        with self.lock:
            now = time.monotonic_ns()
            counters = self.worker if client_id is None else self.clients[client_id]
            first = counters.increment(event, now)
            if per_participant:
                first = counters.increment("per_participant_" + event, now) or first
            if (
                not first
                and not immediate
                and self.last_report_ns is not None
                and now - self.last_report_ns < REPORT_INTERVAL_NS
            ):
                return
            self.last_report_ns = now
            report = {
                "schema": 1,
                "pid": os.getpid(),
                "event": event,
                "unix_ms": time.time_ns() // 1_000_000,
                "mono_ns": now,
                "worker": self.worker.snapshot(),
                "clients": {key: value.snapshot() for key, value in self.clients.items()},
            }
            if client_id is not None:
                report["client_id"] = client_id
            if pending is not None:
                report["pending_on_cleanup"] = pending
        LOGGER.warning("BAZZA_MEET_AUDIT_WSS %s", json.dumps(report, separators=(",", ":")))

    def browser_snapshot(self, payload):
        if not ENABLED or type(payload.get("schema")) is not int or payload["schema"] != 1:
            return
        elapsed_ms = payload.get("elapsed_ms")
        counters = payload.get("counters")
        if not numeric(elapsed_ms) or not isinstance(counters, dict):
            return
        clean = {key: counters.get(key) for key in BROWSER_COUNTERS}
        if not all(numeric(value) for value in clean.values()):
            return
        report = {
            "schema": 1,
            "pid": os.getpid(),
            "worker_unix_ms": time.time_ns() // 1_000_000,
            "worker_mono_ns": time.monotonic_ns(),
            "elapsed_ms": elapsed_ms,
            "counters": clean,
        }
        LOGGER.warning("BAZZA_MEET_AUDIT_BROWSER %s", json.dumps(report, separators=(",", ":")))


def numeric(value):
    return type(value) is int and 0 <= value <= MAX_BROWSER_NUMBER


audit = ProviderAudit()
