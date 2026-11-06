# Live and saved transcript versions

Status: active

## Outcome and boundaries

Use one transcript view for live and saved speech. Show the confirmed live text
as soon as recording stops. Each explicit re-transcription creates a separate
version using the requested audio source, model and language.

## Acceptance criteria

- The same confirmed row keeps its speaker, timestamp, text and grouping across
  Stop. Concurrent live speakers have independent provisional rows.
- The first version remains readable during later recognition. Successful runs
  appear as separately selectable versions after reopening the meeting.
- Empty, failed and cancelled runs preserve existing versions.
- Reopening the meeting while a version is running restores its stage and elapsed
  time. Recognition and participant timestamp matching report stage progress.
  Cancelling stops the version job before the save boundary. The UI reports 100%
  after the new version is saved and selects it automatically.
- Switching the view leaves the initial transcript used by search and summaries
  intact. Participant-track recognition can use the saved Meet or Discord tracks.

## Ownership and verification

- Desktop UI and tests use the shared transcript renderer and version selector.
- Legacy Stop publishes the initial version before background media processing.
- The library keeps version rows separate from canonical search and edits.
- The managed desktop uses authenticated service operations to store versions.

Verification: all 257 UI tests, TypeScript checks, documentation checks and
changed Rust syntax checks passed. The interface tests cover the Stop handoff,
retaining the raw initial text, two saved alternatives across reopen, and the
participant-track generation route.

The native Rust tests and desktop/service build remain to be run. Native
acceptance needs an explicitly requested build and an isolated recording
library.
