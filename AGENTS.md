# Bazza: agent entry point

Bazza is a local-first meeting recorder: React/TypeScript UI, Tauri 2 orchestration,
and a Rust workspace. Windows and macOS are the supported desktop targets.

## Local development only

Keep source and docs local. The only authorized remote is the public
collaboration repo `origin` (github.com/aust917/bazza, public since 2026-11-07);
pushes go there only. Other remotes/forks, releases, publication or deployment
require an explicit user change to this rule. Never push secrets. Read-only
upstream fetches are allowed.

## Find only the context you need

Start with `git status --short`. Read the scoped `AGENTS.md` before editing in
its area, then open only relevant supporting docs.

| Task | Start here | Scoped instructions |
| --- | --- | --- |
| UI, routes, windows, typed IPC | `app/src/` | [app/AGENTS.md](app/AGENTS.md) |
| Commands, recording lifecycle, pipeline | `src-tauri/src/` | [src-tauri/AGENTS.md](src-tauri/AGENTS.md) |
| Storage, audio, detection, models, AI, sharing | `crates/bazza-*/` | [crates/AGENTS.md](crates/AGENTS.md) |
| Sidecars, packaging, clean installs | `scripts/`, `.cargo/`, `.github/workflows/` | [scripts/AGENTS.md](scripts/AGENTS.md) |
| Website source (separate Astro package) | `site/` | [site/AGENTS.md](site/AGENTS.md) |
| Product requirements, coverage and next work | [ТЗ](docs/product/requirements.md), [roadmap](docs/dev/plans/2026-10-30-local-fork-roadmap.md) | Read the relevant requirement/phase only |
| Cross-layer navigation or development docs | [Development index](docs/dev/README.md) | Read the relevant page only |

## Product boundaries

- Preserve capture behavior and defaults. Establish an application baseline before
  capture/sync changes; untested synchronization concerns remain hypotheses.
- Detection offers a picker; recording, speaker naming and sharing remain user actions.
- Keep meeting data local by default. Network features are explicit settings/actions;
  see [privacy](docs/privacy.md) when changing a data boundary. No telemetry or mandatory
  Bazza account; optional external-provider access follows the user's chosen integration.
- Keep original transcript text; cleanup belongs in the separate cleaned representation.
- Keep reusable logic in Tauri-free crates, app orchestration in `src-tauri`, and UI
  commands/events in `app/src/lib/ipc.ts`. Preserve Windows/macOS platform guards.
- Do not commit recordings, transcripts, models, secrets, native build output or agent
  session state. Preserve the MIT license and upstream attribution.

## Verify the changed boundary

Commands run from the repository root. JavaScript uses pnpm 10 and Node 22+.

Refresh the [Windows portable build](docs/dev/testing.md#windows-portable) after
app fixes. Reset the portable library, preserving Turbo and Discord/Meet setup.
Build installers, launch installed Bazza, and run other builds only on request
or after confirmation.

| Change | First useful check |
| --- | --- |
| Docs / instructions | `node scripts/check-docs.mjs` and `git diff --check` (no install needed) |
| UI / TypeScript | `pnpm typecheck`; targeted Vitest test, then `pnpm test` as needed |
| Rust crate | `cargo test -p <crate>` with approval if it will build |
| Website | `pnpm --dir site build` with approval |

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
