# One-click Google Meet setup

Status: complete

Let a new Windows x64 user provision the Attendee worker on the same computer from **Settings → Google Meet**. Keep manual configuration available for macOS and separately managed servers. The user still enables the bot for each recording and admits it if Google Meet requests approval. [Setup guide](../attendee-integration.md), [privacy boundary](../../privacy.md).

## Acceptance criteria

- A supported Windows x64 machine can configure Attendee with the setup button. Bazza downloads the software, configures Docker in its own WSL 2 environment, and generates the credentials and TLS callback automatically.
- If WSL 2 needs to be enabled, Windows prompts for administrator approval and Bazza indicates when a restart and retry are required.
- A subsequent Google Meet recording starts the local worker as needed and uses the current WSL callback route. Verify warm startup and the worker's TLS connection independently of Meet admission. The existing recording integration handles the named participant WAVs.
- The advanced manual path remains available, and automatic setup leaves an existing manual connection in place until the user disconnects it.

## Verification

The Windows 11 x64 test imported an isolated Ubuntu WSL 2 distribution and built the pinned Attendee image with no existing pilot data. The first run exposed a worker health check that exited before Docker Compose finished writing. After correcting that check, setup reused the downloaded layers, authenticated to the loopback API and returned fresh callback TLS files. A second test initialized an empty database and generated new credentials with the corrected provisioner and the cached image.

Starting Attendee after terminating the test distribution took about 10 seconds and preserved the API key. The managed worker connected to a Windows test listener with TLS 1.3 and exchanged WebSocket messages using its configured CA and callback hostname. The [live Attendee pilot](../test-reports/2026-11-03-attendee-google-meet-pilot.md) covers participant audio. Windows already had WSL 2 enabled, so the first-time administrator prompt and restart still need a clean Windows VM to verify.

`pnpm typecheck`, all 198 UI tests, `cargo check -p bazza --offline`, and the two `bazza-attendee` tests passed. The local Windows installer is built with `pnpm tauri build --bundles nsis`.
