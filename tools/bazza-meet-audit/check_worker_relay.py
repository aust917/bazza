"""Verify two trusted WSS round trips from the isolated worker to Windows loopback."""

import argparse
import json
import os
from pathlib import Path
import re
import ssl
import subprocess
import sys
import tempfile
from threading import Thread
import time
from urllib.parse import urlparse

from websockets.sync.server import serve


DISTRO = "BazzaMeetAudioAuditQa"
WORKER_PROBE = b"""from websockets.sync.client import connect
import sys
try:
    for attempt in range(2):
        with connect(sys.argv[1], open_timeout=7, close_timeout=3) as socket:
            socket.send(f'audit-fixture-{attempt}')
            assert socket.recv(timeout=5) == f'audit-fixture-{attempt}'
except Exception as error:
    print(f'{attempt + 1} {type(error).__name__}', file=sys.stderr)
    raise SystemExit(1) from None
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("service_fixture", type=Path)
    parser.add_argument("audit_runtime", type=Path)
    parser.add_argument("--workers", type=int, choices=(1, 3, 9), default=1)
    args = parser.parse_args()
    service = json.loads(args.service_fixture.read_text(encoding="utf-8-sig"))
    callback = urlparse(service["callbackUrl"])
    if callback.scheme != "wss" or callback.hostname != "bazza-attendee.local" or not callback.port:
        raise SystemExit("Expected the existing isolated callback certificate hostname and port")

    wsl = Path(os.environ["SystemRoot"]) / "System32/wsl.exe"
    docker = [str(wsl), "-d", DISTRO, "-u", "root", "--exec", "docker"]
    options = {"capture_output": True, "creationflags": subprocess.CREATE_NO_WINDOW, "timeout": 20, "check": True}
    gateway = subprocess.run(docker + ["network", "inspect", "--format",
        "{{range .IPAM.Config}}{{.Gateway}}{{end}}", "bazza-attendee-local_default"], **options).stdout.decode().strip()
    workers = subprocess.run(docker + ["ps", "--filter", "label=com.docker.compose.service=worker",
        "--format", "{{.Names}}"], **options).stdout.decode().splitlines()
    if len(workers) != args.workers or any(not re.fullmatch(r"bazza-attendee-local-worker-\d+", worker) for worker in workers):
        raise SystemExit("Start the expected number of isolated workers using relay transport")

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(service["certificatePath"], service["privateKeyPath"])

    def echo(connection):
        message = connection.recv(timeout=6)
        if message in ("audit-fixture-0", "audit-fixture-1"):
            connection.send(message)

    with tempfile.TemporaryDirectory(prefix="relay-check-", dir=args.audit_runtime) as temporary:
        temporary = Path(temporary)
        with serve(echo, "127.0.0.1", callback.port, ssl=context) as server:
            thread = Thread(target=server.serve_forever)
            thread.start()
            try:
                failures = []
                for index, worker in enumerate(sorted(workers)):
                    ready, stop = temporary / f"ready-{index}", temporary / f"stop-{index}"
                    relay = subprocess.Popen([sys.executable, "-B", str(Path(__file__).with_name("relay_tcp.py")),
                        "--distribution", DISTRO, "--bind", gateway, "--port", str(callback.port),
                        "--target-port", str(callback.port), "--ready-file", str(ready), "--stop-file", str(stop),
                        "--seconds", "45"], creationflags=subprocess.CREATE_NO_WINDOW)
                    try:
                        deadline = time.monotonic() + 10
                        while not ready.is_file():
                            if relay.poll() is not None or time.monotonic() >= deadline:
                                raise SystemExit("Callback relay did not become ready")
                            time.sleep(0.1)
                        result = subprocess.run(docker + ["exec", "-i", worker, "python", "-",
                            f"wss://bazza-attendee.local:{callback.port}/audit-fixture"],
                            input=WORKER_PROBE, **{**options, "check": False})
                        if result.returncode:
                            lines = result.stderr.decode("utf-8", errors="replace").splitlines()
                            kind = lines[-1] if lines and re.fullmatch(r"[12] [A-Za-z][A-Za-z0-9]*", lines[-1]) else "unclassified"
                            failures.append(f"{worker}: {kind}")
                    finally:
                        stop.touch()
                        if relay.wait(timeout=10):
                            raise SystemExit("Callback relay did not stop cleanly")
                if failures:
                    raise SystemExit("WSS round trip failures (attempt and class): " + ", ".join(failures))
            finally:
                server.shutdown()
                thread.join()
    print(f"Two trusted WSS round trips passed through each of {len(workers)} isolated workers.")


if __name__ == "__main__":
    main()
