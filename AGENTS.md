# Hark: agent entry point

Hark is a local-first meeting recorder: React/TypeScript UI, Tauri 2 orchestration,
and a Rust workspace. Windows and macOS are the supported desktop targets.

## Find only the context you need

Start with the task and `git status --short`. Read the scoped `AGENTS.md` before
editing in that area; some clients only auto-load instructions along their startup
directory. Open supporting documents only when relevant, rather than reading all docs.

| Task | Start here | Scoped instructions |
| --- | --- | --- |
| UI, routes, windows, typed IPC | `app/src/` | [app/AGENTS.md](app/AGENTS.md) |
| Commands, recording lifecycle, pipeline | `src-tauri/src/` | [src-tauri/AGENTS.md](src-tauri/AGENTS.md) |
| Storage, audio, detection, models, AI, sharing | `crates/hark-*/` | [crates/AGENTS.md](crates/AGENTS.md) |
| Native sidecars and packaging | `scripts/`, `.cargo/`, `.github/workflows/` | [scripts/AGENTS.md](scripts/AGENTS.md) |
| Public website (separate Astro package) | `site/` | [site/AGENTS.md](site/AGENTS.md) |
| Cross-layer navigation or development docs | [Development index](docs/dev/README.md) | Read the relevant page only |

## Product boundaries

- Detection offers a picker; recording, speaker naming and sharing remain user actions.
- Keep meeting data local by default. Network features are explicit settings/actions;
  see [privacy](docs/privacy.md) when changing a data boundary. No telemetry or accounts.
- Keep original transcript text; cleanup belongs in the separate cleaned representation.
- Keep reusable logic in Tauri-free crates, app orchestration in `src-tauri`, and UI
  commands/events in `app/src/lib/ipc.ts`. Preserve Windows/macOS platform guards.
- Do not commit recordings, transcripts, models, secrets, native build output or agent
  session state. Preserve the MIT license and upstream attribution.

## Verify the changed boundary

Commands run from the repository root. JavaScript uses pnpm 10 and Node 22+.

| Change | First useful check |
| --- | --- |
| Docs / instructions | `node scripts/check-docs.mjs` and `git diff --check` (no install needed) |
| UI / TypeScript | `pnpm typecheck`; targeted Vitest test, then `pnpm test` as needed |
| Rust crate | `cargo test -p <crate>` |
| Website | `pnpm --dir site build` |

Use [testing.md](docs/dev/testing.md) for setup, CI parity, native libraries and
model-backed tests. Native workspace builds are expensive; documentation changes
do not need them. Recording smoke mode captures real audio into the user's library.
`pnpm docs:check` is an alias for the direct Node documentation check.

For multi-session work, use the compact plan/handoff convention in
[agent-workflow.md](docs/dev/agent-workflow.md). Small edits need no plan file.
Update the owning documentation when changing a documented command or boundary.
Report what changed, checks actually run, and any remaining limitation.

Before distributing an independent build, consult [fork.md](docs/dev/fork.md):
upstream update signing, app identity and data paths are still inherited.
