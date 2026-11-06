# Frontend guidance

`src/App.tsx` selects the main hash router or the popup/record-bar window from
`?window=`. Keep secondary windows independent of main-window onboarding and
navigation.

- Route application commands and event subscriptions through `src/lib/ipc.ts`.
  A contract change also needs the Rust command, handler registration, and payload
  definitions. Preserve argument names and serialized field names across both
  languages.
- Keep `src/lib/mock.ts` aligned with changed commands. Plain `pnpm dev` uses
  sample data outside Tauri; a successful browser preview does not verify native
  recording, dialogs, persistence, or IPC.
- Native dialog/opener plugins and player URL conversion already have dedicated
  callers outside `ipc.ts`. Keep those integrations
  localized; do not introduce direct filesystem access in UI components.
- Release event listeners when components unmount, including subscriptions that
  resolve asynchronously. Shared events reach multiple windows; filter by the
  relevant meeting/chat identifier.
- Preserve raw and cleaned transcript choices and timestamp-based navigation.
  Keep export and talk-time calculations in the existing pure helpers.

Use [the testing guide](../docs/dev/testing.md) for frontend checks and the
distinction between mock previews and native validation. Tests live next to the
component or helper they exercise.
