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

The desktop shortcut **Bazza Portable** runs the launcher in
`%LOCALAPPDATA%\Bazza-Portable` with the built-in Windows PowerShell 5.1
(`powershell.exe`); no PowerShell 7 or Codex runtime is needed. Publish a new
build only when the user asks: the portable library holds their real meetings.

The library location is `library.json` in that folder (`library` and `backups`).
`update-portable.ps1 -Library <dir> -Backups <dir>` sets it; later updates keep it.
Updates never reset, replace or move the library. Before the first launch of a
different build, the launcher snapshots the library into `backups` as
`update-<time>` and refuses to start the new build if the copy fails, since a
newer build may migrate the database forward. The launcher also refuses to start
when the library folder is missing, rather than creating an empty library.

`scripts/backup-portable-library.ps1` (published as `Backup Bazza Library.ps1`)
writes complete snapshot folders; files unchanged since the previous snapshot are
hard links, so they cost no extra space. `models\` is skipped. The Task Scheduler
task **Bazza library backup** runs `-Kind daily` once a day and keeps 14 daily
snapshots; `update-*` snapshots are kept until removed by hand. `backup.log`
lists every snapshot.

Restore: close Bazza, move the library folder aside, copy a snapshot back with
`robocopy <snapshot> <library> /E`, then copy `models\` from the moved folder.
Roll back a build by publishing an older staged runtime; if the newer build had
already opened the library, also restore the `update-*` snapshot taken before it.
To add meetings from another library (a copy, with Bazza closed), use
`cargo run -p bazza-share --example merge_library -- merge <source> <library> <tmp.bazza>`:
it goes through `.bazza` export/import and skips existing meeting IDs. Copy the
meeting folders' other files (`discord\`, clock and diagnostics) afterwards
without overwriting; packages carry only the main media.

Build and publish (Windows PowerShell 5.1, from a committed tree):

```powershell
$run = 'portable-YYYYMMDD'
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-portable-cuda.ps1 -RunName $run
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/update-portable.ps1 -Runtime ".git/local-builds/$run/runtime"
```

`build-portable-cuda.ps1` runs `Snapshot`, `Build` and `Stage` (or one of them
with `-Action`). Its preflight names any missing input. The machine-local inputs
are untracked and do not use the system CUDA (CUDA 12.8 fails with VS 2026):

- `-BuildEnv` (default `.git/local-builds/cuda1322`): CUDA 13.2.2 in `toolkit\`,
  unpacked from the NVIDIA redist archives listed with hashes in
  `scripts/cuda-toolkit-13.2.json`; `ninja\`; the reused Cargo `target\`;
  `models\ggml-silero-v5.1.2.bin`; and the verified prebuilt CUDA DLLs, ffmpeg and
  NeMo runtime in `-NativeInputs`.
- `-Tools` (default `.git/local-tools`): `rustup`, `cargo` and `pnpm-shim`.
- VS 2026 with MSVC 14.50 (`-MsvcVersion`); CUDA 13.2 rejects MSVC 19.60+.

The snapshot copies tracked files of `HEAD` and records the commit in the
staged `cuda-runtime.json` and in the published `current.json`. The build
refuses to run when `Cargo.lock` differs from the one the prebuilt CUDA DLLs were
verified with; then rebuild those native inputs first. Check the staged
`cuda-runtime.json` for `toolkit` 13.2.2 and `architecture` `sm_89`.

After launch, the running `bazza.exe` should have `ggml-cuda.dll` and
`cublas64_13.dll` loaded. Model-backed tests built with `--features cuda` in
this environment log `backend=cuda`.

Discord and Google Meet settings files live in the library. Windows Credential
Manager keys their secrets by the exact library path, so moving the library
requires connecting each integration again. The managed Attendee WSL installation
lives outside the library and Bazza starts it as needed when connecting to Meet.

The verified `ggml-large-v3-turbo.bin` stays in the persistent model cache at
`%LOCALAPPDATA%\Bazza-Review\model-cache`. When the library has no matching copy,
the launcher verifies the cache against the catalog values recorded at publish
time and copies it into the library's `models\`. Bazza initializes both
recognition model selections to `whisper-large-v3-turbo` in a new portable library.

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

Two opt-in desktop tests replay a saved Discord bot recording without the app
and write JSON for an external scorer. They only read the given copy:

```powershell
$env:BAZZA_TEST_MODEL = 'C:\models\ggml-large-v3-turbo.bin'
$env:BAZZA_VAD_MODEL = 'src-tauri\resources\ggml-silero-v5.1.2.bin'
$env:BAZZA_EVAL_RECORDING = 'C:\fixtures\recording-copy'   # mic/sys/mix + discord/
$env:BAZZA_EVAL_OUTPUT = 'C:\fixtures\combined.json'
cargo test --release -p bazza --lib combined_transcript_of_a_saved_recording -- --ignored
$env:BAZZA_EVAL_TRACKS = 'C:\fixtures\recording-copy\discord'
$env:BAZZA_EVAL_OUTPUT = 'C:\fixtures\live.json'
cargo test --release -p bazza --lib live_captions_of_saved_discord_tracks -- --ignored
```

`prepare-bundle.mjs` downloads the pinned Silero VAD model into
`src-tauri/resources/`; without it post-call recognition runs without VAD.

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
