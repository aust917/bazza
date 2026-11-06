"""Exercise the real probe CLI against a loopback Attendee fixture."""

import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
from urllib.parse import parse_qs, urlsplit
import wave


@unittest.skipUnless(shutil.which("ffmpeg"), "FFmpeg is required")
class ProbeTest(unittest.TestCase):
    def test_parallel_dispatch_lost_create_response_and_cleanup(self):
        bots, outputs, leaves = {}, [], []
        lock = threading.Lock()

        class Attendee(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def reply(self, data):
                body = json.dumps(data).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                with lock:
                    if self.path == "/api/v1/bots":
                        bot_id = f"bot_fixture{len(bots) + 1}"
                        bot = {"id": bot_id, "state": "joined_not_recording", **body}
                        bots[bot_id] = bot
                        if len(bots) == 1:
                            self.connection.shutdown(socket.SHUT_RDWR)
                            return
                        self.reply(bot)
                    else:
                        bot_id, action = self.path.split("/")[-2:]
                        if action == "output_audio":
                            outputs.append((bot_id, body["type"], len(base64.b64decode(body["data"]))))
                        else:
                            assert action == "leave"
                            leaves.append(bot_id)
                            bots[bot_id]["state"] = "ended"
                        self.reply({})

            def do_GET(self):
                request = urlsplit(self.path)
                with lock:
                    if request.path.endswith("/participants"):
                        self.reply({"results": [
                            {"name": bot["bot_name"], "uuid": f"speaker_{bot['id']}"} for bot in bots.values()
                        ]})
                    elif request.path.endswith("/bots"):
                        key = parse_qs(request.query)["deduplication_key"][0]
                        self.reply({"results": [bot for bot in bots.values() if bot["deduplication_key"] == key]})
                    else:
                        self.reply(bots[request.path.split("/")[-1]])

        server = ThreadingHTTPServer(("127.0.0.1", 0), Attendee)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with wave.open(str(root / "clip.wav"), "wb") as wav:
                wav.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
                wav.writeframes(b"\0\0" * 1600)
            ids = ("a", "b")
            scenario = {
                "schema_version": 1, "duration_ms": 2500,
                "participants": [{"id": speaker, "name": "Same name", "voice": speaker} for speaker in ids],
                "events": [{"participant_id": speaker, "at_ms": 0, "action": "join"} for speaker in ids],
                "utterances": [{"id": f"{speaker}-1", "participant_id": speaker, "at_ms": 100,
                                "duration_ms": 100, "speech_start_ms": 0, "speech_end_ms": 100,
                                "wav": "clip.wav", "text": "synthetic"} for speaker in ids],
            }
            plan, log = root / "scenario.json", root / "probe.jsonl"
            plan.write_text(json.dumps(scenario), encoding="utf-8")
            env = {
                **os.environ, "BAZZA_ATTENDEE_API_URL": f"http://127.0.0.1:{server.server_port}",
                "BAZZA_ATTENDEE_API_KEY": "loopback-test-token", "BAZZA_TEST_MEET_URL": "https://meet.google.com/aaa-bbbb-ccc",
            }
            result = subprocess.run(
                [sys.executable, "-B", str(Path(__file__).with_name("probe.py")), str(plan),
                 "--confirm-test-meeting", "--log", str(log), "--observer-bot-id", "bot_observer"],
                env=env, capture_output=True, text=True, timeout=20,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            journal = log.read_text(encoding="utf-8")
        self.assertNotIn("loopback-test-token", journal)
        self.assertNotIn("meet.google.com", journal)
        self.assertEqual(len(bots), 2)
        self.assertEqual(len({bot["bot_name"] for bot in bots.values()}), 2)
        self.assertEqual(set(leaves), set(bots))
        self.assertEqual({bot_id for bot_id, _, _ in outputs}, set(bots))
        self.assertEqual(len(outputs), 2)
        self.assertTrue(all(kind == "audio/mp3" and length > 100 for _, kind, length in outputs))
        events = [json.loads(line) for line in journal.splitlines()]
        started = [event for event in events if event["event"] == "audio_post_started"]
        self.assertEqual({event["participant_id"] for event in started}, set(ids))
        self.assertTrue(all(event["mono_ns"] > 0 and event["planned_at_ms"] == 100 for event in started))
        found = [event for event in events if event["event"] == "participant_discovered"]
        self.assertEqual({event["participant_id"] for event in found}, set(ids))
        self.assertEqual(len({event["attendee_participant_uuid"] for event in found}), 2)


if __name__ == "__main__":
    unittest.main()
