# Build and asset scripts

`prepare-bundle.mjs` downloads ffmpeg, builds the diarization sidecar and LLM,
then collects native runtime libraries for Tauri. It can be expensive. Check
`--profile`, `--features`, `--target` and `CARGO_TARGET_DIR` handling when changing it;
CPU/CUDA artifacts must not mix. Its `dev` profile outputs to Cargo's `debug` directory.

`stage-runtime-libs.mjs` prepares test runtime libraries after test compilation.
It currently assumes `target/<profile>` and ignores custom target directories.
See [testing](../docs/dev/testing.md) for the CI sequence and runtime prerequisites.

For script-only edits, start with `node --check scripts/<file>.mjs`; it checks syntax
without running downloads/builds. `make-icons.py` generates assets: inspect resulting
images and commit only intentional icon changes.
