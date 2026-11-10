"""Deterministic handshake fixture using the actual pinned Attendee classes."""

import argparse
import ast
import json
import logging
from pathlib import Path
from queue import Empty, SimpleQueue
from threading import Lock
from types import SimpleNamespace
from typing import Callable


CONTROLLER = "bots/bot_controller/bot_controller.py"
MANAGER = "bots/bot_controller/bot_websocket_client_manager.py"
CLIENT = "bots/bot_controller/bot_websocket_client.py"
SOURCE_FILES = (CONTROLLER, MANAGER, CLIENT)
STATES = SimpleNamespace(JOINING="joining", STAGED="staged", LEAVING="leaving", CONNECTING="connecting", DISCONNECTING="disconnecting")
MEETINGS = SimpleNamespace(GOOGLE_MEET="meet", TEAMS="teams")
AUDIO = "wss://example.invalid/audio"
MIXED = "wss://example.invalid/mixed"
VIDEO = "wss://example.invalid/video"
FRAMES = [{"frame": n} for n in range(3)]


def class_from_source(source, class_name, namespace, method=None):
    tree = ast.parse(source)
    class_node = next(node for node in tree.body if isinstance(node, ast.ClassDef) and node.name == class_name)
    if method is not None:
        class_node.body = [node for node in class_node.body if isinstance(node, ast.FunctionDef) and node.name == method]
    tree.body = [class_node]
    exec(compile(tree, "<pinned-attendee>", "exec"), namespace)
    return namespace[class_name]


class Socket:
    def __init__(self):
        self.closed = False

    def close(self):
        self.closed = True


class Fixture:
    def __init__(
        self, sources, *, participant_audio=AUDIO, mixed_audio=MIXED, state=STATES.JOINING,
        meeting=MEETINGS.GOOGLE_MEET, audit=None, participant_trigger=None,
    ):
        self.calls = []
        self.jobs = []
        self.socket = Socket()
        self.during_handshake = lambda: None
        self.logger = logging.Logger("handshake_fixture", logging.CRITICAL)
        self.logger.addHandler(logging.NullHandler())

        def connect(url):
            assert url == AUDIO
            self.calls.append("handshake")
            self.during_handshake()
            return self.socket

        fixture = self

        class Thread:
            def __init__(self, target, daemon):
                assert daemon
                self.target = target
                self.alive = False

            def start(self):
                self.alive = True
                fixture.jobs.append(self)

            def is_alive(self):
                return self.alive

        client_class = class_from_source(
            sources[CLIENT],
            "BotWebsocketClient",
            dict(
                logger=self.logger, connect=connect, SimpleQueue=SimpleQueue, Empty=Empty, Lock=Lock, Thread=Thread,
                Callable=Callable, json=json, audit=audit, PER_PARTICIPANT_TRIGGER=participant_trigger,
            ),
        )
        manager_class = class_from_source(
            sources[MANAGER],
            "BotWebsocketClientManager",
            dict(logger=self.logger, BotWebsocketClient=client_class, Callable=Callable),
        )
        controller_class = class_from_source(
            sources[CONTROLLER],
            "BotController",
            dict(
                logger=self.logger,
                BotStates=STATES,
                MeetingTypes=MEETINGS,
                BotEventManager=SimpleNamespace(set_requested_bot_action_taken_at=lambda bot: self.calls.append("action")),
            ),
            method="take_action_based_on_bot_in_db",
        )
        self.manager = manager_class(mixed_audio, participant_audio, VIDEO, lambda message: None)
        self.client = self.manager._per_participant_audio_client
        self.controller = controller_class()
        self.controller.bot_in_db = SimpleNamespace(state=state, join_at=SimpleNamespace(isoformat=lambda: "test"))
        self.controller.websocket_client_manager = self.manager
        self.controller.get_meeting_type = lambda: meeting
        self.controller.adapter = SimpleNamespace(init=lambda: self.calls.append(("adapter", len(self.jobs))))

    def begin_join(self):
        self.controller.take_action_based_on_bot_in_db()

    def complete_handshake(self):
        thread = self.jobs.pop(0)
        assert thread.target.__name__ == "_connection_loop"
        thread.target()
        thread.alive = False

    def send(self, *frames):
        for frame in frames:
            self.manager.send_per_participant_audio(frame)

    def accepted(self):
        items = []
        while True:
            try:
                items.append(self.client.send_queue.get_nowait())
            except Empty:
                return items


def original(sources):
    fixture = Fixture(sources)
    fixture.begin_join()
    assert fixture.calls == ["action", ("adapter", 0)]
    assert fixture.client.connection_state == fixture.client.NOT_STARTED

    fixture.send(FRAMES[0])
    assert fixture.client.connection_state == fixture.client.CONNECTING
    fixture.during_handshake = lambda: fixture.send(FRAMES[1])
    fixture.complete_handshake()
    fixture.send(FRAMES[2])
    assert fixture.accepted() == FRAMES[2:]
    assert fixture.client.dropped_message_ticker == 2
    print("Pinned lazy startup drops 2 of 2 PCM frames before handshake completion and queues the next frame.")

    fixture = Fixture(sources)
    fixture.send(FRAMES[0])
    fixture.during_handshake = fixture.manager.cleanup
    fixture.complete_handshake()
    assert fixture.client.connection_state == fixture.client.CONNECTED and not fixture.socket.closed
    print("Pinned client adopts an open socket after cleanup during a pending handshake.")


def candidate(sources):
    fixture = Fixture(sources)
    fixture.begin_join()
    assert fixture.calls == ["action", ("adapter", 1)]
    fixture.manager.start_per_participant_audio()
    assert len(fixture.jobs) == 1
    fixture.complete_handshake()
    fixture.send(*FRAMES)
    assert fixture.accepted() == FRAMES
    assert fixture.client.dropped_message_ticker == 0
    print("Candidate schedules one handshake before adapter startup and queues all 3 frames when it completes first.")

    fixture = Fixture(sources)
    fixture.begin_join()
    fixture.send(FRAMES[0])
    fixture.during_handshake = lambda: fixture.send(FRAMES[1])
    fixture.complete_handshake()
    fixture.send(FRAMES[2])
    assert fixture.accepted() == FRAMES[2:]
    assert fixture.client.dropped_message_ticker == 2
    print("Candidate still drops 2 of 2 frames during a slow handshake and queues the next frame without replay.")

    for kwargs in (
        {"participant_audio": None},
        {"state": STATES.STAGED},
        {"state": STATES.CONNECTING},
        {"meeting": MEETINGS.TEAMS},
    ):
        fixture = Fixture(sources, **kwargs)
        fixture.begin_join()
        assert not fixture.jobs
    fixture = Fixture(sources)
    fixture.controller.websocket_client_manager = None
    fixture.begin_join()
    assert not fixture.jobs

    fixture = Fixture(sources, mixed_audio=AUDIO)
    fixture.begin_join()
    assert fixture.manager._mixed_audio_client is fixture.client
    assert fixture.manager._per_participant_video_client.connection_state == fixture.client.NOT_STARTED
    assert len(fixture.jobs) == 1
    print("Candidate leaves staged, app-session, non-Meet and mixed/video-only startup lazy and reuses a shared URL.")

    fixture = Fixture(sources)
    fixture.begin_join()
    fixture.during_handshake = fixture.manager.cleanup
    fixture.complete_handshake()
    assert fixture.client.connection_state == fixture.client.STOPPED
    assert fixture.socket.closed
    assert not fixture.jobs
    fixture.client.start()
    assert fixture.client.connection_state == fixture.client.STOPPED
    assert not fixture.jobs
    print("Candidate closes a socket returned after cleanup and starts no workers or retries.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="pinned Attendee source directory")
    parser.add_argument("--candidate", action="store_true", help="also verify the candidate overlay")
    args = parser.parse_args()
    from patch import build_overlay, check_sources

    sources = check_sources({name: (args.source / name).read_text(encoding="utf-8") for name in SOURCE_FILES})
    original(sources)
    if args.candidate:
        candidate(build_overlay(sources))


if __name__ == "__main__":
    main()
