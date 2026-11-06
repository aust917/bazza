# Attendee tracks for Google Meet

Status: complete

Add an optional Google Meet bot to desktop recording. The user configures an Attendee instance and enables it in the picker for a specific Meet link. Bazza stores participant WAVs and their names locally and lists the tracks on the completed meeting.

- [x] Verify bot creation, authenticated audio receipt, distinct participant IDs, pause, and stop using a local fixture.
- [x] Verify the picker and completed meeting views in frontend tests and compile both desktop recording entry points.
- [x] Document configuration, data boundaries, and remaining runtime limits.

The [live Attendee pilot](../test-reports/2026-11-03-attendee-google-meet-pilot.md) established separate participant audio and intelligible live speech. Fixture verification covers the new Bazza integration. Attendee supplies server timestamps, so track placement remains approximate. The existing capture baseline is documented in the [Windows acceptance report](../test-reports/2026-11-02-capture-device-acceptance.md).

Windows checks covered `cargo check -p bazza --offline`, `cargo test -p bazza --lib attendee_tracks --offline`, `cargo test -p bazza-attendee --offline`, `pnpm typecheck`, and all 197 frontend tests. The local HTTP and WSS fixture exercised two tracks, rejected a forged callback, and confirmed bot removal when stopping during bot creation. The Attendee worker image also reached a Windows TLS listener from the WSL 2 Docker network with CA and hostname verification. The [setup guide](../attendee-integration.md) describes the route and current limits.
