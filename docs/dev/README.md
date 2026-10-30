# Development index

Use [AGENTS.md](../../AGENTS.md) for the short entry point. Follow one row below
for the task at hand; no full-documentation read is required.

| Need | Read | Source of truth |
| --- | --- | --- |
| Component boundaries and recording flow | [Architecture](architecture.md) | `crates/`, `src-tauri/src/`, `app/src/` |
| Setup, focused checks, native/model tests | [Testing and development](testing.md) | `package.json`, crate manifests, CI |
| Context management, parallel work, handoff | [Agent workflow](agent-workflow.md) | Root and scoped `AGENTS.md` |
| Fork maintenance, independent release setup | [Fork guide](fork.md) | Tauri config, storage paths, release workflow |
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
