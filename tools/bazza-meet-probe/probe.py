"""Schedule synthetic WAV fixtures on independent Attendee Google Meet bots."""

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from http.client import HTTPException
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import threading
import time
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, urlencode, urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener
import uuid
import wave


READY = {"joined_not_recording", "joined_recording"}
LEFT = {"post_processing", "ended", "fatal_error", "data_deleted"}
BOT_ID = re.compile(r"bot_[A-Za-z0-9_-]{1,120}")
MAX_CLIP_SECONDS = 180
REQUEST_TIMEOUT = 8


class ProbeError(Exception):
    pass


class ApiError(ProbeError):
    def __init__(self, method, status=None):
        self.status = status
        detail = f"HTTP {status}" if status is not None else "response unavailable"
        super().__init__(f"Attendee {method}: {detail}")


@dataclass(frozen=True)
class Clip:
    id: str
    at_ms: int
    end_ms: int
    wav: Path


@dataclass(frozen=True)
class Action:
    at_ms: int
    kind: str
    clip: Clip | None = None


@dataclass(frozen=True)
class Speaker:
    id: str
    name: str
    actions: tuple[Action, ...]


@dataclass(frozen=True)
class Plan:
    duration_ms: int
    speakers: tuple[Speaker, ...]


def need(condition, message):
    if not condition:
        raise ProbeError(message)


def text_field(row, field):
    value = row.get(field)
    need(isinstance(value, str) and value.strip() and len(value) <= 256, f"Invalid {field}")
    return value


def time_field(row, field):
    value = row.get(field)
    need(type(value) is int and value >= 0, f"Invalid {field}")
    return value


def rows(document, name):
    value = document.get(name)
    need(isinstance(value, list) and all(isinstance(item, dict) for item in value), f"Invalid {name}")
    return value


def fixture_root(scenario):
    return next(
        (parent for parent in scenario.parents if parent.name == "local-tests" and parent.parent.name == ".git"),
        scenario.parent,
    )


def check_wave(scenario, row, duration):
    path = Path(text_field(row, "wav"))
    need(not path.is_absolute() and not path.drive, "WAV paths must be relative to the scenario")
    path = (scenario.parent / path).resolve()
    need(path.is_relative_to(fixture_root(scenario)) and path.suffix.lower() == ".wav",
         "WAV is outside the fixture directory or has the wrong extension")
    try:
        with wave.open(str(path), "rb") as clip:
            actual = clip.getnframes() * 1000 / clip.getframerate()
    except (OSError, EOFError, wave.Error):
        raise ProbeError(f"Cannot read WAV fixture {path.name}") from None
    need(abs(actual - duration) <= 25, f"WAV duration differs from the plan for {path.name}")
    return path


def read_plan(scenario):
    scenario = scenario.resolve()
    try:
        document = json.loads(scenario.read_text(encoding="utf-8-sig"))
    except (OSError, ValueError):
        raise ProbeError("Cannot read the scenario JSON") from None
    need(isinstance(document, dict) and type(document.get("schema_version")) is int
         and document["schema_version"] == 1, "Expected scenario schema_version 1")
    duration = time_field(document, "duration_ms")
    need(0 < duration <= 55 * 60_000, "Scenario duration must be within 55 minutes")
    participants = rows(document, "participants")
    need(2 <= len(participants) <= 32, "Expected between 2 and 32 speakers")
    names, actions = {}, {}
    for row in participants:
        speaker_id = text_field(row, "id")
        need(speaker_id not in names, "Duplicate participant ID")
        names[speaker_id] = text_field(row, "name")
        actions[speaker_id] = []
    for row in rows(document, "events"):
        speaker_id, kind = text_field(row, "participant_id"), text_field(row, "action")
        at = time_field(row, "at_ms")
        need(speaker_id in actions and kind in {"join", "leave"} and at <= duration, "Invalid join/leave event")
        actions[speaker_id].append(Action(at, kind))
    clip_ids = set()
    for row in rows(document, "utterances"):
        speaker_id, clip_id = text_field(row, "participant_id"), text_field(row, "id")
        at, length = time_field(row, "at_ms"), time_field(row, "duration_ms")
        speech_start, speech_end = time_field(row, "speech_start_ms"), time_field(row, "speech_end_ms")
        need(speaker_id in actions and clip_id not in clip_ids, "Unknown participant or duplicate utterance ID")
        need(0 <= speech_start < speech_end <= length <= MAX_CLIP_SECONDS * 1000
             and at + length <= duration, f"Invalid clip or speech bounds for {clip_id}")
        clip_ids.add(clip_id)
        clip = Clip(clip_id, at, at + length, check_wave(scenario, row, length))
        actions[speaker_id].append(Action(at, "audio", clip))
    need(clip_ids, "Scenario has no utterances")
    speakers = []
    order = {"leave": 0, "join": 1, "audio": 2}
    for speaker_id, entries in actions.items():
        entries.sort(key=lambda item: (item.at_ms, order[item.kind]))
        if not entries or entries[0].kind != "join":
            entries.insert(0, Action(0, "join"))
        joined, audio_end = False, 0
        for entry in entries:
            if entry.kind == "audio":
                need(joined and entry.at_ms >= audio_end, f"Inactive or overlapping clips for {speaker_id}")
                audio_end = entry.clip.end_ms
            else:
                need(joined == (entry.kind == "leave") and entry.at_ms >= audio_end,
                     f"Invalid join/leave sequence for {speaker_id}")
                joined = entry.kind == "join"
        speakers.append(Speaker(speaker_id, names[speaker_id], tuple(entries)))
    return Plan(duration, tuple(speakers))


def prepare_audio(plan, ffmpeg):
    payloads = {}
    child_env = {key: value for key, value in os.environ.items()
                 if key.upper() not in {"BAZZA_ATTENDEE_API_KEY", "BAZZA_TEST_MEET_URL", "ATTENDEE_API_KEY"}}
    for path in {entry.clip.wav for speaker in plan.speakers for entry in speaker.actions if entry.clip}:
        try:
            result = subprocess.run(
                [ffmpeg, "-nostdin", "-v", "error", "-xerror", "-i", str(path), "-map", "0:a:0", "-map_metadata", "-1",
                 "-ac", "1", "-ar", "44100", "-codec:a", "libmp3lame", "-b:a", "96k", "-f", "mp3", "pipe:1"],
                capture_output=True, check=True, timeout=60, env=child_env,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )
        except (OSError, subprocess.SubprocessError):
            raise ProbeError(f"FFmpeg could not encode {path.name}") from None
        need(0 < len(result.stdout) <= 10 * 1024 * 1024, f"Invalid MP3 size for {path.name}")
        payloads[path] = json.dumps({"type": "audio/mp3", "data": base64.b64encode(result.stdout).decode("ascii")}).encode()
    return payloads


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def api_base(value):
    parsed = urlsplit(value)
    need(parsed.scheme in {"http", "https"} and parsed.hostname
         and not (parsed.username or parsed.password or parsed.query or parsed.fragment),
         "Set a valid BAZZA_ATTENDEE_API_URL")
    need(parsed.scheme == "https" or parsed.hostname in {"localhost", "127.0.0.1", "::1"},
         "Attendee must use HTTPS outside loopback")
    base = value.rstrip("/")
    return base if base.endswith("/api/v1") else base + "/api/v1"


def meet_url(value):
    need(re.fullmatch(r"https://meet\.google\.com/[a-z]{3}-[a-z]{4}-[a-z]{3}/?", value),
         "Set BAZZA_TEST_MEET_URL to the direct URL of the closed test Meet")
    return value.rstrip("/")


class Api:
    def __init__(self, base, token):
        self.base, self.token = base, token
        self.opener = build_opener(ProxyHandler({}), NoRedirect())

    def request(self, method, path, body=None, before_send=None):
        data = body if isinstance(body, bytes) else json.dumps(body).encode() if body is not None else None
        request = Request(self.base + path, method=method, data=data, headers={
            "Authorization": "Token " + self.token, "Content-Type": "application/json",
        })
        try:
            if before_send:
                before_send()
            with self.opener.open(request, timeout=REQUEST_TIMEOUT) as response:
                raw = response.read(2 * 1024 * 1024 + 1)
            if len(raw) > 2 * 1024 * 1024:
                raise ApiError(method)
            return json.loads(raw) if raw else None
        except HTTPError as error:
            error.close()
            raise ApiError(method, error.code) from None
        except (URLError, OSError, ValueError, HTTPException):
            raise ApiError(method) from None

    def participants(self, bot_id):
        cursor = None
        for _ in range(8):
            query = "?" + urlencode({"cursor": cursor}) if cursor else ""
            page = self.request("GET", f"/bots/{bot_id}/participants" + query)
            if isinstance(page, list):
                yield from page
                return
            need(isinstance(page, dict) and isinstance(page.get("results"), list), "Invalid roster response")
            yield from page["results"]
            if not page.get("next"):
                return
            # Follow the cursor only, never a server-supplied host or path.
            need(isinstance(page["next"], str), "Invalid roster cursor")
            cursor = parse_qs(urlsplit(page["next"]).query).get("cursor", [None])[0]
            need(cursor is not None, "Invalid roster cursor")


class Log:
    def __init__(self, file):
        self.file, self.origin_ns = file, None
        self.lock = threading.Lock()

    def emit(self, event, **details):
        with self.lock:
            now = time.perf_counter_ns()
            elapsed = None if self.origin_ns is None else (now - self.origin_ns) / 1e6
            print(json.dumps({"event": event, "mono_ns": now, "elapsed_ms": elapsed, **details},
                             ensure_ascii=False), file=self.file, flush=True)

    def start(self):
        with self.lock:
            before = time.perf_counter_ns()
            utc = time.time_ns()
            after = time.perf_counter_ns()
            self.origin_ns = (before + after) // 2
        self.emit("scenario_start", origin_mono_ns=self.origin_ns, origin_utc_ns=utc,
                  clock_sample_span_ns=after - before)


class Runner:
    def __init__(self, plan, args, api, meeting, audio, log):
        self.plan, self.args, self.api, self.meeting, self.audio, self.log = plan, args, api, meeting, audio, log
        self.run_id = uuid.uuid4().hex[:10]
        self.names = {speaker.id: f"Bazza QA {self.run_id} {index:02} {speaker.name}"[:95]
                      for index, speaker in enumerate(plan.speakers, 1)}
        self.owned, self.active = set(), {}
        self.lock, self.stop = threading.Lock(), threading.Event()

    def get_state(self, bot_id):
        result = self.api.request("GET", f"/bots/{bot_id}")
        need(isinstance(result, dict) and result.get("id") == bot_id and isinstance(result.get("state"), str)
             and re.fullmatch(r"[a-z_]{1,64}", result["state"]),
             "Invalid bot state response")
        return result["state"]

    def recover(self, dedup):
        for _ in range(3):
            try:
                page = self.api.request("GET", "/bots?" + urlencode({"deduplication_key": dedup}))
                for row in page.get("results", []):
                    if row.get("deduplication_key") == dedup and BOT_ID.fullmatch(row.get("id") or ""):
                        return row["id"]
            except (ApiError, AttributeError, TypeError):
                pass
            time.sleep(0.4)
        return None

    def join(self, speaker_id, at_ms):
        need(not self.stop.is_set(), "Probe stopped before bot creation")
        key = "bazza-qa-" + uuid.uuid4().hex
        name = self.names[speaker_id]
        max_uptime = self.plan.duration_ms // 1000 + self.args.join_timeout + 60
        if self.args.start_file:
            max_uptime += self.args.start_timeout
        body = {
            "meeting_url": self.meeting, "bot_name": name, "deduplication_key": key,
            "recording_settings": {"format": "none", "record_async_transcription_audio_chunks": False},
            "transcription_settings": {"meeting_closed_captions": {}},
            "automatic_leave_settings": {
                "waiting_room_timeout_seconds": self.args.join_timeout,
                "max_uptime_seconds": min(3500, max_uptime),
            },
        }
        if self.args.login_group:
            body["google_meet_settings"] = {"use_login": True, "login_group_name": self.args.login_group}
        self.log.emit("join_requested", participant_id=speaker_id, bot_name=name, planned_at_ms=at_ms,
                      deduplication_key=key)
        try:
            result = self.api.request("POST", "/bots", body)
            bot_id = result.get("id") if isinstance(result, dict) else None
            if not isinstance(bot_id, str) or not BOT_ID.fullmatch(bot_id) or result.get("deduplication_key") != key:
                raise ApiError("POST")
        except ApiError as error:
            if error.status is not None and error.status < 500:
                raise
            bot_id = self.recover(key)
            if bot_id is None:
                self.log.emit("join_unconfirmed", participant_id=speaker_id, bot_name=name, deduplication_key=key)
                raise ProbeError("Bot creation could not be confirmed. Check the test meeting for its named bot.") from None
        with self.lock:
            self.owned.add(bot_id)
        self.log.emit("bot_created", participant_id=speaker_id, bot_id=bot_id)
        deadline, previous, failures = time.monotonic() + self.args.join_timeout, None, 0
        while not self.stop.is_set() and time.monotonic() < deadline:
            try:
                state = self.get_state(bot_id)
                failures = 0
            except ApiError as error:
                failures += 1
                if error.status in {401, 403} or failures >= 3:
                    raise
                self.stop.wait(1)
                continue
            if state != previous:
                self.log.emit("bot_state", participant_id=speaker_id, bot_id=bot_id, state=state)
                previous = state
            if state in READY:
                with self.lock:
                    self.active[speaker_id] = bot_id
                return bot_id
            if state in LEFT or state == "joined_recording_permission_denied":
                raise ProbeError(f"Test bot {speaker_id} could not join ({state})")
            self.stop.wait(1)
        raise ProbeError(f"Admission did not complete for test bot {speaker_id}")

    def leave(self, bot_id):
        accepted = False
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            try:
                if self.get_state(bot_id) in LEFT:
                    with self.lock:
                        self.owned.discard(bot_id)
                        for speaker_id, current in tuple(self.active.items()):
                            if current == bot_id:
                                del self.active[speaker_id]
                    self.log.emit("bot_left", bot_id=bot_id)
                    return True
            except ProbeError:
                pass
            if not accepted:
                try:
                    self.api.request("POST", f"/bots/{bot_id}/leave", {})
                    accepted = True
                    self.log.emit("leave_requested", bot_id=bot_id)
                except ApiError:
                    pass
            time.sleep(1)
        self.log.emit("leave_unconfirmed", bot_id=bot_id)
        return False

    def wait_until(self, at_ms):
        seconds = (self.log.origin_ns + at_ms * 1_000_000 - time.perf_counter_ns()) / 1e9
        return not self.stop.wait(max(0, seconds))

    def play(self, speaker, bot_id, clip):
        def before_send():
            need(not self.stop.is_set(), "Probe stopped before audio dispatch")
            late_ms = (time.perf_counter_ns() - self.log.origin_ns) / 1e6 - clip.at_ms
            if late_ms > self.args.max_send_late_ms:
                self.log.emit("audio_skipped", **details, lateness_ms=late_ms)
                raise ProbeError(f"Missed send deadline for {clip.id}")
            self.log.emit("audio_post_started", **details, lateness_ms=late_ms)

        details = {"participant_id": speaker.id, "bot_id": bot_id, "utterance_id": clip.id,
                   "planned_at_ms": clip.at_ms}
        try:
            self.api.request("POST", f"/bots/{bot_id}/output_audio", self.audio[clip.wav], before_send)
        except ApiError as error:
            self.log.emit("audio_post_unconfirmed", **details, http_status=error.status)
            raise
        self.log.emit("audio_post_ack", **details)

    def follow(self, speaker, bot_id):
        for entry in speaker.actions[bool(bot_id):]:
            if not self.wait_until(entry.at_ms):
                return
            if entry.kind == "join":
                bot_id = self.join(speaker.id, entry.at_ms)
            elif entry.kind == "leave":
                need(self.leave(bot_id), f"Could not confirm leave for {speaker.id}")
                bot_id = None
            else:
                self.play(speaker, bot_id, entry.clip)

    def observe(self):
        seen = set()
        speaker_by_name = {name: speaker for speaker, name in self.names.items()}
        while not self.stop.is_set():
            with self.lock:
                active = tuple(self.active.values())
            witnesses = [self.args.observer_bot_id] if self.args.observer_bot_id else list(active[:2])
            for witness in witnesses:
                try:
                    for row in self.api.participants(witness):
                        if not isinstance(row, dict):
                            continue
                        name = row.get("name")
                        speaker_id = speaker_by_name.get(name) if isinstance(name, str) else None
                        found = row.get("uuid")
                        if speaker_id is None or not isinstance(found, str) or not 0 < len(found) <= 512:
                            continue
                        key = (witness, speaker_id, found)
                        if key in seen:
                            continue
                        seen.add(key)
                        self.log.emit("participant_discovered", participant_id=speaker_id,
                                      attendee_participant_uuid=found, observer_bot_id=witness, bot_name=name)
                except (ProbeError, ValueError) as error:
                    self.log.emit("observer_unavailable", observer_bot_id=witness, reason=str(error))
            self.stop.wait(2)
        observed = {speaker for _, speaker, _ in seen}
        for speaker_id in self.names.keys() - observed:
            self.log.emit("participant_unmatched", participant_id=speaker_id)

    def run(self):
        self.log.emit("probe_started", run_id=self.run_id, speakers=len(self.plan.speakers))
        problems = []
        with ThreadPoolExecutor(max_workers=len(self.plan.speakers) + 1) as pool:
            try:
                pool.submit(self.observe)
                warm = {pool.submit(self.join, speaker.id, 0): speaker.id for speaker in self.plan.speakers
                        if speaker.actions[0].at_ms == 0 and speaker.actions[0].kind == "join"}
                ready = {warm[future]: future.result() for future in as_completed(warm)}
                self.log.emit("ready_to_start", admitted=len(ready))
                if self.args.start_file:
                    deadline = time.monotonic() + self.args.start_timeout
                    while not self.args.start_file.is_file() and time.monotonic() < deadline:
                        time.sleep(0.1)
                    need(self.args.start_file.is_file(), "No scenario start signal received")
                self.log.start()
                scheduled = {pool.submit(self.follow, speaker, ready.get(speaker.id)): speaker.id
                             for speaker in self.plan.speakers}
                for future in as_completed(scheduled):
                    try:
                        future.result()
                    except ProbeError as error:
                        problems.append(str(error))
                        self.log.emit("speaker_failed", participant_id=scheduled[future], reason=str(error))
                self.wait_until(self.plan.duration_ms)
            finally:
                self.stop.set()
        return problems

    def cleanup(self):
        with ThreadPoolExecutor(max_workers=len(self.plan.speakers)) as pool:
            return all(list(pool.map(self.leave, tuple(self.owned))))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", type=Path)
    parser.add_argument("--confirm-test-meeting", action="store_true", help="actually join the dedicated test Meet")
    parser.add_argument("--prepare", action="store_true", help="also exercise FFmpeg in offline mode")
    parser.add_argument("--ffmpeg", default="ffmpeg")
    parser.add_argument("--log", type=Path, help="new JSONL file for live runs")
    parser.add_argument("--observer-bot-id", help="existing recorder's API ID in the same Attendee project")
    parser.add_argument("--login-group", help="preconfigured Attendee test login group")
    parser.add_argument("--join-timeout", type=int, default=180)
    parser.add_argument("--max-send-late-ms", type=int, default=1000)
    parser.add_argument("--start-file", type=Path, help="wait for this file to appear after initial admission")
    parser.add_argument("--start-timeout", type=int, default=120)
    args = parser.parse_args()
    try:
        plan = read_plan(args.scenario)
        if not args.confirm_test_meeting:
            count = len(prepare_audio(plan, args.ffmpeg)) if args.prepare else 0
            print(f"Offline validation: {len(plan.speakers)} speakers, {plan.duration_ms} ms, {count} WAVs encoded")
            return 0
        need(args.log is not None and 10 <= args.join_timeout <= 600
             and 0 <= args.max_send_late_ms <= 10_000 and 1 <= args.start_timeout <= 600, "Invalid live options or missing --log")
        wait_seconds = args.start_timeout if args.start_file else 0
        need(plan.duration_ms / 1000 + args.join_timeout + wait_seconds + 60 <= 3500,
             "Admission, start wait and scenario exceed the pinned worker's uptime budget")
        need(not args.observer_bot_id or BOT_ID.fullmatch(args.observer_bot_id), "Invalid observer bot ID")
        need(not args.start_file or not args.start_file.exists(), "Start signal must not exist before launch")
        token = os.environ.get("BAZZA_ATTENDEE_API_KEY", "").strip()
        need(bool(token) and not any(char.isspace() for char in token), "Set BAZZA_ATTENDEE_API_KEY")
        base = api_base(os.environ.get("BAZZA_ATTENDEE_API_URL", ""))
        meeting = meet_url(os.environ.get("BAZZA_TEST_MEET_URL", ""))
        audio = prepare_audio(plan, args.ffmpeg)
        with args.log.open("x", encoding="utf-8") as file:
            runner = Runner(plan, args, Api(base, token), meeting, audio, Log(file))
            try:
                problems = runner.run()
            finally:
                clean = runner.cleanup()
            need(clean, "One or more test bot departures are unconfirmed. Check the test meeting and the JSONL log.")
            need(not problems, f"{len(problems)} speaker(s) had errors. See the JSONL log.")
        print(f"All scheduled audio requests acknowledged for {len(plan.speakers)} test speakers. See the JSONL log.")
        return 0
    except KeyboardInterrupt:
        print("Probe interrupted. Check the JSONL log for departures.", file=sys.stderr)
        return 130
    except ProbeError as error:
        print(str(error), file=sys.stderr)
        return 1
    except (OSError, ValueError):
        print("Could not open the local log or parse the configuration.", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
