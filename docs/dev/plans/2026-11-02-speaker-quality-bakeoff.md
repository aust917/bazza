# Speaker quality tests before application integration

Status: complete for the bounded local pre-integration evaluation; application integration is not part of this task.
Owner: speaker-quality bake-off; application inference and normal libraries are outside this task.

## Outcome and boundaries

Run actual local model comparisons on the saved [Russian voice and podcast corpus](../test-plans/2026-11-02-soniox-voice-corpus.md) before selecting an implementation. The user prioritizes maximum quality on clean audio, especially short utterances. Preserve satisfactory ordinary-recording behavior and unknown rejection. The [technology research](../../product/2026-11-02-speaker-recognition-research.md) supplies candidates, not measured winners.

Keep source/model downloads local, preserve the frozen corpus and its hashes, and write new derived fixtures/results under a separate ignored local test root. No model, meeting audio, transcript, credentials or raw embeddings belong in tracked documentation. Do not change application models, thresholds, capture, settings or the user's library. New external audio processing and paid services are not part of the local tests.

## Acceptance criteria

- Freeze environment, executable/model identities and corpus provenance before inference.
- Compare current TitaNet small with actual ERes2NetV2, CAM++ and ECAPA embeddings; evaluate further strong candidates when feasible. Keep unavailable/failed configurations visible.
- Test isolated clean speech at 0.5/1/2/3/5/10 seconds, full independent utterances, known and unknown voices, and separate registration length from query length.
- Compare actual current diarization, Community-1 and Nemotron 3 where runnable. Include full clean podcasts and derived short dialogues with and without longer speaker context; stress variants remain secondary.
- Separate raw embedding, diarization, matching-policy and caption-assignment effects. Do not label clip-placement or automatically derived activity proxies as human speech DER.
- Freeze development/test partition before calibration; retain unmodified current-policy results and distinguish exploratory calibration from production acceptance.
- Record per-duration/per-speaker errors, false names, abstentions, processing failures and performance. Sanity-check evaluator behavior with reference/permuted/error outputs.
- Produce a concise report with reproducible commands, actual evidence, recommendation and limits. A synthetic six-voice result alone cannot establish real-person or macOS acceptance.

## Steps

- [x] Inspect current worktree and existing corpus/runtime documentation.
- [x] Verify immutable inputs and create deterministic independent duration/context fixtures.
- [x] Run direct encoder comparison and current Rust matching policy.
- [x] Run complete diarization comparison and secondary stress tests.
- [x] Evaluate context, short-turn preservation and final naming separately.
- [x] Review results independently and document the measured selection.

## Decisions and findings

- Machine: Windows, Intel i7-14700KF, 64 GB RAM, NVIDIA RTX 4090. Native CPU baseline uses two threads; GPU-heavy diarization is coordinated separately.
- Python 3.12 and existing ML packages/caches are available; task tooling uses isolated environments and records exact versions.
- Existing corpus has eight enrollment samples for four known synthetic identities, four unknown-reference samples, 36 independent source turns and twelve assembled podcasts. Unknown identities never enter the enrolled gallery.
- Current sidecar uses one embedding artifact for both clustering and final voiceprints. Encoder-only comparisons therefore use fixed reference intervals, not a blind replacement of its `--emb` argument.
- All 209 frozen corpus artifacts verified. Main duration set has 768 encoder inputs; six additional development dialogues and six held-out context dialogues use disjoint source recordings. Bins are exact waveform duration, with separately recorded activity estimates.
- Actual current sidecar completed all 18 original/derived cases. Community-1 completed all 18 after the user enabled gated-model access and local Hugging Face authentication. Nemotron Q8, original FP32 and Sortformer comparisons include reset, full-length and CPU/CUDA checks.
- Three official converted CAM++ ONNX artifacts have a partial-window pooling discrepancy against their original PyTorch checkpoints. Separate experimental corrected copies match the reference on 28 parity cases per model; original results remain preserved. Corrected weights are not installed in Bazza.
- Current-policy decisions are checked against the actual Rust matching function; apparent nearest-neighbor quality and accepted names must remain separate metrics.
- A small separately licensed real-Russian control was added because this six-voice synthetic corpus alone cannot support a quality decision. Only verified available files are used; actor/recording-part separation, missing metadata entries and limited enrollment lengths are explicit.

## Verification

`cargo test --offline -p bazza-diarize -p bazza-store` passed: 25 diarization and 57 storage tests. Controlled timestamp checks exercised the actual caption assignment function. Existing historical app acceptance is a separate baseline and is not relabeled as this task's results.

Main comparisons are complete: 17 artifact configurations, including three failed CAM exports retained as diagnostics; 14 usable human/enrollment configurations. Python decisions match the actual Rust function on 12,920 synthetic and 23,730 human-fold queries. Source parity checks, native-enrollment compatibility and independent positive-result audits passed. The [measured report](../test-reports/2026-11-02-speaker-quality-bakeoff.md) records exact denominators, conclusions and coverage.

The 30-minute repeated-scene guard confirmed stable identities and full input coverage, but exposed mixed postprocessing of a compacted native prefix. A separate uniform-history adapter prototype restored every tested short event on CPU and GPU; it is not an application change. Original inputs/model identities passed final checksum verification. Quality scores use clip-placement/activity proxies, not human-annotated DER; timings are indicative rather than a controlled speed ranking.

## Handoff

Task-local utilities and evidence are under the machine-local `voice-bakeoff-20261102` tool root and `20261102-voice-bakeoff` result root. No evaluation work remains. A future integration should prototype full-context Nemotron with existing TitaNet/Voices, preserve uniform long-stream postprocessing, and validate natural real meetings plus application caption/name-confirmation behavior. Short-only naming, wider unseen-person coverage and macOS native acceptance remain limits of these results; the current application/defaults were not changed.
