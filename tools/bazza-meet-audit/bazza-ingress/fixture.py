"""Loopback-only test for the isolated instrumented Attendee ingress."""

import argparse
import asyncio
import base64
from collections import Counter
import datetime as dt
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import ipaddress
import json
import os
from pathlib import Path
import socket
import ssl
import struct
import subprocess
import tempfile
import threading
import time
import wave

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID
from websockets.asyncio.client import connect
from websockets.exceptions import InvalidStatus

FIXTURE_KEY = "fixture-local-secret"
BOT = "bot_fixture"


def certificates(root):
    ca_key, leaf_key = [ec.generate_private_key(ec.SECP256R1()) for _ in range(2)]
    ca_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Bazza audit test CA")])
    leaf_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Bazza audit test listener")])
    now = dt.datetime.now(dt.timezone.utc)

    def template(key, subject, issuer):
        return (
            x509.CertificateBuilder()
            .public_key(key.public_key())
            .subject_name(subject)
            .issuer_name(issuer)
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - dt.timedelta(days=1))
            .not_valid_after(now + dt.timedelta(days=1))
        )

    ca = (
        template(ca_key, ca_name, ca_name)
        .add_extension(x509.BasicConstraints(ca=True, path_length=0), True)
        .add_extension(x509.SubjectKeyIdentifier.from_public_key(ca_key.public_key()), False)
        .add_extension(x509.KeyUsage(False, False, False, False, False, True, True, None, None), True)
        .sign(ca_key, hashes.SHA256())
    )
    leaf = (
        template(leaf_key, leaf_name, ca_name)
        .add_extension(x509.BasicConstraints(ca=False, path_length=None), True)
        .add_extension(x509.AuthorityKeyIdentifier.from_issuer_public_key(ca_key.public_key()), False)
        .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), False)
        .add_extension(x509.SubjectAlternativeName([x509.IPAddress(ipaddress.ip_address("127.0.0.1"))]), False)
        .sign(ca_key, hashes.SHA256())
    )
    (root / "ca.pem").write_bytes(ca.public_bytes(serialization.Encoding.PEM))
    (root / "cert.pem").write_bytes(leaf.public_bytes(serialization.Encoding.PEM))
    (root / "key.pem").write_bytes(
        leaf_key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        )
    )


def packet(participant, value, stamp, bot=BOT):
    return {
        "bot_id": bot,
        "trigger": "realtime_audio.per_participant",
        "data": {
            "participant_uuid": participant,
            "timestamp_ms": stamp,
            "sample_rate": 16000,
            "chunk": base64.b64encode(struct.pack("<160h", *[value] * 160)).decode(),
        },
    }


async def wait_for(condition):
    async with asyncio.timeout(10):
        while not condition():
            await asyncio.sleep(0.025)


async def verify(executable, root):
    certificates(root)
    run = root / "new-run"
    info = {"left": False}

    class Stub(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, body):
            payload = json.dumps(body).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_POST(self):
            if self.headers.get("Authorization") != f"Token {FIXTURE_KEY}":
                self.send_error(403)
            elif self.path == "/api/v1/bots":
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                info["callback"] = body["websocket_settings"]["per_participant_audio"]["url"]
                self.reply({"id": BOT, "state": "joined_recording"})
            elif self.path == f"/api/v1/bots/{BOT}/leave":
                info["left"] = True
                self.reply({"id": BOT, "state": "leaving"})
            else:
                self.send_error(404)

        def do_GET(self):
            if self.headers.get("Authorization") != f"Token {FIXTURE_KEY}":
                self.send_error(403)
            elif self.path == f"/api/v1/bots/{BOT}":
                self.reply({"id": BOT, "state": "ended" if info["left"] else "joined_recording"})
            elif self.path == f"/api/v1/bots/{BOT}/participants":
                self.reply({"results": [{"uuid": "private-a", "name": "Name A"}, {"uuid": "private-b", "name": "Name B"}], "next": None})
            else:
                self.send_error(404)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Stub)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with socket.socket() as candidate:
        candidate.bind(("127.0.0.1", 0))
        callback_bind = f"127.0.0.1:{candidate.getsockname()[1]}"
    settings = root / "config.json"
    settings.write_text(json.dumps({
        "api_url": f"http://127.0.0.1:{server.server_port}",
        "callback_url": f"wss://{callback_bind}/private/fixture",
        "bind_address": callback_bind,
        "certificate_path": str(root / "cert.pem"),
        "private_key_path": str(root / "key.pem"),
        "meeting_url": "https://meet.google.com/abc-defg-hij",
        "max_seconds": 30,
    }), encoding="utf-8")
    env = {**os.environ, "BAZZA_AUDIT_ATTENDEE_API_KEY": FIXTURE_KEY}
    proc = subprocess.Popen([executable, "--fixture", run, settings], env=env)
    context = ssl.create_default_context(cafile=str(root / "ca.pem"))
    try:
        await wait_for(lambda: (run / "connected.signal").is_file() or proc.poll() is not None)
        assert proc.poll() is None and not (run / "attendee").exists()
        callback = info["callback"]
        try:
            async with connect(callback.split("?")[0] + "?token=invalid", ssl=context, proxy=None):
                raise AssertionError("invalid WSS token accepted")
        except InvalidStatus as failure:
            assert failure.response.status_code == 404
        async with connect(callback, ssl=context, proxy=None) as ws:
            stamp = int(time.time() * 1000)
            pre_adopt = packet("private-a", 999, stamp)
            await ws.send(json.dumps(pre_adopt))
            await (await ws.ping())
            (run / "adopt.signal").touch()
            await wait_for((run / "adopted.signal").is_file)
            assert (run / "attendee").is_dir()
            stamp = int(time.time() * 1000)
            invalid = packet("private-a", 999, stamp)
            invalid["data"]["sample_rate"] = 8000
            await ws.send(json.dumps(invalid))
            await ws.send("{malformed")
            await ws.send(json.dumps(packet("private-a", 999, stamp, "bot_private_forged")))
            await ws.send(json.dumps(packet("private-a", 1, stamp)))
            await ws.send(json.dumps(packet("private-b", 2, stamp)).encode())
            await (await ws.ping())
        (run / "stop.signal").touch()
        assert await asyncio.to_thread(proc.wait, 15) == 0
        assert info["left"]

        text = (run / "ingress.jsonl").read_text(encoding="utf-8")
        rows = [json.loads(line) for line in text.splitlines()]
        counts = Counter(row["event"] for row in rows)
        for kind, count in {
            "wss_handshake_rejected": 1,
            "wss_handshake_accepted": 1,
            "raw_text": 5,
            "raw_binary": 1,
            "rejected_before_adopt": 1,
            "rejected_sample_rate": 1,
            "rejected_json": 1,
            "accepted_pcm": 3,
            "queued_ingress": 3,
            "rejected_bot": 1,
            "queued_writer": 2,
            "writer_consumed": 2,
            "adopted": 1,
            "cleanup_confirmed": 1,
        }.items():
            assert counts[kind] == count, (kind, counts)
        assert rows[-1]["dropped_audit_events"] == 0
        before = next(row for row in rows if row["event"] == "rejected_before_adopt")
        adopted = next(row for row in rows if row["event"] == "adopted")
        assert before["monotonic_us"] < adopted["monotonic_us"]
        assert all(row.get("packet") for row in rows if row["event"] in {"raw_text", "raw_binary"})
        for secret in (FIXTURE_KEY, callback, callback.split("token=")[1], "private-a", "private-b", "bot_private_forged", pre_adopt["data"]["chunk"]):
            assert secret not in text
        manifest = json.loads((run / "attendee" / "tracks.json").read_text(encoding="utf-8"))
        assert {track["display_name"] for track in manifest["tracks"]} == {"Name A", "Name B"}
        for track in manifest["tracks"]:
            with wave.open(str(run / "attendee" / track["file"]), "rb") as pcm:
                expected = 1 if track["display_name"] == "Name A" else 2
                assert pcm.getframerate() == 16000
                samples = struct.unpack(f"<{pcm.getnframes()}h", pcm.readframes(pcm.getnframes()))
                assert set(samples) <= {0, expected} and samples.count(expected) == 160
        print("Loopback WSS audit passed: rejection, adoption, queue, two tracks, confirmed leave.")
    finally:
        if proc.poll() is None:
            (run / "stop.signal").touch(exist_ok=True) if run.exists() else None
            try:
                proc.wait(15)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    arguments = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="bazza-ingress-fixture-") as temporary:
        asyncio.run(verify(arguments.executable.resolve(strict=True), Path(temporary)))
