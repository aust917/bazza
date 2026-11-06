# Desktop integration guidance

`src/lib.rs` registers commands and plugins; `src/state.rs` owns shared storage,
settings, engines, and active recording state. Keep domain algorithms in the
corresponding `../crates/` package.

- Changing IPC requires updating `src/commands/`, registration in `src/lib.rs`,
  and `../app/src/lib/ipc.ts`. Event names/payloads must match frontend types;
  events are broadcast to multiple windows.
- Keep recording start/stop and heavy filesystem or inference work off the main
  thread. Preserve `busy_guard()` around operations using cached engines so the
  idle unloader cannot interrupt active processing. Avoid holding shared locks
  across long operations.
- Detection opens the picker; it must not start recording. Preserve speaker-name
  confirmation and explicit share creation. Previously enabled integrations and
  startup recovery may run in the background.
- Preserve partial recordings and orphan recovery when changing recorder or
  pipeline stages. Distinguish fatal input/storage failures from optional
  diarization, cleanup, embedding, or summary failures.
- Treat `tauri.conf.json`, capabilities, sidecars, and asset-protocol scopes as
  integration boundaries. Fork releases need their own updater signing and
  identity decisions; a different app identifier alone does not change the
  `Bazza` data directory defined in `bazza-store`. Use its Bazza-only `data_dir`
  and `database_path` when opening a user library.

See [testing](../docs/dev/testing.md) for native prerequisites, build checks, and
smoke-test behavior before launching hardware-dependent validation.
