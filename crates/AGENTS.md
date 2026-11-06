# Rust crate guidance

These crates own reusable domain logic and native integrations; Tauri commands,
window state, and orchestration belong in `../src-tauri/`. Use the
[architecture map](../docs/dev/architecture.md) to choose a crate before reading
unrelated implementations.

- Keep `bazza-diarize` independent of native inference. `bazza-diarize-cli` owns
  sherpa-onnx execution; its JSON contract lives in `bazza-diarize/src/protocol.rs`.
  Update both sides when changing sidecar inputs or output.
- In `bazza-store`, append schema migrations instead of rewriting released
  migrations. Preserve raw segment text when updating `clean_text`, and keep FTS,
  retrieval chunks, and vectors consistent with transcript edits. Exercise schema
  upgrades as well as creation of an empty database.
- Preserve model checksum verification and resumable download behavior in
  `bazza-models`. Catalog changes must provide real compatible artifacts and
  checksums.
- Keep platform APIs behind the existing target guards. Capture callbacks must
  remain bounded; disk writing, inference, and expensive shutdown work belong on
  worker paths.
- Maintain token validation and disabled-share checks in `bazza-share`; exported
  media and bundles must remain confined to their intended files.

Prefer the affected crate's tests first; native AI crates need additional runtime
libraries. See [testing](../docs/dev/testing.md) before choosing workspace-wide
checks or model-dependent examples.
