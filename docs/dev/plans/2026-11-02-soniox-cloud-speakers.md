# Isolated Soniox speaker evaluation

Status: complete on 2026-11-02. Application integration is outside scope. [Results and decision](../test-reports/2026-11-02-soniox-cloud-speakers.md).

## Outcome and boundaries

Measure actual Soniox cloud diarization on the frozen [synthetic Russian corpus](../test-plans/2026-11-02-soniox-voice-corpus.md). Evaluate experimental identification by prepending the four known enrollment samples to each query and deriving names only from their time windows. This is joint clustering, not a native persistent voice-enrollment API.

The user's request explicitly authorizes these cloud tests. Upload only the fictional corpus and derived fixtures. Keep application code, settings, ordinary libraries, models and matching thresholds unchanged. Credentials stay in process memory; delete each remote async transcription and file separately. Tools, audio, provider responses and machine-local evidence stay under `.git/local-tools/soniox-cloud-20261102/` and `.git/local-tests/20261102-soniox-cloud/`.

## Acceptance criteria

- Verify the frozen corpus hashes and save the provider model catalog.
- Run async baseline and known-anchor versions of all twelve podcasts; reverse anchor order for three clean podcasts.
- Run independent 0.5/1/2/3/5/10-second query dialogues, baseline and anchored, retaining unknown identities outside enrollment.
- Check one short query at a time with the same four enrollment anchors, separating query context from duration.
- Compare real-time diarization on the three full clean podcasts at actual wall-clock audio rate, without endpointing.
- Derive known-name mappings exclusively from enrollment windows, reject ambiguous mappings, and count false names for unknowns separately.
- Retain missing output, errors, latency and cleanup outcomes. Use clip/token attribution proxies, never claim human speech DER.
- Independently review the evaluator and publish a local decision report with actual results and limits.

## Progress

- [x] Inspect scope, preserve existing worktree, verify all 209 frozen file hashes.
- [x] Build 42 isolated cases and save current v5 model availability.
- [x] First async two-speaker clean probe completed; remote file and transcription deleted.
- [x] Complete the initial 39 async and three real-time cases.
- [x] Extend with 36 single-query cases; finish all 75 async + three RT requests without processing errors.
- [x] Run evaluator sanity checks and independent methodological/numerical review.
- [x] Write results, recommendation and remaining limits.
- [x] Delete remote objects and independently confirm HTTP 404 for all 150 IDs.

## Decisions

The four full reference-A samples form the known gallery. Query turns and reference-B samples are not enrollment. All four anchors are included even when some known people are absent. No speaker names or query text are supplied as provider context. The same fixed rules apply to every condition. Real-time tests measure provider output directly, without the app's capture/IPC pipeline.

## Verification and handoff

All 78 provider requests completed. Both scripts compile; evaluator sanity checks pass, and an independent oracle comparison agrees with brute force on 400 random problems. All 209 frozen corpus hashes and every derived input hash remain valid. `node scripts/check-docs.mjs` checked 80 Markdown files successfully; `git diff --check` passed, and new untracked documents received a separate whitespace check. Existing unrelated CRLF conversion warnings are outside this task. No application build was run.

The decision is complete: retain async diarization as a candidate, reject the tested prefix method as reliable automatic identification. Forward anchors falsely name an unknown speaker; changing their order changes the outcome. Real-time six-speaker output merges two pairs. Short isolated unknowns receive false names through three seconds. These are synthetic-corpus observations, not real-person acceptance.

Local tools and raw evidence stay at the paths above; `evaluation/summary.json`, `run-metadata.json` and `remote-cleanup-audit.json` own exact results, identities and deletion evidence. No paid follow-up or implementation is pending. Reuse saved responses offline; any later real-person evaluation or application integration is separate work.
