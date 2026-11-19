#!/usr/bin/env bash
set -euo pipefail

test "${WSL_DISTRO_NAME:-}" = BazzaMeetAudioAuditQa
workers=${1:-1}
variant=${2:-legacy}
transport=${3:-direct}
timing=${4:-standard}
case "$workers" in
    1|3|9) ;;
    *) exit 2 ;;
esac

cd /opt/bazza-attendee
export BAZZA_API_PORT=23497
audit_runtime=/mnt/e/WSL/BazzaMeetAudioAuditQa
audit_tools=/mnt/e/Claude/Development/Bazza/tools/bazza-meet-audit
export BAZZA_AUDIT_ROOT="$audit_runtime"
export BAZZA_WINDOWS_HOST
case "$transport" in
    direct) BAZZA_WINDOWS_HOST=$(ip -4 route show default | awk 'NR == 1 {print $3}') ;;
    relay) BAZZA_WINDOWS_HOST=$(docker network inspect --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}' bazza-attendee-local_default) ;;
    *) exit 2 ;;
esac
test -n "$BAZZA_WINDOWS_HOST"
case "$variant" in
    legacy) overlay="$audit_tools/audit.compose.yaml" ;;
    baseline|candidate)
        BAZZA_AUDIT_ROOT="$BAZZA_AUDIT_ROOT/worker-$variant"
        overlay="$BAZZA_AUDIT_ROOT/worker.compose.yaml"
        ;;
    *) exit 2 ;;
esac
test -f "$overlay"
compose=(docker compose --env-file credentials.env --env-file runtime.env -f compose.yaml -f "$overlay")
case "$timing" in
    standard) ;;
    timing)
        test "$variant" = candidate
        export BAZZA_PLAYOUT_PAYLOAD="$audit_runtime/playout-audit.js"
        export BAZZA_PLAYOUT_ADAPTER="$audit_runtime/timing-web-bot-adapter.py"
        export BAZZA_RTP_PAYLOAD="$audit_runtime/rtp-audit.js"
        for file in "$BAZZA_PLAYOUT_PAYLOAD" "$BAZZA_PLAYOUT_ADAPTER" "$BAZZA_RTP_PAYLOAD"; do
            test -f "$file"
        done
        compose+=(-f "$audit_tools/attendee-playout/overlay.compose.yaml" -f "$audit_tools/timing-receiver.compose.yaml")
        ;;
    *) exit 2 ;;
esac
if [ "$workers" = 1 ]; then
    "${compose[@]}" up -d --no-deps --scale worker=1 worker
else
    "${compose[@]}" up -d --no-deps --no-recreate --scale "worker=$workers" worker
fi
