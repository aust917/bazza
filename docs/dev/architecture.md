# Architecture

[Developer index](README.md) · [Verification](testing.md)

Hark is a Tauri 2 desktop app: a Rust core split into small crates, and a React/TypeScript UI. The crates know nothing about Tauri; `src-tauri/` wires them to IPC commands and events.

```
crates/
  hark-store     SQLite (+FTS5) schema, meetings, segments, settings; data-dir layout
  hark-detect    meeting detection: app/window patterns, debounce, audio-activity fallback, OS window enumeration
  hark-capture   cpal mic + system-audio loopback, Windows per-process loopback (WASAPI), DSP, WAV writer, ffmpeg video helpers
  hark-models    hardware probe, model tiers, catalog.json, resumable SHA-256-verified downloads
  hark-asr       local Whisper, provider transcript contract, explicit Soniox streaming adapter
  hark-diarize   pure diarization logic: turn->caption mapping, "me" detection, voice matching, cluster consolidation, slice planning/stitching
  hark-diarize-cli  sidecar binary: sherpa-onnx (pyannote + TitaNet) over a WAV window, JSON out
  hark-llm       llama.cpp + OpenAI-compatible backends; cleanup, summary (chunked), title, chat, follow-up email, live notes, embeddings
  hark-summary-runner  explicit subscription CLI summaries, bounded subprocesses, cancellation and isolation
  hark-calendar  .ics parsing and event matching
  hark-share     LAN shares, validated .hark bundles, SRT/WebVTT parsing, standalone HTML export
src-tauri/
  src/lib.rs          app builder, plugins, idle engine unloader (skips while `AppState::busy` > 0), orphan recovery on start
  src/state.rs        AppState: store, settings, engines, active recording, live feed, busy guard
  src/recorder.rs     start/stop/pause, stream reopen on device loss, orphan recovery, manual picker
  src/pipeline.rs     post-call pipeline: mux -> transcribe (parallel with sharded diarization) -> cleanup -> chunks/embeddings -> summary
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
  src/routes/*        Onboarding, Library, Meeting (transcript / summary / chat, live view while recording), Voices, Ask, Settings
  src/components/*    Transcript, SpeakerChip, ChatPanel, LiveView, Player, ShareDialog, ...
  src/lib/export.ts, talktime.ts   pure helpers (txt/srt/md rendering, talk-time shares) with unit tests
  src/windows/*       Popup (the call prompt and the manual picker) and RecordBar (`index.html?window=...`)
scripts/
  prepare-bundle.mjs, stage-runtime-libs.mjs  sidecars and native runtime libraries
site/                 separate Astro website with its own package and lockfile
```

`app/src/lib/ipc.ts` centralizes application command invocation and event
subscriptions. Existing native API exceptions are dialog/opener plugins in
routes and share components, `convertFileSrc` in the player, and updater/process
APIs in `app/src/lib/updates.ts`. Browser mock previews do not validate recording,
persistence, or Rust IPC.

## Recording data flow

1. `detector_loop` matches visible windows against `patterns.toml` (or audio activity) -> `detection` event -> popup. Manual starts (Record now, hotkey, tray) emit a synthetic `manual` detection so the same popup acts as the picker.
2. User picks a window and audio mode and clicks Record -> `recorder::start`: `hark_capture::Recorder` opens the mic and either a device loopback (everything) or a Windows process loopback (that app's process tree). Streams resample to 48 kHz in their callbacks; a worker writes separate WAVs and uses `buffered_mixer.rs` to retain unmatched portions before writing `mix.wav` and emitting levels + 16 kHz PCM. Excessive source lag uses a bounded fallback with a warning; separate tracks retain accepted input. A stalled or errored stream is reopened on the current device without stopping the recording; stream teardown never runs on the caller's thread.
3. 16 kHz PCM feeds the selected STT worker. Local Whisper remains the default: a growing utterance buffer is re-transcribed every 2 s, committed on a pause or at 15 s. Opt-in Soniox uses a bounded queue/replay buffer and saves only confirmed final captions with provider provenance; network failure never owns or stops the local writer. Captions go to the record bar and `live_feed`, whose notes loop keeps the configured LLM path.
4. Optional ffmpeg sidecar records the screen to a fragmented MP4 (`screen.raw.mp4`), tied to the app with a kill-on-close job object. Capture remains continuous during pause; durable `screen.raw.timeline.json` records the actual audio transitions. Final mux removes these intervals, re-encoding only paused video; without pauses it copies the video stream. The audio duration caps output without truncating audio when video ends earlier. New Windows recordings use `mix.clock.json` and the first gdigrab PTS in `screen.raw.clock.txt` to align origins. `capture_environment.rs` scopes display/system power requests to video; a watchdog checks desktop/process availability and a bounded stderr reader preserves capture diagnostics. Legacy/macOS video keeps the earlier timing path. Failed processing retains raw/metadata; see [sync fix](test-reports/2026-10-31-sync-fix.md) and [Windows clock/health acceptance](test-reports/2026-11-01-capture-acceptance.md) for measurements and platform limits.
5. Stop (off the main thread) -> `pipeline::post_process`: mux video; run sharded local diarization and either use the selected Soniox final text or the quality Whisper pass. Map clusters onto segments and suggest only compatible known voiceprints; then cleanup, search indexing, configured LLM summary/title and enabled webhook. Explicit local re-transcription can replace cloud text while retaining prior provenance. Agent summaries run through a separate user action and survive automatic refreshes; see [runner boundary](summary-agents.md). Input/read/store failures can fail a meeting; optional AI stages retain available data.
6. On launch, meetings still in `recording` or `processing` from a previous session are finished from the files on disk.

## Persistence and packaging boundaries

`crates/hark-store/src/db.rs` selects the platform's `Hark` data folder,
optionally following its `data-dir.txt` pointer. The root holds `hark.db`,
`models/`, `recordings/<meeting-id>/`, and managed `voice-samples/`. Changing the Tauri app identifier does
not isolate this data. Ordered migrations use SQLite `user_version`; raw
transcript text is retained separately from `clean_text`.

Bundle import stages approved media files, then installs them with one metadata
transaction and an interrupted-import marker. Finished SRT/WebVTT imports bypass
inference. Safe selected STT provenance travels with `.hark`; global voice memory
and private credentials do not. See [import](../import.md) and [voice memory](voice-memory.md).

`src-tauri/tauri.conf.json` controls identity, bundle assets, updater trust, and
asset access; capabilities live in `src-tauri/capabilities/`. Packaging needs
ffmpeg and the diarization sidecar plus native AI runtime libraries.
`.cargo/config.toml` pins the portable CPU baseline. Keep CPU, CUDA, and Metal
build requirements distinct; see [verification](testing.md).

## Conventions

- Pure logic gets unit tests inside its crate; platform glue stays thin.
- Fallible IPC commands use `Result<T, String>`; the UI shows the error string.
- Events are broadcast to all windows (`app.emit`); shared payload structs live in `src-tauri/src/events.rs`, with chat payloads also in `commands/ai.rs`.
- `AppState::busy_guard()` protects active engine work from idle unloading.
- Recording, naming speakers, and creating shares require user actions. Recovery, post-processing, and previously enabled integrations may run automatically. The network surface includes model downloads, update checks, configured calendar/model endpoints, webhooks, LAN shares, opt-in Soniox and explicit agent summaries; see [privacy](../privacy.md).
