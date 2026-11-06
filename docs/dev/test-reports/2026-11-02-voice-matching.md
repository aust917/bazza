# Voice matching on the frozen Russian corpus

Status: complete for the recorded schema-v7 snapshots and synthetic test scope.
The fixed run processed all 52 cases with no wrong known-name suggestions and
withheld all 24 unknown reference segments. It also withheld 75 of 160 known
segments, including every noisy/codec known segment. Gallery-invalidation,
confirmation and delivered-build reopen checks also passed. These results do
not establish general recognition quality.

## Scope and evidence

The [task plan](../plans/2026-11-02-voice-matching.md) owns the current scope and
next action. The [corpus guide](../test-plans/2026-11-02-soniox-voice-corpus.md)
owns frozen speaker roles, enrollment files, audio transforms and reference limits.
No new provider generation is part of this task. The app harness exercises native
profile enrollment, supplied-text recording import, local `rediarize` and saved
speaker suggestions. A separate native player probe checks muted sample playback.
This does not measure STT accuracy, capture, summaries or visual presentation.

Local evidence is under `.git/local-tests/20261102-voice-matching-acceptance/`
(**ACCEPTANCE** below), separate from the frozen corpus directory. Its disposable
library, copied baseline runtime and synthetic results are untracked. No ordinary
user library or capture source is an input.

| Preserved runtime | SHA-256 |
| --- | --- |
| Baseline app | `93814ba996a400fe15c02d7a45e580fa743a4bdbb8b773c6cea1a90426b5ab0b` |
| Baseline/raw-diagnosis sidecar | `1d997d957ab9075771809521b97ec5115e81acb5be44463dea037dc6624011f7` |
| Main fixed app | `1d79410c679a9474cfd01a47f96afefc70d1135d16a4ab3481ad4be229bd1727` |
| Gallery-confirmation app | `e9a3cf0bdfb88627d6346aac037536199a84fa58314363681a616b758bc6909a` |
| Delivered app | `839f12e7bcd2defbfb3d2e367565e2edef9aa2435c431e194d2a659dd810d97b` |
| Fixed sidecar | `ddb8810dac7693a7b463234d10190702c8d0efb48cfe876317cb9bcef6166be8` |

Private `runtime-baseline/`, `runtime-fixed/`, `runtime-final/` and
`runtime-delivered/` copies keep runtime identity separate from the changing
source tree. The 52-case fixed suite used the main fixed app; the separate
confirmation suite used the gallery-confirmation app. The delivered app is
28,531,712 bytes and passed its own reopen probe. `ACCEPTANCE/delivered-runtime.json`
records equal backend source hashes across the last two builds; the delivered
build followed readiness of the final font assets. Do not claim that the entire
52-case suite was rerun on the delivered executable.

## Completed raw sidecar diagnosis

Utility: `python .git/local-tools/probe-voice-corpus-raw.py`. It converts each
frozen podcast to 16 kHz input and runs the copied CPU sidecar with two threads,
before application consolidation or voice-name matching. Evidence:
`ACCEPTANCE/raw-diarization/summary.json`, per-case JSON and logs. The summary
records the binary and model hashes.

| Podcast reference speakers | Clean raw clusters | Noisy | Codec | Overlap |
| --- | --- | --- | --- | --- |
| Two known (`s1`, `s2`) | 3 | 4 | 9 | 3 |
| Four known (`s1`–`s4`) | 5 | 5 | 8 | 7 |
| Four known plus two unknown (`s1`–`s6`) | 6 | 6 | 9 | 6 |

All twelve cases produced measurable output, but cluster count is not speaker
recognition accuracy. Dominant overlap with the placed reference clips already
mixed `s3`/`s5` in the six-speaker noisy case and `s1`/`s4` in the six-speaker
codec case. Some codec-condition embedding components were nonfinite and were
serialized as JSON `null`. The earlier offline diagnostic's NaN scores must not
be described as evidence that those were merely zero vectors. Application
matching cannot recover a clean identity merely from the count of engine clusters.

`ACCEPTANCE/raw-score-diagnostic.json` contains an offline diagnostic using the
enrollment-derived verification rule. It retains clean known groups and withholds
unknown groups; noisy/codec cases often abstain for known voices too. This is a
diagnostic on engine output, not proof that the actual app uses or persists the
intended result. The actual native results below establish that separate boundary.

The overlap mapping is based on clip placement, including synthetic silence. It
is not an aligned speech-activity reference or speech DER. Mixed-reference groups
are a useful failure signal, not a substitute for a manually checked meeting
quality benchmark.

## Actual app baseline and correction

The completed baseline aggregate is `ACCEPTANCE/baseline-summary.json`: all 52
expected cases were observed, 49 processed, and three returned processing errors.
Eight known samples were added through IPC to four profiles; neither unknown
voice was enrolled. These are reference-segment suggestion outcomes, rather than
automatically assigned names or a biometric accuracy benchmark.

| Baseline bucket | Cases processed | Correct known | Wrong known | False known for unknown | Mixed-reference groups |
| --- | --- | --- | --- | --- | --- |
| Independent isolated turns/references | 40/40 | 32 | 0 | 8 | 0 |
| Clean podcasts | 3/3 | 25 | 7 | 4 | 3 |
| Noisy podcasts | 3/3 | 25 | 7 | 4 | 3 |
| Codec podcasts | 0/3 | — | — | — | — |
| Overlap podcasts | 3/3 | 25 | 7 | 4 | 3 |
| Total processed cases | 49/52 | 107 | 21 | 20 | 9 |

The three codec errors are missing processing results, never successful unknown
rejection. The pointer's original absent state was backed up before activation;
the baseline app stopped gracefully and that state was restored before the fixed
phase. Final restore-at-handoff evidence is recorded below.

The first attempt failed on `01-two-known-codec`: the app could not deserialize
an embedding component (`invalid type: null, expected f32`). That attempt is
preserved as failed. The harness now records native processing errors per case
and resumes with `--skip-completed`, without recomputing or invalidating completed
suggestions. This is a separate processing regression, not successful rejection
of an unknown speaker. A scoped sidecar output guard now omits
invalid, wrong-dimension or zero embeddings while keeping their turns unknown;
normalization, model identity and the matching floor stay unchanged.

The fixed normal-config app/sidecar were built and copied to the private fixed
runtime. Startup migration 7 passed: four profiles, eight samples and 184 segments
remained unchanged, while 61 pending suggestions were cleared to zero. Evidence:
`ACCEPTANCE/before-policy-migration.json`, `baseline-library.sqlite` and
`after-policy-migration.json`. The saved baseline database preserves schema v6.

The completed main fixed aggregate is `ACCEPTANCE/fixed-summary.json`: all 52
cases processed, with zero processing errors. No known segment received another
known identity, and no unknown segment received a known identity. Unknowns were
detected and withheld rather than disappearing from the results.

| Fixed bucket | Cases processed | Correct known | Missed known | Unknown withheld | Mixed-reference groups |
| --- | --- | --- | --- | --- | --- |
| Independent isolated turns/references | 40/40 | 22 | 10 | 8 | 0 |
| Clean podcasts | 3/3 | 32 | 0 | 4 | 0 |
| Noisy podcasts | 3/3 | 0 | 32 | 4 | 1 |
| Codec podcasts | 3/3 | 0 | 32 | 4 | 1 |
| Overlap podcasts | 3/3 | 31 | 1 | 4 | 0 |
| Total | 52/52 | 85 | 75 | 24 | 2 |

This is a conservative tradeoff, not general recognition-quality acceptance.
The clean podcasts retained all 32 known suggestions; overlap retained 31 of 32.
Independent isolated turns dropped from 32 correct to 22 correct and ten
abstentions. All codec cases now process, but all 64 known segments in the noisy
and codec conditions abstained. Two engine-level mixed-reference groups remain.
No threshold was fitted to the stress labels.

The main run's six native lifecycle checks passed and its settings were restored.
`ACCEPTANCE/fixed-reopen-5fbdb15b-7c45-489d-a96d-58039d506ae2/` contains 58 passing
restart checks: four profiles, eight samples and all 52 case snapshots persisted.
The main app stopped and its pointer state was restored before the separate
confirmation probe.

## Related gallery invalidation regression

Review found that mutating profile B cleared only B's pending suggestions, while
it could also invalidate a pending suggestion for A: the nearest candidate or
the set of passing candidates may have changed. The store correction now clears
all unconfirmed pending guesses inside successful evidence transactions. That
invalidation preserves confirmed identities and text; an explicit profile
deletion still clears its own links. It covers sample addition/deletion/recomputation,
confirmed-observation updates, profile upsert and deletion. Display-only profile
rename, empty-profile creation and failed transactions retain guesses. This is
separate from changing acoustic thresholds.

The separate `ACCEPTANCE/confirmation/library/` probe passed 62 checks:

- A real `accept_speaker_suggestion` confirmed A's name and added its meeting
  observation, giving A three samples and B two. Unknown and stale-B acceptance
  requests were rejected.
- Recomputing B cleared pending guesses for both profiles. Confirming A cleared
  an unrelated pending B guess while preserving confirmed identity and raw
  transcript text/timestamps.
- Four muted sample-playback checks passed in the native player.

Evidence: `ACCEPTANCE/confirmation/initial-ac5fc8af-8179-4281-af68-9be8e915b89f/result.json`.
The delivered executable then passed 38 reopen checks in
`ACCEPTANCE/confirmation/reopen-ced61c0e-6553-4125-bd53-17f6dda5205e/result.json`.
The completed 52-case database/snapshots were not used for confirmation or mutated
by this probe. The accepted migration snapshot is explicitly schema v7; later
capture-owned migration work is outside these results.

Known holdouts must be independent of enrollment: after both reference samples
are enrolled, use separately generated podcast turns rather than either enrolled
sample as the known holdout. One-sample profiles are a distinct support/abstention
case, not evidence that independent known recognition was preserved.

## Commands, checks and handoff

The native entrypoints were `.git/local-tools/run-soniox-voices-corpus.mjs` with
`--session`, `--phase baseline` or `--phase fixed`, and `--go`; `--skip-completed`
resumed the failed baseline without rerunning completed cases, and `--reopen`
checked persistence. The separate `.git/local-tools/check-voice-corpus-confirmation.mjs`
used its own `confirmation/session.json`, `--go`, then `--reopen`. The root-owned
`.git/local-tools/voice-corpus-runtime.ps1` coordinated private runtime launch,
library activation and pointer restoration. Preserve the saved evidence; another
run needs an explicitly owned disposable session and a coordinated runtime slot.

Coordinated checks passed on the shared source snapshot: 25 diarization tests,
36 store tests, and 19 sharing unit tests plus one sharing-server test (81 total).
The full desktop library run passed 33 tests with one ignored, including three
voice, two organization and three video tests. Its temporary `TAURI_CONFIG` set
bundle resources and external binaries to empty lists to avoid copying runtime
DLLs; it is not evidence for a normal-config application build. Logs use the local
`.git/local-tools/bazza-strict-rename-` prefix. A small provenance hardening change
subsequently passed the store retest (37/37), and the sidecar output guard passed
three CLI tests. Clippy passed for store, diarization and CLI with all targets
and `-D warnings`; the release sidecar built successfully. The coordinated
normal-config app build and desktop clippy also passed. Final gallery regressions
passed 40/40 store tests and strict store clippy; logs are
`.git/local-tools/voice-matching-cache-tests.log` and
`voice-matching-cache-clippy.log`. The normal-config final app build and subsequent
post-font-assets build passed (`voice-matching-final-build.log` and
`voice-matching-final-assets-build.log`). Node syntax checks passed for both native
helpers, and three local acceptance Python tools passed compilation. The
localization task's UI probe remains separate from voice-recognition acceptance.

`ACCEPTANCE/teardown.json` records restore-at-handoff: all three pointer backups
have `restored: true`, both owned sessions have inactive pointers and null app
PIDs, and the frozen corpus manifest hash is unchanged. The original Bazza
pointer was absent and that state was restored. Source/Cargo/runtime ownership
was handed back to capture/localization; later work may legitimately change its
own process/pointer state or append a migration. No continuing global idle-state
claim is made for those later tasks.

## Remaining limits

This is synthetic Russian speech on the local Windows CPU setup. It does not
establish quality for real people, real microphones/rooms, macOS, production
threshold calibration or old product formats. Preserve all failures and unknown
cases; do not alter the frozen corpus to improve scores.
