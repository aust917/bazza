"""Stage a numeric-only provider overlay from the exact pinned Attendee files."""

import argparse
import ast
import hashlib
from pathlib import Path
import sys


PIN = "eaacec16f2939b595f196b3957a717bbb0ed23e9"
WEB_ADAPTER = "bots/web_bot_adapter/web_bot_adapter.py"
BOT_CONTROLLER = "bots/bot_controller/bot_controller.py"
WS_CLIENT = "bots/bot_controller/bot_websocket_client.py"
HELPER = "bots/bazza_meet_audit.py"
HASHES = {
    WEB_ADAPTER: "374cb82e098c8632b822d1502bbb8cd0391a12c59f70711b15d0dc760bbcba63",
    BOT_CONTROLLER: "d912bb938ddd582f7dae53d749b1fed08c1a9c82d884e2df1cdc1614497422d5",
    WS_CLIENT: "2efe4bad3a83a32ae4c13c9500cc520583945d6c3946704a0c05691b8da370b1",
}
AUDIT_IMPORT = "from bots.bazza_meet_audit import audit\n"


def replace_once(source, old, new):
    if source.count(old) != 1:
        raise ValueError("Pinned instrumentation anchor is missing or ambiguous")
    return source.replace(old, new, 1)


def patch_adapter(source):
    source = replace_once(
        source,
        "from bots.bot_adapter import BotAdapter\n",
        "from bots.bot_adapter import BotAdapter\n" + AUDIT_IMPORT,
    )
    source = replace_once(
        source,
        "    def process_per_participant_audio_frame(self, message):\n",
        "    def process_per_participant_audio_frame(self, message):\n"
        '        audit.note("browser_per_participant_received")\n',
    )
    return replace_once(
        source,
        "                    json_data_is_dict = isinstance(json_data, dict)\n",
        "                    json_data_is_dict = isinstance(json_data, dict)\n"
        '                    if json_data_is_dict and json_data.get("type") == "BazzaMeetAuditBrowser":\n'
        "                        audit.browser_snapshot(json_data)\n"
        "                        continue\n",
    )


def patch_controller(source):
    source = replace_once(
        source,
        "from bots.bot_controller.bot_websocket_client_manager import BotWebsocketClientManager\n",
        "from bots.bot_controller.bot_websocket_client_manager import BotWebsocketClientManager\n" + AUDIT_IMPORT,
    )
    return replace_once(
        source,
        "    def send_per_participant_audio_chunk_to_websocket_client(self, speaker_id, chunk_time, chunk_bytes):\n",
        "    def send_per_participant_audio_chunk_to_websocket_client(self, speaker_id, chunk_time, chunk_bytes):\n"
        '        audit.note("controller_per_participant_received")\n',
    )


def patch_client(source):
    changes = [
        (
            "from websockets.sync.client import connect\n",
            "from websockets.sync.client import connect\n\n"
            "from bots.bazza_meet_audit import PER_PARTICIPANT_TRIGGER, audit\n",
        ),
        (
            "        self.send_queue = SimpleQueue()\n",
            "        self.send_queue = SimpleQueue()\n"
            "        self._bazza_audit_client = audit.new_client()\n",
        ),
        (
            '        logger.info(f"Starting BotWebsocketClient for url {self.websocket_url}")\n',
            '        logger.info("Starting BotWebsocketClient")\n',
        ),
        (
            '            logger.error("Error closing BotWebsocketClient websocket: %s", e)\n',
            '            logger.error("Error closing BotWebsocketClient websocket: %s", type(e).__name__)\n',
        ),
        (
            "            self.connection_state = self.STOPPED\n",
            "            self.connection_state = self.STOPPED\n"
            '            audit.note("cleanup", self._bazza_audit_client, immediate=True, pending=self.send_queue.qsize())\n',
        ),
        (
            "    def send_async(self, message: dict):\n"
            "        if self.connection_state == self.CONNECTED:\n"
            "            self.send_queue.put(message)\n"
            "        else:\n",
            "    def send_async(self, message: dict):\n"
            '        per_participant = isinstance(message, dict) and message.get("trigger") == PER_PARTICIPANT_TRIGGER\n'
            "        if self.connection_state == self.CONNECTED:\n"
            "            self.send_queue.put(message)\n"
            '            audit.note("enqueued", self._bazza_audit_client, per_participant=per_participant)\n'
            "        else:\n"
            '            audit.note("drop_disconnected", self._bazza_audit_client, per_participant=per_participant)\n',
        ),
        (
            "            self.connection_state = self.CONNECTING\n",
            "            reconnecting = self.connection_state == self.CONNECTED\n"
            "            self.connection_state = self.CONNECTING\n"
            '            audit.note("reconnect_started" if reconnecting else "connecting", self._bazza_audit_client, immediate=True)\n',
        ),
        (
            "                self.websocket = connect(self.websocket_url)\n",
            '                audit.note("connect_attempt", self._bazza_audit_client, immediate=True)\n'
            "                self.websocket = connect(self.websocket_url)\n",
        ),
        (
            "                self.connection_state = self.CONNECTED\n",
            "                self.connection_state = self.CONNECTED\n"
            '                audit.note("connected", self._bazza_audit_client, immediate=True)\n',
        ),
        (
            "            except Exception as e:\n"
            "                retries += 1\n",
            "            except Exception as e:\n"
            "                retries += 1\n"
            '                audit.note("connect_error", self._bazza_audit_client, immediate=True)\n',
        ),
        (
            "                    self._max_retries,\n"
            "                    e,\n",
            "                    self._max_retries,\n"
            "                    type(e).__name__,\n",
        ),
        (
            "        self.connection_state = self.FAILED\n",
            "        self.connection_state = self.FAILED\n"
            '        audit.note("retries_exhausted", self._bazza_audit_client, immediate=True)\n',
        ),
        (
            "            try:\n"
            "                self.websocket.send(json.dumps(message))\n"
            "            except Exception as e:\n"
            '                logger.info("BotWebsocketClient send failed (%s). Leaving loop.", e)\n',
            '            per_participant = isinstance(message, dict) and message.get("trigger") == PER_PARTICIPANT_TRIGGER\n'
            "            try:\n"
            "                self.websocket.send(json.dumps(message))\n"
            '                audit.note("sent", self._bazza_audit_client, per_participant=per_participant)\n'
            "            except Exception as e:\n"
            '                audit.note("send_error", self._bazza_audit_client, per_participant=per_participant, immediate=True)\n'
            '                logger.info("BotWebsocketClient send failed (%s). Leaving loop.", type(e).__name__)\n',
        ),
        (
            "            except ConnectionClosed:\n",
            "            except ConnectionClosed:\n"
            '                audit.note("peer_closed", self._bazza_audit_client, immediate=True)\n',
        ),
        (
            '                logger.info("BotWebsocketClient recv failed (%s). Leaving loop.", e)\n',
            '                audit.note("receive_error", self._bazza_audit_client, immediate=True)\n'
            '                logger.info("BotWebsocketClient recv failed (%s). Leaving loop.", type(e).__name__)\n',
        ),
    ]
    for old, new in changes:
        source = replace_once(source, old, new)
    return source


def build_overlay(sources):
    patches = {WEB_ADAPTER: patch_adapter, BOT_CONTROLLER: patch_controller, WS_CLIENT: patch_client}
    overlay = {}
    for name, patch in patches.items():
        original = sources[name].replace("\r\n", "\n")
        if hashlib.sha256(original.encode("utf-8")).hexdigest() != HASHES[name]:
            raise ValueError(f"{name} does not match Attendee {PIN}")
        overlay[name] = patch(original)
    overlay[HELPER] = Path(__file__).with_name("audit_runtime.py").read_text(encoding="utf-8")
    for name, content in overlay.items():
        ast.parse(content, filename=name)
    return overlay


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="directory containing the pinned Attendee source")
    parser.add_argument("output", type=Path, help="new directory for read-only worker mounts")
    args = parser.parse_args()
    source, output = args.source.resolve(), args.output.resolve()
    try:
        if output == source or output.is_relative_to(source):
            raise ValueError("The output directory must be outside the upstream source")
        if output.exists():
            raise ValueError("The output directory must not already exist")
        files = build_overlay({name: (source / name).read_text(encoding="utf-8") for name in HASHES})
        output.mkdir()
        for name, content in files.items():
            path = output / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8", newline="\n")
    except (OSError, ValueError) as error:
        print(f"Could not stage the Attendee WSS overlay: {error}", file=sys.stderr)
        return 1
    print(f"Staged {len(files)} files for Attendee {PIN}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
