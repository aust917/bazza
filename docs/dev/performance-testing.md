# Performance regression tests

The ordinary UI suite includes the `*.performance.test.*` files. Run just those
checks from the repository root:

```powershell
pnpm test:performance
```

These tests use synthetic long inputs, fake timers, deferred IPC responses and
work counters. Their budgets stay the same across machines. Keep assertions on
visible output as well: dropping events, skipping rows or omitting audio must
fail even when that would reduce the measured work.

## UI budgets

| Boundary | Fixture and required work |
| --- | --- |
| `Transcript.performance.test.tsx` | 128 and 2,048 segments; logarithmic timestamp lookup; zero row formatting within an utterance; two rows updated when the active utterance changes; backwards seek and trailing gap keep the right highlight |
| `Meeting.performance.test.tsx` | Actual Meeting page and Transcript with 1,500 segments; playback preserves row reuse through parent callbacks/derived props; percent-only processing events add no row work or detail IPC reads |
| `ChatPanel.performance.test.tsx` | Real Markdown, 128 completed answers, 1,200 deltas over 60 flushes; one live render per batch, completed history stays cached; foreign/late events and unmount leave no pending parser work |
| `Library.performance.test.tsx` | Events for 40 meeting IDs: over 12,000 processing events and 500 recorder events; one event-driven request in flight, one follow-up for the latest state, metadata loaded only when needed; failure recovery and cleanup; typing burst produces one debounced search |
| `format.performance.test.ts` | 1,000 dates with language and year changes; at most six Intl constructors for two supported locales, no per-row `toLocale*` formatting, correct presentation after cache reuse |

The existing `useAudioLevels.test.tsx` also verifies serial device probes, retry
pacing, hidden-window suspension and stale-result cleanup. Run it with
`pnpm exec vitest run src/lib/useAudioLevels.test.tsx` or the full `pnpm test` suite.

## Native budgets

With the [Rust toolchain and native dependencies](testing.md) available:

```powershell
cargo test -p bazza-capture -p bazza-store --release --lib --locked --offline performance_
```

- `wav::tests::performance_resampling_delivers_bounded_blocks_before_reading_the_whole_recording`
  exercises generated mono/stereo inputs of 32,768 and 131,072 frames, including
  identity, downsampling and upsampling. The consumer receives bounded output
  blocks after at most 16,384 input samples, and receives the complete recording.
- `vectors::tests::performance_scoped_retrieval_scores_only_matching_vectors_across_bounded_batches`
  uses more than two eligible batches among other meetings, incompatible vector
  dimensions and sparse IDs. Each read returns at most 512 candidates; scoring
  visits each eligible vector once and returns the expected top seven results.

`--offline` requires cached dependencies; omit it for an initial setup that needs
crate downloads. Release mode reuses the local native build when available; the
budgets also apply in debug mode.

The desktop package has a separate check because the regular workspace test
command excludes `bazza`:

```powershell
cargo test -p bazza --release --lib --features tauri/custom-protocol --locked --offline performance_
```

Prepare real sidecars/runtime libraries as described in [testing.md](testing.md)
before compiling this package. The tests themselves do not open audio devices,
launch the application or access a user library. The `commands::performance_tests`
module gates a worker with channels and verifies that the caller yields, another
command can finish before release, and worker errors preserve subsequent use.
Five-second channel timeouts are deadlock watchdogs, not latency targets. It also
checks at compile time that heavy command entry points return futures.

## Evidence and limits

The initial Windows run passed all 14 new checks (nine UI, five Rust), the full
113-test UI suite, `pnpm typecheck`, and the affected Rust library suites:
87 capture, 61 Store and 45 desktop tests passed; six existing environment-dependent
tests remained ignored. Documentation checks and `git diff --check` passed.

On 2026-11-02, isolated copies of the UI modules were deliberately regressed:
removing transcript memoization, using linear active-row lookup, rerendering chat
history, flushing every token, removing Library coalescing/search debounce and
removing Intl caching all made the new checks fail. An unchanged copy passed.
The application sources were preserved during these mutation checks.

Work counters cover selected boundaries. They do not measure first-open DOM
cost, browser layout/frame latency, retained copies or total process memory.
Async command signatures do not prove that every statement inside a command
runs on a worker. Preserve native acceptance with a disposable library: a cheap
IPC heartbeat must continue responding during cold model loading, export and
library copying. Model stress, macOS and the live-ASR backlog need separate
validation. The [performance audit](test-reports/2026-11-02-performance-audit.md)
and [fix report](test-reports/2026-11-02-performance-fixes.md) record the earlier
runtime measurements and remaining risks.
