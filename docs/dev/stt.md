# Selected speech-to-text providers

Local Whisper remains the default for live and final transcription. Soniox is
opt-in: Settings requires a saved key, explicit consent and provider selection.
The key lives in a separate private SQLite setting. The UI can save/clear it and
read its presence, but cannot retrieve it. This is local database storage, not an
OS keychain. Recording exports copy only whitelisted transcript provenance.

`hark-asr::provider` defines provider, model, language, confirmed captions and
streaming/complete/incomplete status. Times use the recording's active 16 kHz mono
timeline, with pauses removed. Partials are display-only. `stt_stage.rs` persists
the chosen provider marker before opening capture devices; unreadable/missing
chosen-result storage is not permission to substitute another provider.

Selecting Local, revoking consent, or clearing the key also cancels a currently
starting/active Soniox worker. Registration and configuration changes share locks,
so revocation cannot miss a worker awaiting publication in ActiveRecording. The
network worker stops asynchronously; media and confirmed text remain. Hark does
not start Whisper midway through that recording: choose Re-transcribe afterward.

The Soniox worker receives a bounded queue on its own thread. It uses the fixed
Soniox WSS endpoint, bearer header authentication, float32 little-endian 16 kHz
mono audio, and the configured real-time model/language hints. It retains up to
60 seconds of unacknowledged audio for replay, rebases times after reconnect,
deduplicates already confirmed tokens, and permits three transient retries.
Handshake/write/finalization timeouts are 8/5/15 seconds per attempt; idle
connections receive keepalives every ten seconds. Backlog, authorization and
protocol failures terminate STT while local capture continues. Server error text
and credentials are not echoed into diagnostics.

Automatic post-call processing uses saved Soniox final captions even when the
stream failed or Settings has since changed. It validates their bounds against
the media and never promotes partials. Existing local diarization/cleanup remain
separate stages. An explicit Re-transcribe chooses a local Whisper model and
retains previous cloud provenance; absent local models preserve existing text.
Meeting deletion removes current and previous STT text from settings too.

Whisper final results also record provenance. Imported `.hark` metadata uses a
whitelist and additive v1 field; ready SRT/WebVTT imports bypass inference.
See [import](../import.md) for the distinct finished-transcript path.

The protocol was checked on 2026-11-01 against the official
[WebSocket API](https://docs.soniox.com/api-reference/stt/websocket-api),
[authentication](https://docs.soniox.com/guides/websocket-authentication) and
[keepalive](https://docs.soniox.com/stt/rt/connection-keepalive) documentation.
Tests use in-memory WebSocket peers for PCM/config/end-of-stream, reconnect/replay,
authentication errors and cancellation. These are protocol/software checks;
actual account/model availability, latency, quality and real interruption behavior
remain service acceptance. No Soniox request was made in this implementation task.
