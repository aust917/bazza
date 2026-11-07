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
| Pure Rust logic | `cargo test -p bazza-calendar -p bazza-diarize -p bazza-store` | No whisper/llama/sherpa engines; still needs Rust/C toolchain |
| One crate | `cargo test -p bazza-detect` (replace crate) | Native/platform cost depends on crate |

UI tests use Vitest + jsdom with setup in `app/src/test-setup.ts`. Tests are beside
their implementation. Rust unit tests live in source modules; integration fixtures
live in crate `tests/` directories. Keep regression tests at the smallest useful boundary.

For long-input UI, IPC event storms, streamed audio and bounded retrieval, use the
[performance regression suite](performance-testing.md). `pnpm test:performance`
runs its UI checks; the guide lists the separate native checks and coverage limits.

For keyboard navigation, focus, pending actions and recoverable errors, run
pnpm test:ux. The [UX testing guide](ux-testing.md) separates automated
interaction contracts, computer-use walkthroughs and testing with real users.

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

For an isolated native development run, set `BAZZA_DATA_DIR` to a nonempty absolute
path in that process's environment. It bypasses the normal library and `data-dir.txt`
without changing either. Invalid values stop startup instead of opening the normal
library. A simultaneous second GUI also needs an alternate Tauri identifier and
WebView profile in a local, ignored test configuration; changing the data directory
alone does not bypass single-instance coordination. Do not run recording smoke mode
against the user's normal library.

### Windows portable

Refresh the portable build after application fixes. The desktop shortcut
**Bazza Portable** always points to the launcher in `%LOCALAPPDATA%\Bazza-Portable`.
Build a standalone CUDA runtime with its sidecars and DLLs, then publish the
verified staged directory and `cuda-runtime.json` with the local updater.

```powershell
pwsh -NoProfile -File scripts/update-portable.ps1 -Runtime <staged-runtime-directory>
```

The launcher uses one portable library and WebView profile under `profile/`.
The first launch of a new build clears the previous portable library and WebView
data after the old app has exited. Reopening the same build uses the current
portable library. Confirm first-run onboarding when handing over a new build.

Discord and Google Meet configuration files stay at the same portable library
path across resets. Windows Credential Manager keeps their credentials at that
path. The launcher also preserves any Attendee TLS files referenced within the
portable profile. The managed Attendee WSL installation lives outside the profile
and Bazza starts it as needed when connecting to Meet. Set up each integration
once in portable to reuse it on future builds.

The verified `ggml-large-v3-turbo.bin` stays in the persistent model cache at
`%LOCALAPPDATA%\Bazza-Review\model-cache`. The launcher verifies it against the
catalog values recorded at publish time and links it into the portable library.
Bazza initializes both recognition model selections to `whisper-large-v3-turbo`
in that profile. Onboarding, language detection, and automatic cleanup and
summary start with their factory defaults.

The installed edition uses the installer and its own library. Build the
installer and launch the installed edition only when the user requests each
action. A portable update leaves a running instance available to finish its
work. Close the old instance before opening the latest build from the shortcut.

## Rust checks matching CI

CI prepares empty target-named sidecar placeholders and creates
`src-tauri/resources/nemo-speech/` before compiling the workspace tests, including
the `bazza` desktop harness. Follow the preparation step in [CI](../../.github/workflows/ci.yml)
on a fresh checkout. Existing real sidecars can also satisfy this step.

```powershell
cargo test --locked --workspace --no-run
node scripts/stage-runtime-libs.mjs --profile debug
cargo test --locked --workspace
cargo clippy --locked --workspace --exclude bazza --all-targets -- -D warnings
```

`--no-run` compiles tests before staging. The staging script copies llama/sherpa
DLLs or dylibs beside test binaries; cached builds may otherwise fail at runtime
even when compilation succeeds. Staging alone does not build or test anything.

**Custom target directory:** both `stage-runtime-libs.mjs` and `prepare-bundle.mjs`
honor `CARGO_TARGET_DIR`, `--target <triple>` and Cargo's dev/debug output mapping.
The test staging script keeps Nemotron below each selected output directory in
`nemo-speech/0.3.0/<triple>/`, separate from top-level llama/sherpa libraries.

Use real sidecars for local app runtime validation.

## Desktop development and native build

```powershell
node scripts/prepare-bundle.mjs --profile dev
pnpm tauri dev
```

Preparation downloads ffmpeg, builds `bazza-diarize-cli` and `bazza-llm`, and collects
runtime libraries into `src-tauri/binaries/` and `src-tauri/resources/`. Cargo profile
`dev` outputs to `target/debug/`. Keep generated binaries/resources out of commits.
FFmpeg handles encoding and monitor capture.
It also prepares the pinned NeMo-Speech.cpp CPU runtime in a separate
`resources/nemo-speech/0.3.0/<target>/` tree. Keep that entire tree, including its
licenses, isolated from llama/Whisper libraries with the same basenames. Nemotron
GGUF weights are a separate catalog download, not part of the native runtime.

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
$env:BAZZA_TEST_MODEL = 'C:\models\ggml-base.en.bin'
cargo test -p bazza-asr --test transcribe_fixture -- --ignored
$env:BAZZA_TEST_LLM = 'C:\models\your-instruct-model.gguf'
cargo test -p bazza-llm generates_pong -- --ignored
$env:BAZZA_EMBED_MODEL = 'C:\models\bge-small-en-v1.5-q8_0.gguf'
cargo test -p bazza-llm real_model_embeds_similar_texts_closer -- --ignored
$env:BAZZA_TEST_NEMO_RUNTIME = 'C:\runtime\nemo-speech\0.3.0\x86_64-pc-windows-msvc\bin'
$env:BAZZA_TEST_NEMOTRON_MODEL = 'C:\models\Nemotron-3-Diarization.q8_0.gguf'
$env:BAZZA_TEST_NEMOTRON_WAV = 'C:\fixtures\conversation-16k-mono-pcm16.wav'
cargo test -p bazza-diarize-cli real_cpu_runtime_processes_existing_audio_and_resets_between_jobs -- --ignored
$env:BAZZA_TRACK_WAV = 'C:\fixtures\participant-track-16k-mono.wav'
cargo test --release -p bazza-asr --test speech_windows_track -- --ignored --nocapture
```

The last fixture prints the speech windows and raw/trimmed Whisper captions that
the post-call combined transcript would see for one saved participant track.

The Nemotron fixture is opt-in and checks model/stream reset, repeated jobs and
native cleanup against an existing local 16 kHz mono PCM16 recording. It does not
record audio. On macOS the runtime path ends in `lib`. The pinned v0.3.0 library
family stays loaded until sidecar process exit because unloading after inference
can crash its CPU/OpenMP runtime; streams and model allocations are still destroyed.

Replace example paths with installed models. Native runtime libraries must also be
available; build/stage the relevant test binaries first. Report model, backend and
OS when describing inference results; a model test is not a general quality benchmark.

### Saved voice-profile corpus

The [Russian Soniox corpus guide](test-plans/2026-11-02-soniox-voice-corpus.md)
locates the machine-local six-speaker samples, podcasts and stress variants and
owns their reuse procedure. Reuse saved audio for regression checks. Enroll only
the four known speakers in a disposable library; two voices are deliberately
unknown. Generation/audio validation and native Bazza recognition acceptance are
separate evidence. Audio, sample text, credentials and model output stay outside
tracked source; the guide records actual completion and remaining coverage.

## Recording smoke test

The built app's smoke mode **records eight seconds of real audio using current audio settings**,
saves a meeting in the normal Bazza data directory and processes it with available
models. It can include video with `BAZZA_SMOKE_VIDEO=1` when exactly one screen is available.
It prints transcript text. Run intentionally with suitable test audio and
a disposable library; changing the app identifier alone does not isolate its data.
After preparing/building the release app:

```powershell
$env:BAZZA_SMOKE = '1'
$env:BAZZA_SMOKE_CHAT = '1'
try { & .\target\release\bazza.exe }
finally { Remove-Item Env:BAZZA_SMOKE, Env:BAZZA_SMOKE_CHAT -ErrorAction SilentlyContinue }
```

On macOS, run the built `bazza` executable with the same variables. A plain dev
`cargo build` expects the Vite server and is unsuitable for standalone smoke runs.
Inspect output and exit status: success does not guarantee every optional model stage ran.

## Website

```powershell
pnpm --dir site install --frozen-lockfile
pnpm --dir site build
```

The Astro site has separate dependencies/lockfile. Root UI checks do not build it.
