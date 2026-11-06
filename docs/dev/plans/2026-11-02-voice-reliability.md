# TitaNet recovery and mixed-cluster naming

Status: complete. Owner: speaker-recognition session, 2026-11-02.

## Requested outcome

The user authorized two follow-ups to [Nemotron integration](../test-reports/2026-11-02-nemotron-integration.md):
recover usable TitaNet voiceprints when a valid speech pool produces nonfinite
output, and check within-cluster voice consistency before proposing a known name.
Short-only identification calibration is a separate, unrequested third item.

Preserve the TitaNet artifact/frontend/voice space, existing profiles and samples,
current identity-verification thresholds, transcript text and capture behavior.
Keep anonymous turns when evidence is invalid or contradictory. A mixed average
must not enter reusable voice memory through explicit naming or the Me shortcut.
All source and artifacts stay local.

## Coordination and ownership

MCP phase 3 owns new capture-handoff/service/API/core modules and Store service
schema. It explicitly released diarize/CLI/protocol and pipeline voice matching.
Do not edit its Store/schema/service/capture/recorder/STT code. No new dependencies
are planned; coordinate any manifest hunk in advance. Native builds use a separate
ignored target and runtime. Leave the running user app and normal library alone.

## Evidence before implementation

- Freeze source and input hashes. Keep source audio/labels separate from inference.
- Numerical policy uses duration/validity only, never a desirable name or reference
  identity. Preserve the existing full-pool vector when it is valid.
- Distinguish numerical recovery of the first bounded speech pool from coherence
  observations sampled across the complete original cluster timeline.
- Freeze DEV inputs and selection rules before comparing policies. The already
  observed human/synthetic failures are regression diagnostics, not blind tests.
- Count valid vectors, available speech and sampled coverage separately. Missing
  vectors do not establish successful unknown rejection. Cues sharing a cluster
  vector do not count as independent matching trials.

## Acceptance gates

1. Reproduce the known NaN pools with the unchanged artifact; compare deterministic
   shorter-window candidates on codec, clean and human controls. Measure CPU/memory.
2. Compare label-free coherence and conservative per-window identity checks on DEV;
   retain valid known-name behavior while reducing mixed-cluster false proposals.
3. Version evidence by original cluster ID. Preserve every scheduled window,
   invalid reason and unique coverage; malformed/absent evidence cannot masquerade
   as proven homogeneity. Unknown identity remains separate from consistency.
4. Apply a naming/reusable-vector veto through consolidation and the Me shortcut;
   retain anonymous turns, original text and existing voice memory.
5. Run meaningful pure/native tests and actual isolated app regressions, including
   profile/sample retention, failure/insufficient evidence and restart. Update
   owning documentation with exact results and limits.

## Current evidence and next action

Baseline and experiments live under ignored `.git/local-tests/20261102-voice-reliability/`
and `.git/local-tools/20261102-voice-reliability/`. Full-first/complete-partition
numerical recovery and the bounded rank-veto/primary-compatibility policy are
implemented. Native evidence retains original cluster IDs through consolidation;
separate gates protect naming/Me and reusable observations. Policy-versioned
queries cannot fall back to older unverified caches or profile centroids; merges
discard both disposable queries, and reprocessing checks cache invalidation/writes.

The immutable packaged Windows acceptance build is `acceptance/runtime.next1/`;
the normal-identifier counterpart is `runtime-production.next1/`. Both include the
same freshly frozen sidecar. The earlier raw-Cargo builds lacked custom-protocol
and are diagnostic failures, not accepted runtimes. Tests and isolated native-app
acceptance are being recorded in the [follow-up report](../test-reports/2026-11-02-voice-reliability.md).
Completed: 58 pure tests, 20 CLI tests plus one real model/runtime/reset test,
53 native tests including FFmpeg, strict scoped Clippy, 543 final-source policy
replays, six paired final CLI regressions, and a raw long CLI geometry audit.
Packaged app acceptance passed 24 recorded run groups / 270 assertions, including
all four library startups/restarts, unchanged original profiles/21 sample vectors,
one explicit valid query observation, cache/confirmation/merge/Me/failure guards,
Meeting/Voices UI, contextual short returns and 1813.5-second continuity. Exact
hashes, source distinctions and denominators are in the report.

No required work remains for these two points. Codec identity recall remains
0/6 despite finite recovery; short-only identification is unchanged. Sampled
agreement misses subsecond/uninspected mixtures and some unknown voices; the
report records counterexamples and lost known suggestions. The user's running
app, normal runtime/library and MCP-owned source remain unchanged. The new
normal-identifier runtime is ready separately; no source or build was published.
