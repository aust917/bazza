# Selected speech-to-text providers

Local Whisper remains the default for live and final transcription. Soniox is
opt-in: Settings requires a saved key, explicit consent and provider selection.
New profiles detect the speech language automatically. Existing profiles retain
their saved speech language, and the active choice appears before recording.
Local live Whisper detects language in each window in Auto mode. The preceding
window's text supplies context only when the user selects an explicit language.
The Soniox key lives in a separate private SQLite setting. The UI can save/clear it and
read its presence, but cannot retrieve it. This is local database storage, not an
OS keychain. Recording exports copy only whitelisted transcript provenance.

`bazza-asr::provider` defines provider, model, language, confirmed captions and
streaming/complete/incomplete status. Times use the recording's active 16 kHz mono
timeline, with pauses removed. Partials are display-only. `stt_stage.rs` persists
the chosen provider marker before opening capture devices; unreadable/missing
chosen-result storage is not permission to substitute another provider.

Selecting Local, revoking consent, or clearing the key also cancels a currently
starting/active Soniox worker. Registration and configuration changes share locks,
so revocation cannot miss a worker awaiting publication in ActiveRecording. The
network worker stops asynchronously; media and confirmed text remain. Bazza does
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

Completed live captions become the initial transcript. Local Whisper rows keep
their original text and timing, including the platform display names when using
a Meet or Discord bot. Selected bot recordings recognize only the bot's
participant streams during capture, using local Whisper. Mix/system ASR,
including hidden Soniox work, is disabled in this mode. Waiting or losing the
bot never silently changes that policy. Unknown display names retain separate
participant IDs. Full mic/system/mix media continues to be saved. Ordinary
recordings retain their selected STT behavior. Soniox confirmation
and incomplete results remain durable in the standard recording mode, including
after a stream failure.
Partials stay in the recording view. Transcript cleanup remains a separate text
representation. Automatic cleanup and summaries are off for new profiles. Users
can enable either in Settings, and previously saved choices are preserved.

Meet and Discord participant tracks supply the speaker identity for their
captions. Automatic diarization is skipped for a retained local live transcript
that already contains speaker labels. This applies to the whole transcript,
and preserves the bot's participant names.
For an anonymous local live transcript, enabled diarization can add labels such
as `Speaker 1` and `Speaker 2` to the existing rows when the required models are
available. Their IDs, original text and timestamps are preserved.

The transcript view uses the same rows during and after recording. The initial
text becomes readable when Stop completes, before optional post-processing. A
manual run accepts a local Whisper model, language and available audio source.
After a bot recording, the post-call worker creates a separate combined version.
It recognizes saved participant tracks, verifies their playback positions, and
uses uniquely aligned bot audio as references for conservative gain/delay
subtraction from analysis copies of system and microphone audio. The remaining
speech has typed source provenance and neutral source labels. No complete speech
interval is muted; original WAV files are unchanged. Ambiguous subtraction,
unmapped bot rows, missing required inputs or incomplete track metadata keep the
version partial. Partial results are available for review but are not promoted.
Successful runs are saved as independent versions and shown when ready. They
remain selectable after reopening. For participant-track versions, a click on
the text or timestamp seeks to the recording position when Bazza has matched
the segment to the recorded audio. Unmatched rows and older versions without
saved alignment display the participant-track time. The initial transcript
is archived before a complete combined version becomes the canonical projection
for library search, edits and summaries. Explicit user selections stay selected;
archived original and other alternatives are read-only. Manual corrections and
confirmed speaker changes prevent automatic replacement. Ordinary manual versions
remain view-only. An empty, partial, cancelled or failed run leaves existing text
available. The service rotates its Ready checkpoint and claim before optional
AI stages so those stages read the published combined source.
Meeting deletion removes its stored versions and STT provenance. `.bazza` v1
bundles can include a validated additive transcript-version archive, preserving
the original, alternatives, source provenance and the effective projection.
See the [implementation and acceptance plan](plans/2026-11-06-bot-only-live-and-system-supplement.md)
for the bounded algorithm and remaining real-call coverage.

Whisper final results also record provenance. Imported `.bazza` metadata uses a
whitelist and additive v1 field; ready SRT/WebVTT imports bypass inference.
See [import](../import.md) for the distinct finished-transcript path.

The protocol was checked on 2026-11-02 against the official
[WebSocket API](https://docs.soniox.com/api-reference/stt/websocket-api),
[authentication](https://docs.soniox.com/guides/websocket-authentication) and
[keepalive](https://docs.soniox.com/stt/rt/connection-keepalive) documentation.
Tests use in-memory WebSocket peers for PCM/config/end-of-stream, reconnect/replay,
authentication errors and cancellation. These are protocol/software checks.

The [2026-11-02 Windows follow-up](test-reports/2026-11-02-full-automatic-acceptance.md)
used generated speech for native Whisper and Soniox processing. Soniox live/final
captions, terminal provider failure, consent/key revocation, retained media/text
and explicit local re-transcription were exercised. Normal Stop remained
`incomplete` with an invalid audio watermark; its cause is not established.
Whisper Small also returned text on verified zero PCM. These findings remain open.
Actual transport interruption/reconnect, real-speech quality, other devices and
macOS are not accepted by those synthetic checks.
