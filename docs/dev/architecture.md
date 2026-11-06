# Architecture

[Developer index](README.md) · [Verification](testing.md)

Bazza is a Tauri 2 desktop app: a Rust core split into small crates, and a React/TypeScript UI. The crates know nothing about Tauri; `src-tauri/` wires them to IPC commands and events.

```
crates/
  bazza-domain    private value models/defaults shared without SQLite or native engines
  bazza-store     SQLite (+FTS5) schema, meetings, segments, settings; data-dir layout
  bazza-service-api/client  public schemas and separate bounded private application RPC
  bazza-core/service       fixture-only repository operations and standalone Store owner
  bazza-capture-handoff    OS artifact ownership, confirmed-caption journal and terminal seal
  bazza-detect    meeting detection: app/window patterns, debounce, audio-activity fallback, OS window enumeration
  bazza-capture   native Windows mic/endpoint/process capture, CPAL macOS audio, DSP, WAV writer, ffmpeg video helpers
  bazza-attendee  Attendee bot API, secure audio receiver and named participant tracks
  bazza-models    hardware probe, model tiers, catalog.json, resumable SHA-256-verified downloads
  bazza-asr       local Whisper, provider transcript contract, explicit Soniox streaming adapter
  bazza-diarize   pure diarization logic: turn->caption mapping, "me" detection, voice matching, cluster consolidation, slice planning/stitching
  bazza-diarize-cli  sidecar binary: sherpa-onnx (pyannote + TitaNet) over a WAV window, JSON out
  bazza-llm       llama.cpp + OpenAI-compatible backends; cleanup, summary (chunked), title, chat, follow-up email, live notes, embeddings
  bazza-summary-runner  explicit subscription CLI summaries, bounded subprocesses, cancellation and isolation
  bazza-calendar  .ics parsing and event matching
  bazza-share     LAN shares, validated .bazza bundles, SRT/WebVTT parsing, standalone HTML export
src-tauri/
  src/lib.rs          app builder, plugins, idle engine unloader (skips while `AppState::busy` > 0), orphan recovery on start
  src/state.rs        AppState: store, settings, engines, active recording, live feed, busy guard
  src/recorder.rs     start/stop/pause, stream reopen on device loss, orphan recovery
  src/pipeline.rs     post-call pipeline: mux -> transcribe (parallel with sharded diarization) -> cleanup -> chunks/embeddings -> summary
  src/processing_jobs.rs one FIFO worker for heavy meeting pipelines, duplicate-ID exclusion, capture lifecycle admission
  src/asr_pick.rs     per-machine measured whisper speed; picks the model that fits the recording on CPU
  src/live_feed.rs    captions + running notes for the recording in progress
  src/stt_stage.rs    explicit STT selection, private credential commands, provider provenance
  src/summary_stage.rs configured LLM summaries and explicit subscription-agent summaries
  src/import.rs       media -> pipeline, or media + finished subtitles -> atomic ready meeting
  src/detector_loop.rs 2 s poll -> `detection` event + popup window (never records)
  src/tray.rs, windows.rs, shortcuts.rs
  src/commands/*.rs   IPC surface (meetings, recording, settings, models, devices, ai, speakers, organize)
app/
  src/main.tsx, App.tsx  React entry point, main hash router and secondary-window selection
  src/lib/ipc.ts      typed application commands + events; see native API exceptions below
  src/lib/mock.ts     sample data outside Tauri (plain browser development)
  src/routes/*        Onboarding, Library, RecordingSetup, Meeting (transcript / summary / chat, live view while recording), Voices, Ask, Settings
  src/components/*    Transcript, SpeakerChip, ChatPanel, LiveView, Player, ShareDialog, ...
  src/lib/export.ts, talktime.ts   pure helpers (txt/srt/md rendering, talk-time shares) with unit tests
  src/windows/*       Popup (detected call prompt) and RecordBar (`index.html?window=...`)
scripts/
  prepare-bundle.mjs, stage-runtime-libs.mjs  sidecars and native runtime libraries
site/                 separate Astro website with its own package and lockfile
```

`app/src/lib/ipc.ts` centralizes application command invocation and event
subscriptions. Existing native API exceptions are dialog/opener plugins in
routes and share components and `convertFileSrc` in the player. The local
`app/src/lib/updates.ts` returns no update without contacting a server.
Browser mock previews do not validate recording,
persistence, or Rust IPC.

The [MCP implementation](plans/2026-11-02-mcp-implementation.md) is still gated
to explicit synthetic libraries. Its separate fixture GUI uses RepositoryClient;
ordinary routes retain AppState/Store until full worker/data cutover. Current
standard `Store::open(bazza.db)` holds the same canonical lifetime owner lock as
service constructors. Any service identity entry blocks legacy opens before
SQLite, including when the service has exited. Native Store/AppState setup runs
after single-instance plugin admission, so duplicate launch can still focus the
existing window. Historical binaries that ignore this interlock require the
planned stopped-owner cutover. A library copy reserves its destination owner and
does not copy root-level lock/discovery/broker coordination files.

## Recording data flow

The optional Google Meet path runs alongside native capture. The picker selects a
meeting URL for the Attendee bot. The Rust receiver writes supplemental WAVs and
`attendee/tracks.json` under the local meeting directory. Attendee reports the
participant identity and approximate packet times. [Google Meet integration](attendee-integration.md).

1. `detector_loop` matches visible windows against `patterns.toml` (or audio activity) -> `detection` event -> popup. Manual starts from Record now, the idle hotkey, or the tray open `/recording` in the main window. For video, both pickers select one whole display. They select the sole display automatically and require a choice when several are connected. The recorder checks the selection before starting. Meet and Discord modes add a bot destination alongside the local capture options.
2. User checks the audio sources and clicks Record. `bazza_capture::Recorder` opens the mic and, when system sound is enabled, a loopback for the selected or default output. The output captures everything played through that device independently of the display. Streams resample to 48 kHz in their callbacks. A worker writes separate WAVs and uses `buffered_mixer.rs` to retain unmatched portions before writing `mix.wav` and emitting levels and 16 kHz PCM. Excessive source lag uses a bounded fallback. Separate tracks retain accepted input. The recorder retries a missing or failed source independently. It keeps an explicitly selected output and follows default-device changes when using the default. On Windows, successful loopback polls count as healthy even in silence. Recovered system samples join the existing timeline, and the recording bar reports source loss and reconnection. Stream teardown uses the existing platform-specific path.
3. 16 kHz PCM feeds the selected STT worker. Local Whisper remains the default: a growing utterance buffer is re-transcribed every 2 s, committed on a pause or at 15 s. Empty or exactly zero PCM bypasses Whisper inference; nonzero quiet speech retains the normal path. Opt-in Soniox uses a bounded queue/replay buffer and saves only confirmed final captions with provider provenance; network failure never owns or stops the local writer. Nonterminal progress cannot acknowledge unsent samples. Terminal `finished` confirms an explicitly closed, fully sent EOS stream after error/timestamp validation; its processing statistic is not a replay ACK. See [Soniox finalization](test-reports/2026-11-02-soniox-finalization.md). Captions go to the record bar and `live_feed`, whose notes loop keeps the configured LLM path.
4. Optional ffmpeg sidecar records the screen to a fragmented MP4 (`screen.raw.mp4`), tied to the app with a kill-on-close job object. Capture remains continuous during pause; durable `screen.raw.timeline.json` records the actual audio transitions. Final mux removes these intervals, re-encoding only paused video; without pauses it copies the video stream. The audio duration caps output without truncating audio when video ends earlier. New Windows recordings use `mix.clock.json` and the first gdigrab PTS in `screen.raw.clock.txt` to align origins. `capture_environment.rs` scopes display/system power requests to video; a watchdog checks desktop/process availability and a bounded stderr reader preserves capture diagnostics. Clock-less/macOS video keeps the earlier timing path. Failed processing retains raw/metadata; see [sync fix](test-reports/2026-10-31-sync-fix.md) and [Windows clock/health acceptance](test-reports/2026-11-01-capture-acceptance.md) for measurements and platform limits.
5. Stop (off the main thread) -> `pipeline::post_process`: mux video; retain completed live text or recognize the saved audio when needed. Enabled local diarization adds labels to anonymous transcripts while preserving existing row IDs, text and timing. Local live transcripts with speaker labels, including names from Meet or Discord participant tracks, skip automatic diarization for the whole transcript. See [speaker attribution](stt.md). Map anonymous clusters onto segments and suggest only compatible known voiceprints; then cleanup, search indexing, configured LLM summary/title and enabled webhook. Explicit local re-transcription can replace cloud text while retaining prior provenance. Agent summaries run through a separate user action and survive automatic refreshes; see [runner boundary](summary-agents.md). Input/read/store failures can fail a meeting; optional AI stages retain available data.
6. On launch, meetings still in `recording` or `processing` from a previous session are finished from the files on disk. Recovery strictly decodes original and temporary video before publication; a retained invalid MP4 is not exposed as playable video by IPC or bundle export. Normal clean Stop avoids the additional full recovery decode.

After a completed meeting with participant tracks, the user can request a
separate transcript from those tracks. Local Whisper reads each participant's
audio, then Bazza saves the result beside that provider's track manifest. This
transcript uses the bot's timeline. The regular player and transcript keep using
the computer recording. [Participant transcript plan](plans/2026-11-04-bot-transcript-experience.md).

Selected bot recordings persist a recording-local source policy before opening
devices. Live ASR uses participant streams only. `bot_supplement.rs` performs
post-call source-aware recognition on bounded WAV windows; known bot references
may be removed from analysis copies using `bazza-capture::reference_residual`.
Uncertain matches remain partial, never an automatic full publication. Complete
combined versions atomically archive the initial transcript and update the
canonical projection used by search and AI. Managed publication rebinds the
existing Ready checkpoint to the registered combined source and requires a new
worker claim. Raw media and prior text remain available. See the
[bot-only and supplement plan](plans/2026-11-06-bot-only-live-and-system-supplement.md).

Heavy meeting pipelines share one FIFO worker across recovery, import, Stop and
explicit reruns. A meeting ID cannot be queued twice or deleted while its job is
reserved. New jobs wait during capture startup, active recording and Stop joins;
an already running job is not preempted by a new recording. Pipeline publication
updates only processing fields, preserving later user edits. File preparation
reads WAV in bounded blocks; diarizer processes read their assigned ranges, and
track RMS does not retain the full PCM. See the
[performance verification](test-reports/2026-11-02-performance-fixes.md) for measurements
and remaining live-ASR/initial-render limits.

Windows microphone and output-endpoint packets use one Unix origin plus native device frame positions;
per-packet QPC jitter must not insert/trim continuous PCM. Native gaps/overlaps,
silence flags and SRC history are handled before archival. Windows input capture
uses the selected capture endpoint without LOOPBACK and retains device-default
buffer duration. It follows default-input changes only when no fixed input was
selected. macOS keeps CPAL input. Virtual process loopback may report a constant
zero native position; it uses delivered frame order and marks timing estimated
instead of dropping real packets as duplicates. See the
[fidelity diagnosis/fix](test-reports/2026-11-01-system-audio-fidelity.md) for the
earlier system-audio failure and [capture reliability](test-reports/2026-11-02-capture-reliability.md)
for microphone/recovery/per-app verification and remaining coverage.

`src-tauri/src/video_diagnostic.rs` persists bounded, versioned processing
diagnostics in each recording's `processing-diagnostics.json`. Video warnings are
independent of typed optional-stage failures and deferred missing-model retries.
The readable database error is a projection restored at startup; AI actions cannot
clear an unresolved video warning. The sidecar is local, excluded from bundle
media allowlists, and removed with the recording directory.

## Persistence and packaging boundaries

`crates/bazza-store/src/db.rs` selects the platform's `Bazza` data folder,
optionally following its own `data-dir.txt` pointer. The root holds `bazza.db`,
`models/`, `recordings/<meeting-id>/`, and managed `voice-samples/`. Changing the Tauri app identifier does
not isolate this data. Ordered migrations use SQLite `user_version`; raw
transcript text is retained separately from `clean_text`.

Migration v8 adds `clean_text_manual`. Ready-only, meeting-scoped
`set_transcript_correction` changes this cleaned overlay, including an intentional
empty string, or explicitly resets it with null. Raw text, times and provider
captions remain unchanged. Automatic cleanup preserves manual rows and uses them
as context. FTS/chunks update atomically; embeddings are refreshed off the UI
thread with writes guarded against stale chunk text. A missing embedding model
does not undo a saved edit. Explicit re-transcription replaces segment rows and
their overlays after the UI explains that effect. See the
[correction plan](plans/2026-11-02-transcript-corrections.md) for validation scope.

Bundle import stages approved media files, then installs them with one metadata
transaction and an interrupted-import marker. Finished SRT/WebVTT imports bypass
inference. Safe selected STT provenance travels with `.bazza`; global voice memory
and private credentials do not. The v1 segment manifest includes the additive
manual flag, defaulting false when absent, and preserves empty cleaned text.
See [import](../import.md) and [voice memory](voice-memory.md).

`src-tauri/tauri.conf.json` controls identity, bundle assets, and
asset access; capabilities live in `src-tauri/capabilities/`. Packaging needs
ffmpeg and the diarization sidecar plus native AI runtime libraries.
`.cargo/config.toml` pins the portable CPU baseline. Keep CPU, CUDA, and Metal
build requirements distinct; see [verification](testing.md).

## Conventions

- Pure logic gets unit tests inside its crate; platform glue stays thin.
- Fallible UI commands use `CmdResult<T>` with typed `UiMessage` code/args and
  optional diagnostic detail; UI catalogs render the selected language.
- Events are broadcast to all windows (`app.emit`); shared payload structs live in `src-tauri/src/events.rs`, with chat payloads also in `commands/ai.rs`.
- `AppState::busy_guard()` protects active engine work from idle unloading.
- Recording, naming speakers, and creating shares require user actions. Recovery, post-processing, and previously enabled integrations may run automatically. The network surface includes model downloads, configured calendar/model endpoints, webhooks, LAN shares, opt-in Soniox and explicit agent summaries; see [privacy](../privacy.md). Automatic updates are disabled for this local build.
