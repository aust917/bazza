# Contributing to Hark

Thanks for helping. Hark is small on purpose; the best contributions keep it that way.

## Ground rules

1. **Manual by design.** Nothing records, names a speaker, or shares without a user click. PRs that add automation must keep an explicit confirmation step.
2. **Local only.** No telemetry, no accounts, no network calls except model downloads and optional user-configured endpoints.
3. **Cross-platform.** Windows and macOS are first-class. Keep platform code behind `cfg` and small.

## Setup

Start with [AGENTS.md](AGENTS.md) for area routing and repository boundaries, or the
[development index](docs/dev/README.md) for detailed guidance. Read only the pages
relevant to your change. See the [fork guide](docs/dev/fork.md) for independent releases.

See the README for toolchain requirements. Then:

```bash
pnpm install --frozen-lockfile
node scripts/prepare-bundle.mjs --profile dev   # ffmpeg + diarize sidecar + runtime libs
pnpm tauri dev
```

Checks depend on the area changed. Documentation needs only `node scripts/check-docs.mjs` and
`git diff --check`; UI work starts with `pnpm typecheck` and focused tests. For the
Rust workspace, the CI sequence is:

```bash
cargo test --workspace --exclude hark --no-run
node scripts/stage-runtime-libs.mjs --profile debug
cargo test --workspace --exclude hark
cargo clippy --workspace --exclude hark --all-targets -- -D warnings
```

See [testing.md](docs/dev/testing.md) for prerequisites, runtime staging, UI/site
checks and model-backed tests. Headless smoke records real microphone/system audio
into the normal library; run it intentionally with a disposable library as described there.

## Where things live

[Architecture](docs/dev/architecture.md) has the crate map. Pure logic goes in crates
with unit tests; Tauri glue stays in `src-tauri/`; UI file operations go through native commands/plugins.
For long tasks or parallel agent work use [agent-workflow.md](docs/dev/agent-workflow.md).
Historical plans live in [the archive](docs/archive/README.md).

To drive the real built app for an end-to-end check, launch it with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222` and attach with `playwright-core`'s `connectOverCDP`; the main window is the `tauri.localhost` page without `?window=`. Native dialogs cannot be driven that way - call the command (`import_recording`, `start_recording`, ...) through `window.__TAURI_INTERNALS__.invoke` instead.

Build hygiene: the CPU build must stay portable (`.cargo/config.toml` pins ggml to AVX2 - never `GGML_NATIVE`); `cargo clean -p <crate>` only clears the dev profile, add `--release` when a build-script input changed. A second variant (CUDA) should use its own `CARGO_TARGET_DIR`; `scripts/prepare-bundle.mjs` honours it and clears stale runtime libraries before collecting.

## Pull requests

- One change per PR, with a test where the logic is testable.
- Match the surrounding style (`cargo fmt`, Prettier defaults).
- Describe what you verified on which OS.

## Good first issues

Look for the `good first issue` label. Model catalog additions, detection patterns for more meeting apps, and summary templates are great starting points.
