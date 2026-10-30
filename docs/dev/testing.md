# Testing and local development

Choose checks for the changed area. Commands below run from the repository root;
PowerShell syntax is used where environment variables are needed. Command definitions
are verified against manifests and CI; this page is not a record of a successful run.

## Cold clone prerequisites

- UI/site: Node 22+ and pnpm 10, matching `.github/workflows/ci.yml`.
- Rust: stable Rust via rustup (`rust-toolchain.toml`), a C/C++ toolchain and CMake.
- Windows native AI: MSVC Build Tools and LLVM/libclang for bindgen.
- macOS: Xcode Command Line Tools. CI covers Windows and macOS; Linux is not in its matrix.
- Full desktop runtime: prepared ffmpeg and diarization sidecars plus native AI libraries.
  These are generated locally, not committed.

Install JavaScript dependencies reproducibly:

```powershell
pnpm install --frozen-lockfile
```

Rust commands may download crates and build bundled/native dependencies on first use.
Workspace checks compile whisper.cpp, llama.cpp and sherpa-onnx bindings and are expensive.

## Small checks first

| Change | Check | Cost / coverage |
| --- | --- | --- |
| Documentation | `node scripts/check-docs.mjs` and `git diff --check` | No install or native build |
| One UI helper/component | `pnpm exec vitest run src/lib/export.test.ts` (replace path) | Vite test paths are relative to `app/` |
| TypeScript contract | `pnpm typecheck` | Type checking, no native build |
| Pure Rust logic | `cargo test -p hark-calendar -p hark-diarize -p hark-store` | No whisper/llama/sherpa engines; still needs Rust/C toolchain |
| One crate | `cargo test -p hark-detect` (replace crate) | Native/platform cost depends on crate |

UI tests use Vitest + jsdom with setup in `app/src/test-setup.ts`. Tests are beside
their implementation. Rust unit tests live in source modules; integration fixtures
live in crate `tests/` directories. Keep regression tests at the smallest useful boundary.

`pnpm docs:check` aliases the direct Node check. Prefer the direct command in a cold
clone: some pnpm versions/wrappers run dependency installation checks before scripts.

## Frontend and browser preview

```powershell
pnpm typecheck
pnpm test
pnpm build
pnpm dev
```

`pnpm dev` serves the UI at `http://localhost:1420`. Outside Tauri,
`app/src/lib/mock.ts` supplies sample data automatically. A browser preview verifies
presentation and interaction; it does not verify recording, native IPC or AI inference.
`pnpm build` writes `app/dist/`; TypeScript can update ignored `*.tsbuildinfo` files.

## Rust checks matching CI

The regular CI sequence excludes the desktop package named `hark`:

```powershell
cargo test --workspace --exclude hark --no-run
node scripts/stage-runtime-libs.mjs --profile debug
cargo test --workspace --exclude hark
cargo clippy --workspace --exclude hark --all-targets -- -D warnings
```

`--no-run` compiles tests before staging. The staging script copies llama/sherpa
DLLs or dylibs beside test binaries; cached builds may otherwise fail at runtime
even when compilation succeeds. Staging alone does not build or test anything.

**Custom target directory:** `stage-runtime-libs.mjs` currently always reads
`target/<profile>` and does not honor `CARGO_TARGET_DIR` or a cross-target triple.
Use the default target directory for this sequence, or explicitly arrange runtime
library paths for the selected build. `prepare-bundle.mjs` does honor `CARGO_TARGET_DIR`.

CI separately runs `cargo check -p hark` after creating empty target-named sidecar
placeholders. This checks compilation only. Empty placeholders cannot run the app;
use real sidecars for local runtime validation.

## Desktop development and native build

```powershell
node scripts/prepare-bundle.mjs --profile dev
pnpm tauri dev
```

Preparation downloads ffmpeg, builds `hark-diarize-cli` and `hark-llm`, and collects
runtime libraries into `src-tauri/binaries/` and `src-tauri/resources/`. Cargo profile
`dev` outputs to `target/debug/`. Keep generated binaries/resources out of commits.

For a release build without an installer:

```powershell
node scripts/prepare-bundle.mjs
pnpm tauri build --no-bundle
```

CPU is the default. CUDA/Metal builds need matching preparation/build feature flags
and platform toolchains; see the release workflow. Keep CPU/CUDA outputs in separate
target directories. Preserve `.cargo/config.toml`'s portable CPU baseline; do not
enable `GGML_NATIVE`. After native build-script inputs change, invalidate the affected
build/cache deliberately; `cargo clean -p <crate>` needs `--release` for release output.

## Model-backed fixtures

These tests are ignored by normal runs and need existing local model files:

```powershell
$env:HARK_TEST_MODEL = 'C:\models\ggml-base.en.bin'
cargo test -p hark-asr --test transcribe_fixture -- --ignored
$env:HARK_TEST_LLM = 'C:\models\your-instruct-model.gguf'
cargo test -p hark-llm generates_pong -- --ignored
$env:HARK_EMBED_MODEL = 'C:\models\bge-small-en-v1.5-q8_0.gguf'
cargo test -p hark-llm real_model_embeds_similar_texts_closer -- --ignored
```

Replace example paths with installed models. Native runtime libraries must also be
available; build/stage the relevant test binaries first. Report model, backend and
OS when describing inference results; a model test is not a general quality benchmark.

## Recording smoke test

The built app's smoke mode **records eight seconds of real audio using current audio settings**,
saves a meeting in the normal Hark data directory and processes it with available
models. It prints transcript text. Run intentionally with suitable test audio and
a disposable library; changing the app identifier alone does not isolate its data.
After preparing/building the release app:

```powershell
$env:HARK_SMOKE = '1'
$env:HARK_SMOKE_CHAT = '1'
try { & .\target\release\hark.exe }
finally { Remove-Item Env:HARK_SMOKE, Env:HARK_SMOKE_CHAT -ErrorAction SilentlyContinue }
```

On macOS, run the built `hark` executable with the same variables. A plain dev
`cargo build` expects the Vite server and is unsuitable for standalone smoke runs.
Inspect output and exit status: success does not guarantee every optional model stage ran.

## Website

```powershell
pnpm --dir site install --frozen-lockfile
pnpm --dir site build
```

The Astro site has separate dependencies/lockfile. Root UI checks do not build it.
