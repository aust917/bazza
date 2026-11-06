# Soniox Async diarization with local voice identification

Status: complete on 2026-11-02; no application integration. [Measured report and decision](../test-reports/2026-11-02-soniox-hybrid-speakers.md).

## Outcome and boundaries

Compare Soniox Async diarization plus the existing local TitaNet/Voices identity rule with fully local Nemotron and legacy Pyannote on the same recordings. Reuse the [saved Soniox cloud responses](../test-reports/2026-11-02-soniox-cloud-speakers.md), without reference-audio prefixes. Keep application source, settings, production models and ordinary libraries unchanged.

Tools and raw evidence stay under `.git/local-tools/soniox-hybrid-20261102/` and `.git/local-tests/20261102-soniox-hybrid/`. Enrollment audio/vectors never go to Soniox. A small additional control uses only the already downloaded public Russian actor fixtures from the [local bake-off](../test-reports/2026-11-02-speaker-quality-bakeoff.md); any new provider jobs are deleted together with their files. Credentials remain in process memory.

## Fixed comparison

- Twelve original synthetic podcasts and six saved short-query montages, with matching local inference on the exact audio. Add the two public human-control conversations and three known/unknown gallery folds.
- Fresh frozen production-sidecar Nemotron and Pyannote outputs; no application builds or changes. Cached native cluster embeddings remain a separate compatibility reference.
- Main identity comparison re-extracts every backend's anonymous cluster with one TitaNet Small artifact, 16 kHz PCM16, chronological intervals capped at 60 seconds and minimum one second. No reference-based purification.
- Soniox turns are consecutive lexical tokens of one speaker, split on a label change or gap over 0.8 seconds; no padding or name/text context. Declare a token-union sensitivity check separately if run.
- Use the native Voices cache: two independent samples per known profile. Unknown profiles are absent. The same actual Rust `best_match` rule and unchanged threshold 0.62 apply to all backends; record nearest scores separately from accepted identities.
- Score all backends against the same clip/energy activity proxy. Separate oracle diarization from identification; report correct, missed and wrong known names, false names for unknowns, valid unknown withholding, and unavailable evidence.
- Keep conditions, query durations and human folds separate. Reused crops and repeated voices are correlated; metrics are not human speech DER or real-person acceptance.

## Progress

- [x] Inspect current worktree, frozen cloud results, current local baseline and identity rule.
- [x] Freeze comparison inputs/gallery/protocol and verify hashes.
- [x] Run 40 matched local diarizer jobs and two public-control cloud jobs; reuse 18 saved Soniox responses.
- [x] Run common local embeddings and unchanged Rust matcher: 264 cluster/fold decisions, zero parity mismatches.
- [x] Independently validate scorer, compare all 72 folded results and write decision report.
- [x] Verify all 209 frozen artifacts, common PCM, gallery/model/runtime integrity, four remote deletions and documentation.

## Results and verification

All 60 backend/recording outputs and all 72 fold/backend/case rows completed; no missing pipeline or engine errors. Independent numerical audit checked the entire matrix, strict known/unknown coverage guards and actual Rust decisions. Ten evaluator sanity checks and a separate oracle/brute-force comparison on 500 random problems pass. All five Python tools compile. `node scripts/check-docs.mjs` checked 106 Markdown files successfully; `git diff --check` and a separate whitespace check for the two new untracked documents pass. Existing unrelated CRLF conversion warnings remain outside this task. No application build/test was run because application source was not changed.

The hybrid wins on clean synthetic known-name events (32/32 versus current Nemotron 14/32), but loses on the public actor control with long query context (8/36 versus 36/36, with five of 18 unknown events exposing a false known name versus zero). On short-only actor speech all three pipelines accept zero of 30 known names. These are bounded component/proxy observations, not general real-person accuracy.

A separate native-query compatibility audit reproduced all 52 available Nemotron/Rust decisions from the common extraction, including the positive actor-context control. Six missing native codec vectors stay unavailable; they are not successful rejection evidence. New cloud transcription/file IDs all returned HTTP 404 after deletion. Native voices, enrollment audio and user data never went to Soniox.

## Handoff

Current local baseline is Nemotron with uniform-history processing and TitaNet, integrated by a separate earlier task. Legacy `current-native` bake-off outputs refer to Pyannote and must not be presented as today's selected backend. This task only measures the hybrid and comparison components offline.

No work remains for this bounded comparison. The results do not justify replacing the current local pipeline with the hybrid. Reuse saved results offline; any later integration, new models, threshold calibration or larger real-meeting evaluation is separate work. Exact identities, commands, raw scores, independent audits and deletion evidence remain in the task-local roots above.
