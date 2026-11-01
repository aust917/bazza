# Development index

Use [AGENTS.md](../../AGENTS.md) for the short entry point. Follow one row below
for the task at hand; no full-documentation read is required.

| Need | Read | Source of truth |
| --- | --- | --- |
| Requested product behavior | [ТЗ](../product/requirements.md) | User requirements, R1–R9 |
| Current coverage and missing work | [Implementation status](implementation-status.md) | Source audit; runtime coverage is explicit |
| Next steps for this local fork | [Development roadmap](plans/2026-10-30-local-fork-roadmap.md) | Phases, dependencies and acceptance |
| Component boundaries and recording flow | [Architecture](architecture.md) | `crates/`, `src-tauri/src/`, `app/src/` |
| Setup, focused checks, native/model tests | [Testing and development](testing.md) | `package.json`, crate manifests, CI |
| Recording baseline before capture changes | [Test plan](test-plans/2026-10-30-recording-baseline.md), [build/tests](test-reports/2026-10-30-baseline.md), [actual recording](test-reports/2026-10-31-recording-control.md) | Measured A/V results and explicit limits |
| Mixing drift, pause mismatch and local fix | [Diagnosis](test-reports/2026-10-31-sync-diagnosis.md), [fix verification](test-reports/2026-10-31-sync-fix.md) | Source mechanisms, hardware/offline results and remaining limits |
| Capture clocks, display availability and failure handling | [Completed plan](plans/2026-10-31-capture-clocks-and-health.md), [Windows acceptance](test-reports/2026-11-01-capture-acceptance.md) | Timestamp cause/fix, real pause/idle/failure results and platform limits |
| System-audio waveform corruption found during manual acceptance | [Fidelity diagnosis/fix](test-reports/2026-11-01-system-audio-fidelity.md), [task](plans/2026-11-01-system-audio-fidelity.md) | Native frame clock, exact-sample regression, short hardware repeat; pause/recovery pending |
| Independent product work awaiting runtime acceptance | [Task plan](plans/2026-11-01-independent-product-work.md) | Implemented boundaries, checks and remaining acceptance |
| Portable bundles and finished subtitles | [Import guide](../import.md), [implementation plan](plans/2026-11-01-bundle-and-transcript-import.md) | Validation, atomic storage and external transcript handling |
| Managed voice samples and model identity | [Voice memory](voice-memory.md) | Migration, sample lifecycle and compatible matching |
| Explicit subscription-agent summaries | [Summary agents](summary-agents.md) | CLI isolation, cancellation, provenance and remaining service acceptance |
| Selected local/cloud transcription | [STT providers](stt.md) | Timeline, consent, final text, replay and provenance |
| Context management, parallel work, handoff | [Agent workflow](agent-workflow.md) | Root and scoped `AGENTS.md` |
| Local upstream integration, inherited release settings | [Fork guide](fork.md) | Read-only upstream, Tauri config and storage paths |
| Durable context design rationale | [Decision 0001](decisions/0001-agent-context.md) | The decision and its consequences |
| A long task spanning sessions | [Task plans](plans/README.md) | The relevant task file and current code |
| Original design history | [Archive](../archive/README.md) | Historical material, not current behavior |

## Common changes

- **Add a command:** follow `app/src/lib/ipc.ts`, `src-tauri/src/commands/` and
  command registration in `src-tauri/src/lib.rs`; update payload types together.
- **Change recording:** start in `src-tauri/src/recorder.rs` and
  `crates/hark-capture/src/`; post-call work starts in `src-tauri/src/pipeline.rs`.
- **Change speakers:** distinguish pure `hark-diarize` logic, native
  `hark-diarize-cli` inference, storage and the speaker UI.
- **Change persistence:** inspect `crates/hark-store/src/migrations.rs` and callers;
  preserve existing libraries and raw transcripts.
- **Change model behavior:** start in the owning `hark-asr`/`hark-llm` crate;
  model catalog/downloads belong in `hark-models`.

Search the relevant directory first, then expand as needed. Avoid loading lockfiles,
model catalogs, generated output or the archive for unrelated tasks.
