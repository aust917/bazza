# Isolated Bazza Attendee ingress audit

This standalone Cargo package is a snapshot of Bazza's `bazza-attendee` crate.
The diagnostic executable uses its real TLS, WebSocket, session adoption and WAV
writing paths. Only this snapshot emits diagnostics. It does not start or build
the desktop application and does not open an existing library.

Build with the existing Rust and Visual C++ toolchain, pointing the target at
this directory or another disposable target.

    cargo build --offline --manifest-path Cargo.toml --bin bazza-ingress-audit

Run the local synthetic test with the cached Python `cryptography` and
`websockets` packages.

    python -B fixture.py target/debug/bazza-ingress-audit.exe

For a live run, prepare a private JSON config containing the following fields.
Use a new absolute run directory whose parent already exists. Keep the config,
PEM files, meeting URL, and output in local private storage.

    {
      "api_url": "http://127.0.0.1:8080",
      "callback_url": "wss://example.invalid:9443/audit/audio",
      "bind_address": "0.0.0.0:9443",
      "certificate_path": "E:/private-audit/callback.pem",
      "private_key_path": "E:/private-audit/callback-key.pem",
      "meeting_url": "https://meet.google.com/abc-defg-hij",
      "max_seconds": 600
    }

Load the API key into `BAZZA_AUDIT_ATTENDEE_API_KEY` in the executable's
environment without printing it. Supply `--confirm-room` only when this is an
approved closed test room. The flag is an explicit operator acknowledgment.

    bazza-ingress-audit.exe --live E:\private-audit\run-01 E:\private-audit\config.json --confirm-room

When `connected.signal` appears, the bot is admitted and the callback is live.
Send only approved test audio. To exercise the pre-adoption gate, send a short
test clip while the bot is connected, then create an empty `adopt.signal`. Wait
for `adopted.signal` before sending the recording clips. Create `stop.signal`
to request leave and finish. Admission, adoption, recording and finalization
have separate deadlines. Recording defaults to 600 seconds after adoption,
with an allowed range of 1 to 3,600 seconds. A nonzero exit means the trace or
session lifecycle failed.

`ingress.jsonl` records only event labels, local monotonic microseconds, clock
anchors, numeric packet sequence, byte counts and timestamps. It excludes audio
payloads, URLs, keys, raw errors and participant identifiers. `raw_text` and
`raw_binary` count complete authenticated WebSocket messages after TLS and
protocol framing. `accepted_pcm` means envelope parsing succeeded.
`queued_ingress`, `rejected_bot`, `queued_writer` and `writer_consumed`
identify later boundaries. The last event reports dropped **audit** events
separately from audio queue rejections. Correlate stages by packet number and
clock rather than line order across threads.

After adoption, the unmodified WAV writer saves separate tracks and its usual
participant manifest under the fresh run directory. Those are test media and
should be handled separately from the metadata-only trace.
