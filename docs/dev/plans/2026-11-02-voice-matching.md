# Voice matching: native baseline and unknown rejection

Status: complete
Owner: voice-matching task; source/runtime handoff coordinated with capture and
localization work in the shared working tree.

## Outcome and boundaries

Run the frozen [Russian Soniox corpus](../test-plans/2026-11-02-soniox-voice-corpus.md)
through actual Bazza in an isolated library. Reproduce false suggestions for
unknown voices, correct the matching boundary while preserving independent known
holdouts, and measure all twelve clean/stress podcast variants. Exercise the
[voice-profile lifecycle](../voice-memory.md) through native IPC and record what
the app actually persisted and proposed.

Keep the corpus, sample text, speaker roles and reference hashes fixed. Do not
regenerate voices or raise a threshold solely to pass these synthetic examples.
Keep data local and names as user-confirmed suggestions. Preserve original
transcripts, model identities, existing voice profiles/samples, migrations and Windows/
macOS guards. Audio, model output, test libraries and secrets remain untracked;
source/docs stay local-only. Capture defaults and normal libraries are outside
this task.

## Acceptance criteria

- An actual Bazza native-IPC baseline uses an explicitly isolated library and
  records binary/source/model identities, enrolled profiles and the frozen input
  hashes. Back up the Bazza library pointer, activate only the disposable library,
  then restore and verify the original pointer state after owned processes stop.
  Browser mocks and the older sidecar-only probe are separate evidence.
- Independent known-speaker holdouts retain useful correct suggestions, while
  unknown voices abstain after the fix. Record misses and false suggestions per
  identity; unchanged unknown inputs must not enter enrollment.
- All twelve podcast variants have measured diarization and name-suggestion
  outcomes, including merged/missed speakers and unknown-to-known confusion.
  Separate failures caused by clustering from matching. Do not call clip-bound
  coverage speech DER or imply that every stress case passed.
- Native profile/sample deletion, recomputation, proposal invalidation and restart
  persistence are checked. Names remain suggestions until confirmed by the user.
- A successful change to matching evidence or profile deletion invalidates pending
  guesses for the whole gallery; that invalidation preserves confirmed identities.
  Explicit profile deletion still clears its own links. Display-only
  renames, empty profiles and failed transactions retain guesses. Verify a
  cross-profile change and restart through native IPC in a separate library
  without mutating the main 52-case evidence.
- Invalid/nonfinite sidecar embeddings cannot abort a recording's processing;
  preserve their turns as unknown and test the output boundary explicitly.
- Focused unit tests cover the changed matching decision and preserve existing
  storage/model compatibility. Native before/after evidence distinguishes the
  fixed behavior from remaining synthetic, hardware and platform limits.

## Steps

- [x] Freeze and audio-verify the corpus; retain the original sidecar sample probe.
- [x] Receive the coordinated source-freeze release; preserve the baseline executable.
- [x] Run and save the actual isolated Bazza IPC baseline from the frozen executable.
- [x] Implement and test the bounded matching correction.
- [x] Run focused tests and build the agreed native snapshot.
- [x] Repeat holdouts, unknowns, all podcast conditions and lifecycle/restart checks.
- [x] Finish the related global pending-invalidation fix and its separate native probe.
- [x] Write the measured report and update the owning voice/corpus documentation.

## Decisions and findings

The frozen corpus's original pre-rename sidecar probe accepted all 12 reference
samples as single voices. An offline app-rule probe recognized four known
holdouts but falsely suggested known names for all four unknown samples. This
establishes a concrete regression target, not actual Bazza app acceptance; see
the corpus guide for saved evidence and acoustic similarity limits.

The selected matching design requires at least two independent nonlegacy supports
per profile, derives its verification floor from leave-one-out sample consistency,
ranks the nearest profile before verification, and abstains if multiple profiles
pass. Support loading excludes the current meeting and deduplicates provenance
and normalized vectors. Migration 7 is scoped to clearing pending suggestions.
No global threshold is raised to fit the synthetic unknowns.

The diarization correction preserves up to eight engine clusters, including short
and similar clusters. If an over-cap merge is necessary, naming/voiceprint use
requires unanimous verification of its raw contributors; a missing embedding
leaves an unknown row. The measured report distinguishes this app behavior from
remaining engine-level mixed clusters and missed known suggestions.

The baseline also exposed a sidecar output boundary failure: nonfinite
embedding components became JSON `null`, which the app could not parse as `f32`.
A scoped CLI guard omits invalid, wrong-dimension or zero embeddings while
retaining turns as unknown. This regression is separate from false-name matching
and does not change model identity, normalization or the matching floor.

The baseline executable was frozen in the local acceptance runtime before source
changes. Its SHA-256 is
`93814ba996a400fe15c02d7a45e580fa743a4bdbb8b773c6cea1a90426b5ab0b`.
Runtime execution slots remain coordinated with capture/localization work. The
separate rename task owns strict current database/share formats; this task does
not add acceptance claims for legacy product formats or data-directory discovery.

## Verification

The [measured report](../test-reports/2026-11-02-voice-matching.md) records the
completed raw sidecar diagnosis for all twelve podcast conditions, including
mixed-reference clusters and nonfinite codec embeddings. Its offline verification
diagnostic is separate from app acceptance. The corpus guide records the earlier
generation/audio checks and sample-only probe. The native baseline aggregate
observed all 52 cases: 49 processed, three codec errors, 20 false known-name
suggestions for unknowns. The first failed attempt remains preserved; per-case
resumption did not recompute completed suggestions.

Focused checks, provenance hardening/store retest, CLI output-guard tests,
clippy and the normal-config main fixed build passed. Startup migration kept four
profiles, eight samples and 184 segments unchanged while clearing 61 pending
suggestions. The main fixed run processed 52/52 cases: 85 known correct, 75 known
missed, zero wrong known names, all 24 unknown segments withheld, zero processing
errors. Clean known segments were 32/32 correct, overlap 31/32; isolated known
recall fell to 22/32 and all noisy/codec known segments abstained. Preserve these
quality limits rather than treating abstention as general recognition success.

All six main lifecycle checks and 58 reopen checks passed; that app stopped and
the pointer was restored. The related stale-gallery correction is implemented:
successful evidence transactions invalidate all pending guesses while preserving
confirmed rows; metadata-only changes and failed transactions retain guesses.
Three new regressions bring the store run to 40/40, with strict store clippy
passing. The separate confirmation probe passed 62 checks, including actual
acceptance, cross-profile invalidation and muted playback. The delivered executable
passed 38 reopen checks. Final normal-config and post-font-assets builds passed.
All three pointer backups were restored, both owned sessions were inactive, and
the corpus manifest was unchanged at handoff (`teardown.json` in the report's
local evidence root). This task's migration acceptance is explicitly schema v7;
later capture-owned migrations are outside this snapshot.

## Handoff

No work remains for this bounded correction/acceptance task. Preserve the frozen
corpus, main result database and separate confirmation evidence. Source/Cargo/
runtime ownership has returned to capture/localization; their future app or
migration changes require their own snapshot-specific acceptance. Improving known
recall on short/noisy/codec speech and measuring real-person quality remain
separate follow-up work, with the current misses retained in the report.
