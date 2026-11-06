# Build and asset scripts

`prepare-bundle.mjs` downloads ffmpeg, builds the diarization sidecar and LLM,
then collects native runtime libraries for Tauri. It can be expensive. Check
`--profile`, `--features`, `--target` and `CARGO_TARGET_DIR` handling when changing it;
CPU/CUDA artifacts must not mix. Its `dev` profile outputs to Cargo's `debug` directory.

`stage-runtime-libs.mjs` prepares test runtime libraries after test compilation.
It honors `CARGO_TARGET_DIR`, `--target` and Cargo's dev/debug output mapping.
Keep the pinned NeMo CPU runtime in its own versioned subdirectory: its ggml/llama
libraries must not overwrite the application's native library family.
See [testing](../docs/dev/testing.md) for the CI sequence and runtime prerequisites.

For script-only edits, start with `node --check scripts/<file>.mjs`; it checks syntax
without running downloads/builds. `make-icons.py` generates assets: inspect resulting
images and commit only intentional icon changes.

## Clean installation

A clean installation is a full reset in the target OS user profile before
reinstalling Bazza. Stop Bazza and its services, run the uninstaller, then remove
the remaining Bazza-owned state in that profile. Include settings, caches,
downloaded models, the meeting library and its data at any redirected location,
stored credentials, integration registrations, and managed runtimes such as
the Attendee WSL distribution and its disk. Confirm the target and ownership
before deleting anything. Scope cleanup to Bazza-owned resources in that profile.

Verify that the previous installation and its state are gone before starting
the installer. Verify first-run behavior after reinstalling. Use a separate OS
profile only when the user explicitly requests a separate test profile.
