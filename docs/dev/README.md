# Development index

Use [AGENTS.md](../../AGENTS.md) for the short entry point. Follow one row below
for the task at hand; no full-documentation read is required.

| Need | Read | Source of truth |
| --- | --- | --- |
| Requested product behavior | [ТЗ](../product/requirements.md) | User requirements, R1–R9 |
| Current coverage and missing work | [Implementation status](implementation-status.md) | Source audit; runtime coverage is explicit |
| Next steps for this local fork | [Development roadmap](plans/2026-10-30-local-fork-roadmap.md) | Phases, dependencies and acceptance |
| Component boundaries and recording flow | [Architecture](architecture.md) | `crates/`, `src-tauri/src/`, `app/src/` |
| MCP access for external AI agents | [Implementation plan](plans/2026-11-02-mcp-implementation.md), [fixture adapter](../../crates/bazza-mcp/README.md), [research](research/2026-11-02-mcp-integration.md) | Read-only stdio on synthetic data. Production library access, grants and mutations are open. |
| Setup, focused checks, native/model tests | [Testing and development](testing.md) | `package.json`, crate manifests, CI |
| UI stalls, library scaling and processing memory | [Performance audit, 2026-11-02](test-reports/2026-11-02-performance-audit.md) | Prioritized source findings, production UI component benchmark, synthetic SQL/WAV measurements and explicit limits |
| Performance fixes and before/after checks | [2026-11-02 results](test-reports/2026-11-02-performance-fixes.md) | Background IPC, queued processing, bounded WAV workspace, SQL/UI regressions and actual Windows responsiveness |
| Recording baseline before capture changes | [Test plan](test-plans/2026-10-30-recording-baseline.md), [build/tests](test-reports/2026-10-30-baseline.md), [actual recording](test-reports/2026-10-31-recording-control.md) | Measured A/V results and explicit limits |
| Mixing drift, pause mismatch and local fix | [Diagnosis](test-reports/2026-10-31-sync-diagnosis.md), [fix verification](test-reports/2026-10-31-sync-fix.md) | Source mechanisms, hardware/offline results and remaining limits |
| Capture clocks, display availability and failure handling | [Completed plan](plans/2026-10-31-capture-clocks-and-health.md), [Windows acceptance](test-reports/2026-11-01-capture-acceptance.md) | Timestamp cause/fix, real pause/idle/failure results and platform limits |
| System-audio waveform corruption and later acceptance | [Fidelity diagnosis/fix](test-reports/2026-11-01-system-audio-fidelity.md), [Windows device/A/V results](test-reports/2026-11-02-capture-device-acceptance.md), [task](plans/2026-11-01-system-audio-fidelity.md) | Short/pause/stop/idle and Windows-default return verified; later stress and remaining limits are in the full follow-up below |
| Automated 720p and finished-subtitle acceptance | [Windows runtime report](test-reports/2026-11-02-automated-product-acceptance.md) | Native IPC, actual capture, A/V markers, SRT/VTT exact data, playback/seek/reopen; later chooser checks are in the full follow-up |
| Full automatic Windows acceptance follow-up | [2026-11-02 report](test-reports/2026-11-02-full-automatic-acceptance.md), [task plan](plans/2026-11-02-automatic-acceptance-completion.md) | Long capture, model/service and native import UI checks on synthetic fixtures; microphone/Soniox findings and platform/hardware limits remain explicit |
| Independent product implementation and acceptance | [Task plan](plans/2026-11-01-independent-product-work.md) | Implemented boundaries and linked verification; full product acceptance is not implied |
| Portable bundles and finished subtitles | [Import guide](../import.md), [implementation plan](plans/2026-11-01-bundle-and-transcript-import.md), [Windows package runtime](test-reports/2026-11-02-bundle-runtime-acceptance.md) | Validation, cross-library media/metadata, rollback/retry/duplicate and native playback/restart |
| Managed voice samples and model identity | [Voice memory](voice-memory.md) | Migration, sample lifecycle, compatible matching and native synthetic checks |
| Speaker recognition alternatives and short clean turns | [Technology research](../product/2026-11-02-speaker-recognition-research.md) | Encoder/diarizer shortlist, model licenses, current duration limits and proposed comparison |
| Speaker names from Discord | [Integration research](research/2026-11-03-discord-speaker-attribution.md) | Local RPC access, bot voice tracks, product examples and pilot criteria |
| Creating and assigning Discord bots | [Provisioning research](research/2026-11-03-discord-bot-provisioning.md) | Guided setup, Discord API boundaries and parallel recording options |
| Discord speaker labels during overlap | [Local probe](../../tools/bazza-discord-probe/README.md), [test plan](test-plans/2026-11-03-discord-bot-overlap.md) | Per-account voice tracks, local Whisper and controlled server acceptance |
| Google Meet bot and separate speaker tracks | [Setup](attendee-integration.md), [integration research](research/2026-11-03-google-meet-speaker-tracks.md), [Attendee pilot](test-reports/2026-11-03-attendee-google-meet-pilot.md) | Attendee connection and local WAV files, controlled live overlap, worker routing and timing limits |
| Actual speaker quality comparison before integration | [Measured bake-off](test-reports/2026-11-02-speaker-quality-bakeoff.md), [completed task](plans/2026-11-02-speaker-quality-bakeoff.md) | 17 encoder variants, real/synthetic controls, native Voices compatibility and long-stream postprocessing |
| Nemotron 3 integration with existing TitaNet voices | [Native application results](test-reports/2026-11-02-nemotron-integration.md), [task plan](plans/2026-11-02-nemotron-integration.md) | Continuous long-recording stream, model/runtime packaging, native import/Voices/capture/restart and measured quality limits |
| Saved Russian synthetic voices and podcast fixtures | [Soniox corpus guide](test-plans/2026-11-02-soniox-voice-corpus.md) | Machine-local artifacts, known/unknown enrollment, ground-truth limits and separate recognition acceptance |
| Voice matching baseline and unknown rejection | [Native report](test-reports/2026-11-02-voice-matching.md), [completed task](plans/2026-11-02-voice-matching.md) | Native before/after results, conservative abstention limits, confirmation/global invalidation and snapshot-specific restart checks |
| Explicit subscription-agent summaries | [Summary agents](summary-agents.md) | CLI isolation, cancellation, provenance and native synthetic checks |
| Selected local/cloud transcription | [STT providers](stt.md) | Timeline, consent, final text, replay and provenance |
| Whisper hallucinations on pauses and speech | [Research](research/2026-11-03-whisper-hallucinations.md) | Local Silero VAD, decoding context, available tools and a RU/EN evaluation protocol |
| System-wide dictation with local/cloud models | [Implementation plan](plans/2026-11-02-system-dictation.md), [research](research/2026-11-02-system-dictation.md) | GitHub source snapshots, Windows/macOS text insertion, independent dictation sessions and acceptance gates; planned, not implemented |
| Russian/English desktop interface | [Localization guide](localization.md), [plan and acceptance](plans/2026-11-02-russian-ui-localization.md) | Separate UI locale, cross-window events, native tray, message codes and verification limits |
| Product naming and file formats | [Bazza rename plan](plans/2026-11-02-bazza-rename.md) | Bazza-only names, `.bazza` bundles and actual verification |
| Context management, parallel work, handoff | [Agent workflow](agent-workflow.md) | Root and scoped `AGENTS.md` |
| Local upstream integration, inherited release settings | [Fork guide](fork.md) | Read-only upstream, Tauri config and storage paths |
| Durable context design rationale | [Decision 0001](decisions/0001-agent-context.md) | The decision and its consequences |
| A long task spanning sessions | [Task plans](plans/README.md) | The relevant task file and current code |
| Original design history | [Archive](../archive/README.md) | Historical material, not current behavior |

## Common changes

- **Add a command:** follow `app/src/lib/ipc.ts`, `src-tauri/src/commands/` and
  command registration in `src-tauri/src/lib.rs`; update payload types together.
- **Change recording:** start in `src-tauri/src/recorder.rs` and
  `crates/bazza-capture/src/`; post-call work starts in `src-tauri/src/pipeline.rs`.
- **Change speakers:** distinguish pure `bazza-diarize` logic, native
  `bazza-diarize-cli` inference, storage and the speaker UI.
- **Change persistence:** inspect `crates/bazza-store/src/migrations.rs` and callers;
  preserve existing libraries and raw transcripts.
- **Change model behavior:** start in the owning `bazza-asr`/`bazza-llm` crate;
  model catalog/downloads belong in `bazza-models`.

Search the relevant directory first, then expand as needed. Avoid loading lockfiles,
model catalogs, generated output or the archive for unrelated tasks.

Product identifiers in historical test reports follow current Bazza naming.
Recorded results and hashes remain unchanged; editorial renaming does not
establish acceptance of a later build.
