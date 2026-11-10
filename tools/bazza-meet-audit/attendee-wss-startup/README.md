# Attendee WSS startup candidate

This candidate targets Attendee `eaacec16f2939b595f196b3957a717bbb0ed23e9`. It starts the configured Google Meet per-participant WSS client when the bot begins joining, immediately before the adapter starts. Scheduled bots start WSS when their state becomes `JOINING`. Mixed audio and video keep their own lazy startup unless they share the per-participant endpoint. The ordinary first-frame fallback remains.

## Stage an isolated A/B worker

Use an unmodified copy of the pinned source and choose a fresh output directory for each variant.

```powershell
python -B tools/bazza-meet-audit/attendee-wss-startup/stage_worker.py baseline <pinned-source-dir> <new-baseline-dir>
python -B tools/bazza-meet-audit/attendee-wss-startup/stage_worker.py candidate <pinned-source-dir> <new-candidate-dir>
```

The baseline keeps lazy WSS startup and the candidate enables prestart. Both contain the same numeric browser and provider audit. Each output has `browser-audit.js`, `provider-overlay`, `variant.json` and `worker.compose.yaml`. Use the generated `worker.compose.yaml` as the last Compose override for the disposable worker. Set `BAZZA_AUDIT_ROOT` to the absolute path of its output directory as seen by the Docker host. Pass the generated override in place of the original audit Compose override. Both variants mount the same six targets read-only and fail if a source file is missing. The pinned manager is mounted in baseline and its patched copy in candidate.

The staging helper checks all required upstream hashes and parses the resulting Python and JavaScript before writing. Its offline worker fixture verifies both mount sets, the numeric counters and the candidate cancellation path.

```powershell
python -B tools/bazza-meet-audit/attendee-wss-startup/test_worker.py <pinned-source-dir>
```

## Check the startup change by itself

```powershell
python -B tools/bazza-meet-audit/attendee-wss-startup/test_startup.py <pinned-source-dir> --candidate
python -B tools/bazza-meet-audit/attendee-wss-startup/patch.py <pinned-source-dir> <new-output-dir>
```

The fixture executes the pinned Python classes with fake socket and thread timing. It forces audio before and during the original handshake, confirms the candidate can complete the handshake before audio starts, and exercises a candidate handshake that still finishes after audio starts. It also forces cleanup while a connection is in progress. The candidate leaves that client stopped and closes a socket that returns after cleanup.

Use a trusted receiver because it can observe a connection from a bot that never joins. The candidate keeps the upstream drop behavior while disconnected and leaves Bazza ingress and its bot adoption unchanged. It helps when the handshake fits within the time from adapter startup to first PCM. A slow, failing or reset handshake can still lose audio. The fixtures cannot attribute a historical missing-audio incident.

## Proposed managed Windows delivery

[managed-provision-wss.patch](managed-provision-wss.patch) is a one-file diff against `scripts/attendee-managed/provision.sh`. Apply it only as a source change after review. It embeds the pure, pinned worker patch so the desktop launcher can keep shipping the single existing provisioner. Neither the numeric audit nor the QA relay is included.

This patch is retained as an audit artifact from the isolated checkout. It does not apply to the consolidated `main` provisioner as of 2026-11-10; rebase and validate it against the current provisioner before use. Consolidating the audit tools does not apply this proposal.

`Setup` and `Start` both verify the overlay after the base image is available. First use runs the image's Python in a temporary container with no network and a read-only root filesystem. It validates the three upstream file hashes and stages the exact tested replacements. Compose mounts those three files read-only in the worker and keeps its configuration and CA mount. The versioned mount path makes Compose recreate the worker on first rollout. Later starts reuse hash-verified files; a repaired overlay recreates only the worker. Setup and overlay changes check that Celery responds. The existing Windows callback check remains in Setup.

The proposal reuses the base image for both modes. The managed database and credentials retain their existing setup flow. A warm rollout should run while the managed recorder is idle because adding the mount recreates its worker. This path applies to the managed Windows installation. The server operator owns provider changes for manual or external Attendee installations, including on macOS.

```powershell
git -C <Bazza-checkout> apply --check --whitespace=error-all <managed-provision-wss.patch>
python -B tools/bazza-meet-audit/attendee-wss-startup/verify_provision_patch.py <Bazza-checkout>/scripts/attendee-managed/provision.sh <pinned-source-dir> <disposable-fixture-root> --bash <local-bash>
```

The offline check runs Bash syntax and simulates first staging, verified warm reuse and repair. It executes the embedded Python on the pinned source, checks byte equality against the tested overlay and verifies that input drift is rejected before writing files. The actual provisioner and Docker rollout require a separate installation acceptance run. The [live audio results](../live-results.md) are from the isolated worker, with baseline playback still pending on the same setup.
