# UX regression checks

Run from the repository root:

    pnpm test:ux
    pnpm typecheck

The *.ux.test.tsx files also run in the ordinary pnpm test suite. They check
task completion, keyboard focus, visible feedback, retained drafts and retry
after failure. Synthetic IPC and dialog responses keep these checks independent
of hardware, model downloads, user data and external providers.

## Automated scenarios

| Boundary | User outcome |
| --- | --- |
| Menu | Arrow keys open and navigate enabled actions; Home/End reach the ends; Escape and selection restore focus; Tab leaves the menu |
| Share/export dialog | Named modal receives and contains keyboard focus, Escape returns to its trigger; invalid ranges explain format, ordering or recording bounds before a file picker; progress and failure are announced and retry retains the range |
| Share management | Failed loading, clipboard, disable/delete and refresh actions show errors; retrying a refresh does not repeat a completed mutation; late replies cannot update a closed or different meeting |
| Transcript | Closing or saving a correction returns focus to the originating edit action; the original remains available |
| Speaker name | Pending save blocks duplicate actions; failure retains the editor/draft and offers keyboard retry |
| Chat | IME composition does not send accidentally; failed sends retain text and show an accessible error; switching conversations keeps pending responses and generation controls in the right conversation |
| Library import | Canceling a paired import creates no meeting; slow imports block duplicate entry points, show progress and recover after partial failure |
| Settings | Revoking cloud consent selects local transcription; saving a key can be retried after failure without retyping, and successful save clears the draft |
| Onboarding | A slow hardware probe keeps visible loading feedback and Back; failure has an explicit retry path |

Assertions combine feedback with completed actions. For example, a test must not
pass because an event was ignored, a request never started, or the editor simply
discarded the user's input. Promise gates control pending/error states. These
tests do not impose jsdom wall-clock speed thresholds.

## Computer-use walkthrough, 2026-11-02

Checked the local Vite preview on Windows in the in-app Chromium browser with
the repository's built-in mock backend. Used English at 1280 x 720 and Russian
at 1024 x 720. All meeting text and names were demonstration fixtures; a synthetic
correction was added only to that in-memory preview.

| Task | Observation |
| --- | --- |
| Open a meeting from Library | Navigation and meeting controls were reachable in both languages |
| Open Share and press Escape | Initially failed: focus stayed on Share behind the overlay and Escape did nothing. After the fix, focus entered Close and Escape restored Share |
| Shift+Tab from Close; Tab from the last export control | After the fix, both directions wrapped inside the dialog |
| Open Export with ArrowDown | Initially failed. After the fix, the first enabled item received focus; End reached the last item; Escape restored Export |
| Tab from an open export menu | Menu closed and real browser Tab moved to Re-run |
| Edit a transcript, save with Ctrl+Enter | Corrected text appeared, editor closed, and focus returned to the same edit button |
| Open Raw after saving a correction | Original text remained available |
| Switch interface language to Russian | Navigation and action labels changed; the meeting's English text remained English |
| Enter a clip beginning after the recording ends | Error identified the actual 45:00 recording boundary before opening a file picker; long Russian controls wrapped inside the dialog |
| Search for a visible meeting title | Preview returned no matches because mock.ts has an empty search stub. Native search success was not established by this walkthrough |

The walkthrough confirmed actual browser focus and Tab behavior beyond jsdom.
It did not run native recording, file dialogs, inference, LAN sharing, cloud
requests or user-library persistence. A zero-duration player in these fixtures
is a mock limitation. Native recording acceptance remains in
[the human control plan](test-plans/2026-11-03-human-control.md).

Verification on 2026-11-02: all 34 new UX tests in nine files passed; the full UI
suite passed 147 tests in 38 files, including the performance regressions.
Type checking, production UI build, documentation checks and whitespace checks
passed. Vite retained its large JavaScript chunk warning. No dependencies were
added. Native binaries were not rebuilt for these UI changes.

Chat running-state tracking covers conversation switching inside one open scope.
Restoring generation controls after leaving that page or restarting still needs
a backend job-status API; this suite does not establish that behavior.

## What still needs a person

Agent walkthroughs expose interaction failures and layout obstacles. Testing
with a new user is needed to learn whether labels, grouping and next steps are
understandable without instructions. Use a disposable library and ask the person
to complete these tasks without pointing to specific controls:

1. Find a saved meeting and locate a particular quote.
2. Correct a speaker name and one transcript sentence; find the original again.
3. Check an AI conclusion against its source moment.
4. Export a chosen fragment locally and explain where it was saved.
5. Recover from a failed operation without entering the same text again.

Record completion, requests for help, wrong turns, unexpected consequences and
the person's own explanation of what happened. Record time as an observation;
agree acceptance targets before comparing versions. Also check native keyboard
behavior, actual media, Windows scaling/zoom, long Russian labels, screen-reader
use and macOS separately. Passing these automated scenarios does not establish
overall usability or conformance to an accessibility standard.
