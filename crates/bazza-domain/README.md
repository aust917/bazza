# Shared Bazza value models

Tauri-free, database-free value models shared by the local application and Store.
Store re-exports its existing model names to preserve native and bundle callers.
SQL row decoding, unknown stored-status fallback and persistence remain in Store.

Pure `cosine` and reciprocal-rank `fuse_hits` are also shared here. Their existing
algorithms and stable tie/first-hit behavior are preserved; Store compatibility
re-exports remain available. Native hybrid retrieval calls the domain function
directly. Database vector scanning, top-k selection and stale-result checks still
belong to Store.

These are private application models, not public MCP projections. Some contain
voice vectors, share tokens or private native settings. Public service contracts
must continue to use their separately bounded and sanitized DTOs.

`NativeSettings` defines the native settings JSON document, defaults, enum
values and four validation rules without importing capture or model engines.
Its Debug representation redacts user strings and credentials. Configuration
validation does not start integrations or implement cloud permission/cancellation.

```powershell
cargo test -p bazza-domain -p bazza-store
```

The [stage-3 report](../../docs/dev/test-reports/2026-11-02-mcp-phase-3.md) records
compatibility checks and the remaining native/service cutover gates.
