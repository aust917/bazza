"""Offline test of the staged numeric baseline and startup candidate."""

import argparse
import json
from pathlib import Path
import re
from tempfile import TemporaryDirectory
from types import SimpleNamespace

from stage_worker import BROWSER, MOUNT_ROOT, PROVIDER_AUDIT, stage_worker, startup, VARIANTS
from test_startup import Fixture


def load_staged(output, variant):
    assert json.loads((output / "variant.json").read_text()) == {"variant": variant, "attendee_revision": startup.PIN}
    compose = (output / "worker.compose.yaml").read_text()
    assert "BAZZA_MEET_AUDIT: '1'" in compose
    mounts = re.findall(
        r"      - type: bind\n        source: ([^\n]+)\n        target: /attendee/([^\n]+)"
        r"\n        read_only: true\n        bind:\n          create_host_path: false",
        compose,
    )
    assert len(mounts) == compose.count("      - type: bind") == 6
    assert dict((target, path) for path, target in mounts)[BROWSER] == f"{MOUNT_ROOT}/browser-audit.js"
    provider = {}
    for path, target in mounts:
        assert path.startswith(f"{MOUNT_ROOT}/")
        staged = output / path.removeprefix(f"{MOUNT_ROOT}/")
        assert staged.is_file()
        if target != BROWSER:
            assert path == f"{MOUNT_ROOT}/provider-overlay/{target}"
            provider[target] = staged.read_text(encoding="utf-8")
    return provider


def fixture_for(provider):
    audit_scope = {"__name__": "composed_provider_audit"}
    exec(compile(provider[PROVIDER_AUDIT.HELPER], "<provider-audit>", "exec"), audit_scope)
    rows = []

    def warning(prefix, payload):
        assert prefix == "BAZZA_MEET_AUDIT_WSS %s"
        rows.append(json.loads(payload))

    audit_scope.update(ENABLED=True, LOGGER=SimpleNamespace(warning=warning))
    trigger = audit_scope["PER_PARTICIPANT_TRIGGER"]
    fixture = Fixture(provider, audit=audit_scope["audit"], participant_trigger=trigger)
    return fixture, rows, {"trigger": trigger, "data": {"chunk": "fixture-pcm-marker"}}


def check_transport(provider, variant):
    fixture, rows, packet = fixture_for(provider)
    fixture.begin_join()
    if variant == "baseline":
        assert fixture.client.connection_state == fixture.client.NOT_STARTED
        fixture.send(packet)
    else:
        assert fixture.client.connection_state == fixture.client.CONNECTING
    fixture.complete_handshake()
    fixture.send(packet)
    sent = []

    def send(data):
        sent.append(json.loads(data))
        fixture.client.connection_state = fixture.client.STOPPED

    fixture.socket.send = send
    fixture.client.send_loop()
    fixture.manager.cleanup()
    assert sent == [packet]

    client = str(fixture.client._bazza_audit_client)
    counts = rows[-1]["clients"][client]["counts"]
    assert counts["connecting"] == counts["connect_attempt"] == counts["connected"] == 1
    assert counts["per_participant_enqueued"] == counts["per_participant_sent"] == counts["cleanup"] == 1
    assert counts.get("per_participant_drop_disconnected", 0) == (1 if variant == "baseline" else 0)
    assert all("fixture-pcm-marker" not in json.dumps(row) and "example.invalid" not in json.dumps(row) for row in rows)

    if variant == "candidate":
        fixture, rows, _ = fixture_for(provider)
        fixture.begin_join()
        fixture.during_handshake = fixture.manager.cleanup
        fixture.complete_handshake()
        assert fixture.socket.closed and not fixture.jobs
        client = str(fixture.client._bazza_audit_client)
        counts = rows[-1]["clients"][client]["counts"]
        assert counts["cleanup"] == counts["connect_attempt"] == 1
        assert "connected" not in counts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="unmodified pinned Attendee source directory")
    args = parser.parse_args()
    tool = Path(__file__).resolve().parent
    with TemporaryDirectory(prefix=".worker-test-", dir=tool) as temporary:
        temporary = Path(temporary).resolve()
        if not temporary.is_relative_to(tool):
            raise ValueError("Fixture scratch directory escaped the tool directory")
        providers = {}
        for variant in VARIANTS:
            output = temporary / variant
            stage_worker(args.source, output, variant)
            provider = load_staged(output, variant)
            check_transport(provider, variant)
            providers[variant] = provider
        for path in (PROVIDER_AUDIT.HELPER, PROVIDER_AUDIT.WEB_ADAPTER):
            assert providers["baseline"][path] == providers["candidate"][path]
        assert providers["baseline"][startup.MANAGER] == (args.source / startup.MANAGER).read_text(encoding="utf-8")
        assert (temporary / "baseline/browser-audit.js").read_bytes() == (temporary / "candidate/browser-audit.js").read_bytes()
    print("Verified both bind-mount sets, identical browser/adapter instrumentation, real numeric WSS counters, and candidate cancellation.")


if __name__ == "__main__":
    main()
