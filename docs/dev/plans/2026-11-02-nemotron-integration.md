# Nemotron 3 integration

Status: complete on Windows. Owner: speaker-recognition session, 2026-11-02.

## Scope and decisions

- Use the tested Nemotron 3 Q8 model for meeting diarization in one continuous
  stream. Keep TitaNet Small, existing voice profiles, samples and matching policy.
- Keep pyannote segmentation for voice-sample validation/enrollment and compatibility
  with installations that have not downloaded Nemotron. Do not change recording.
- Read long WAVs in bounded windows, maintain absolute frame positions through
  native history compaction, and apply the same turn policy to the entire stream.
- Load pinned CPU native libraries in the diarization sidecar, isolated from the
  application's llama/Whisper runtime. No Python production dependency.
- Keep data local. New model downloads remain an explicit existing user action.

## Coordination

The MCP session owns Store/service/API/client/core/transport. Its native baseline
passed (42 tests, one FFmpeg-dependent test ignored) before handing the shared
native build slot, state/pipeline and packaging to this session. Do not modify its
Store files, stop the running user GUI or change the user's library pointer.

## Acceptance

1. Legacy enrollment and existing TitaNet identities remain compatible.
2. Native engine checks cover stream continuity, compaction, invalid values,
   fixed eight-channel output and consistent turn postprocessing.
3. Real human short/context fixtures and the 1813.5-second repeated conversation
   run through the shipped sidecar, with full timeline and speaker continuity.
4. Model catalog, downloads and packaged runtime resolve in the native application.
5. Actual application import/reprocessing uses Nemotron, retains voice data and
   conservative unknown handling. Use an isolated acceptance library/runtime.
6. Report checks actually run and platform/hardware limits explicitly.

## Evidence and next action

Pre-integration evidence: [quality bakeoff](../test-reports/2026-11-02-speaker-quality-bakeoff.md).
Runtime/test inputs remain under ignored `.git/local-tests` and `.git/local-tools`.

- Sidecar contract, bounded WAV input/TitaNet pooling, continuous Nemotron routing,
  catalog download and isolated native runtime packaging implemented.
- Native lifecycle testing found a v0.3.0 DLL-unload crash after successful
  inference. The DLL family is pinned until sidecar exit; stream/model cleanup,
  two jobs, a fresh model and clean process exit were tested.
- Independent reviews found two optional-encoder failure paths. The CLI now
  verifies the canonical TitaNet artifact before unsafe native initialization and
  keeps successful anonymous turns on recoverable failure. App orchestration
  passes the canonical path even when the file is absent/truncated.
- Actual sidecar: 1813.5 seconds, 753 turns, three stable IDs, all 26 blocks
  retain 15/15 scored short events. Native enrollment galleries: 30/30 known and
  15/15 unknown-withheld events across three rotations of the same three actors.
- Rust: 26 diarization, 4 model, 62 Store and 9 ownership checks passed. CLI:
  15 regular checks plus separately invoked real-model lifecycle test passed;
  Clippy passed. Final-source Tauri run: all 46 checks passed with FFmpeg enabled.
- TypeScript, targeted 14 UI checks and production frontend build passed. Runtime
  scripts passed syntax checks; six Node checks passed with the real Windows ZIP,
  one Mac symlink check was skipped because this Windows account lacks permission.
- Isolated native GUI successfully downloaded Nemotron using the real button and
  verified its SHA; existing profile/sample vectors stayed unchanged. Windows
  computer-use inspection confirmed the three distinct installed speaker models.
- Actual application human controls: all three fresh galleries retained 30/30
  known short returns and withheld 15/15 unknown returns with context. Short-only
  controls retained 0/30 known names; absence of an unknown name is descriptive,
  not a calibrated rejection result. Anchors are scored separately.
- The final app processed the 1813.5-second repeat in about 45 seconds, retained
  the full timeline and answered 169 status probes (maximum observed 4 ms). This
  was a post-confirmation functional replay, with an extra observation from the
  same source scene in the gallery; it is not fresh held-out recognition evidence.
- Explicit name confirmation and restart retained original sample IDs/vectors;
  the confirmation legitimately added one meeting observation and recomputed the
  profile centroid. Test settings' native level meters ran transient RMS probes;
  the attempted JavaScript stub was ineffective and is not claimed as isolation.

The [native application report](../test-reports/2026-11-02-nemotron-integration.md)
records 69 queued processing jobs, all 52 synthetic regressions, final-app model
failure/recovery, explicit name confirmation/restart and controlled virtual-device
capture through Whisper/Nemotron. All owned instances are stopped; the user app and
all 14 observed ordinary runtime files match the saved baseline. The new production
runtime is frozen separately under the ignored integration evidence directory.

Quality limits remain explicit: short-only natural controls accepted 0/30 known
names; a mixed synthetic cluster produced wrong pending names. On one codec input,
direct C-API inspection reproduced all-NaN Tita outputs from valid 60-second pools;
the guard correctly withheld those vectors. This is not an import failure or
evidence that ordinary compression inherently fails. No matcher tuning or synthetic
unknown-specific calibration was introduced.

Completion evidence: `final-results.json`, `final-integrity.json`,
`quality-independent-audit.json`, `codec-independent-check/`,
`packaging-final-audit.json` and `build-provenance.json` in the ignored evidence root.
macOS native execution, natural long meetings and broader independent voice sessions
remain outside this Windows acceptance. There is no remote publication or deployment.
