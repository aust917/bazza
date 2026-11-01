# Hark: agent entry point

Hark is a local-first meeting recorder: React/TypeScript UI, Tauri 2 orchestration,
and a Rust workspace. Windows and macOS are the supported desktop targets.

## Local development only

Source changes and development documentation stay on this computer. Do not create
remote repositories/forks, push code, create PRs, or publish/deploy this fork unless
the user explicitly changes this local-only requirement. Local Git commits and
fetching upstream updates are allowed; upstream is read-only.

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
| Website source (separate Astro package) | `site/` | [site/AGENTS.md](site/AGENTS.md) |
| Product requirements, coverage and next work | [ТЗ](docs/product/requirements.md), [roadmap](docs/dev/plans/2026-10-30-local-fork-roadmap.md) | Read the relevant requirement/phase only |
| Cross-layer navigation or development docs | [Development index](docs/dev/README.md) | Read the relevant page only |

## Product boundaries

- Preserve capture behavior and defaults. Establish an application baseline before
  capture/sync changes; untested synchronization concerns remain hypotheses.
- Detection offers a picker; recording, speaker naming and sharing remain user actions.
- Keep meeting data local by default. Network features are explicit settings/actions;
  see [privacy](docs/privacy.md) when changing a data boundary. No telemetry or mandatory
  Hark account; optional external-provider access follows the user's chosen integration.
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

An independent distribution is optional future work, currently unauthorized.
[fork.md](docs/dev/fork.md) records inherited update signing, identity and data paths.
