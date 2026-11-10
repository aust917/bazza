"""Stage the candidate Google Meet WSS prestart from pinned Attendee source."""

import argparse
import ast
import hashlib
from pathlib import Path
import sys


PIN = "eaacec16f2939b595f196b3957a717bbb0ed23e9"
CONTROLLER = "bots/bot_controller/bot_controller.py"
MANAGER = "bots/bot_controller/bot_websocket_client_manager.py"
CLIENT = "bots/bot_controller/bot_websocket_client.py"
HASHES = {
    CONTROLLER: "d912bb938ddd582f7dae53d749b1fed08c1a9c82d884e2df1cdc1614497422d5",
    MANAGER: "9e993e20fcae1915f462bb7178aaffb5e1721590b782e7031f4d2fdabdae502f",
    CLIENT: "2efe4bad3a83a32ae4c13c9500cc520583945d6c3946704a0c05691b8da370b1",
}


def replace_once(source, old, new):
    if source.count(old) != 1:
        raise ValueError("Pinned WSS startup anchor is missing or ambiguous")
    return source.replace(old, new, 1)


def patch_controller(source):
    return replace_once(
        source,
        '            logger.info("take_action_based_on_bot_in_db - JOINING")\n'
        "            BotEventManager.set_requested_bot_action_taken_at(self.bot_in_db)\n"
        "            self.adapter.init()\n",
        '            logger.info("take_action_based_on_bot_in_db - JOINING")\n'
        "            BotEventManager.set_requested_bot_action_taken_at(self.bot_in_db)\n"
        "            if self.websocket_client_manager and self.get_meeting_type() == MeetingTypes.GOOGLE_MEET:\n"
        "                self.websocket_client_manager.start_per_participant_audio()\n"
        "            self.adapter.init()\n",
    )


def patch_manager(source):
    return replace_once(
        source,
        "    def send_per_participant_audio(self, payload: dict):\n",
        "    def start_per_participant_audio(self):\n"
        "        if self._per_participant_audio_client:\n"
        "            self._ensure_started(self._per_participant_audio_client)\n"
        "\n"
        "    def send_per_participant_audio(self, payload: dict):\n",
    )


def patch_client(source, *, numeric_audit=False):
    cleanup_note = (
        'audit.note("cleanup", self._bazza_audit_client, immediate=True, pending=self.send_queue.qsize())\n'
        if numeric_audit
        else ""
    )
    connected_note = '                audit.note("connected", self._bazza_audit_client, immediate=True)\n' if numeric_audit else ""
    old_error = "type(e).__name__" if numeric_audit else "e"
    changes = [
        (
            '        logger.info("Stopping BotWebsocketClient")\n'
            "        try:\n"
            "            if self.websocket:\n"
            "                self.websocket.close()\n"
            "        except Exception as e:\n"
            f'            logger.error("Error closing BotWebsocketClient websocket: %s", {old_error})\n'
            "        finally:\n"
            "            self.connection_state = self.STOPPED\n"
            + ("            " + cleanup_note if cleanup_note else ""),
            '        logger.info("Stopping BotWebsocketClient")\n'
            "        with self._start_connection_lock:\n"
            "            self.connection_state = self.STOPPED\n"
            "            websocket = self.websocket\n"
            "        try:\n"
            "            if websocket:\n"
            "                websocket.close()\n"
            "        except Exception as e:\n"
            '            logger.error("Error closing BotWebsocketClient websocket: %s", type(e).__name__)\n'
            + ("        " + cleanup_note if cleanup_note else ""),
        ),
        (
            "        with self._start_connection_lock:\n"
            "            if self.connection_state == self.CONNECTING:\n",
            "        with self._start_connection_lock:\n"
            "            if self.connection_state == self.STOPPED:\n"
            "                return\n"
            "            if self.connection_state == self.CONNECTING:\n",
        ),
        (
            "                self.websocket = connect(self.websocket_url)\n",
            "                websocket = connect(self.websocket_url)\n",
        ),
        (
            "                self.connection_state = self.CONNECTED\n" + connected_note,
            "                with self._start_connection_lock:\n"
            "                    stopped = self.connection_state == self.STOPPED\n"
            "                    if not stopped:\n"
            "                        self.websocket = websocket\n"
            "                        self.connection_state = self.CONNECTED\n"
            "                if stopped:\n"
            "                    websocket.close()\n"
            "                    return\n" + connected_note,
        ),
        (
            "            except Exception as e:\n"
            "                retries += 1\n",
            "            except Exception as e:\n"
            "                if self.connection_state == self.STOPPED:\n"
            "                    return\n"
            "                retries += 1\n",
        ),
        (
            "        # Handle case where we were stopped before we could connect\n"
            "        if self.connection_state != self.CONNECTING:\n"
            '            logger.info("BotWebsocketClient connection loop exited because connection state is %s", self.connection_state)\n'
            "            return\n"
            "\n"
            "        # Exhausted retries\n"
            "        self.connection_state = self.FAILED\n",
            "        with self._start_connection_lock:\n"
            "            if self.connection_state != self.CONNECTING:\n"
            "                return\n"
            "            self.connection_state = self.FAILED\n",
        ),
    ]
    if not numeric_audit:
        changes.extend(
            [
                (
                    '        logger.info(f"Starting BotWebsocketClient for url {self.websocket_url}")\n',
                    '        logger.info("Starting BotWebsocketClient")\n',
                ),
                (
                    "                    self._max_retries,\n"
                    "                    e,\n",
                    "                    self._max_retries,\n"
                    "                    type(e).__name__,\n",
                ),
            ]
        )
    for old, new in changes:
        source = replace_once(source, old, new)
    return source


def check_sources(sources):
    normalized = {name: sources[name].replace("\r\n", "\n") for name in HASHES}
    for name, source in normalized.items():
        if hashlib.sha256(source.encode("utf-8")).hexdigest() != HASHES[name]:
            raise ValueError(f"{name} does not match Attendee {PIN}")
    return normalized


def build_overlay(sources):
    normalized = check_sources(sources)
    patches = {CONTROLLER: patch_controller, MANAGER: patch_manager, CLIENT: patch_client}
    output = {name: patch(normalized[name]) for name, patch in patches.items()}
    for name, source in output.items():
        ast.parse(source, filename=name)
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="pinned Attendee source directory")
    parser.add_argument("output", type=Path, help="new output directory outside the pinned source")
    args = parser.parse_args()
    source, output = args.source.resolve(), args.output.resolve()
    try:
        if output.is_relative_to(source) or output.exists():
            raise ValueError("The output directory must be new and outside the upstream source")
        files = build_overlay({name: (source / name).read_text(encoding="utf-8") for name in HASHES})
        output.mkdir()
        for name, content in files.items():
            path = output / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8", newline="\n")
    except (OSError, ValueError) as error:
        print(f"Could not stage the Attendee WSS startup candidate: {error}", file=sys.stderr)
        return 1
    print(f"Staged {len(files)} candidate files for Attendee {PIN}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
