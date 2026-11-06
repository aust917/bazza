# Voice profiles and managed samples

The **Voices** page extends the existing cross-meeting speaker memory. Profiles
and their speech samples stay in the selected local library. Naming a speaker in
a meeting still requires user confirmation; matching only offers a suggestion.

## Storage and compatibility

Schema migration v6 preserves existing speaker IDs, names and embeddings. It
records the existing embedding as a removable legacy sample without inventing an
audio recording. New confirmed meeting observations retain their meeting/label
provenance, also without copying full meeting audio.

Migration v7 clears only unconfirmed pending suggestions made by the old
nearest-centroid rule. It preserves confirmed identities, profiles, samples and
transcript text; the native migration check is recorded in the
[voice-matching report](test-reports/2026-11-02-voice-matching.md).

Explicitly added samples retain a 1–120 second WAV clip in `voice-samples/` under
the library root, together with the source type, source label, optional meeting
ID, source interval, embedding and model identity. An imported file's basename is
retained; its original absolute path is not stored in sample metadata. Deleting
the source meeting does not delete an explicitly retained voice sample.

The model identity includes the embedding artifact's checksum. The canonical
current identity lives in
[`CURRENT_VOICE_MODEL`](../../crates/bazza-store/src/voice_samples.rs) and must
change when the embedding artifact/backend changes. Matching requests compatible
profiles explicitly. Equal dimensions alone do not establish compatibility.
Adding a sample with a different model or dimension fails without changing the
profile. No automatic conversion of another model's samples is attempted.

Sample addition, removal and explicit recomputation build a normalized centroid
with equal weight per compatible sample. Removing the last sample clears the
voiceprint while keeping the named profile. Scores based on an older centroid
are invalidated. This stored/display centroid is separate from the independent
evidence used to verify new name suggestions.

## Conservative name suggestions

The backend loads compatible profiles through
[`list_voice_match_profiles`](../../crates/bazza-store/src/voice_samples.rs).
Matching needs at least two independent, nonlegacy observations per profile.
The recording currently being matched cannot provide its own verification
evidence. Repeated observations from one meeting count once; meeting samples
without a usable source ID do not count. Reimported/overlapping intervals from
the same file origin and duplicate normalized vectors do not add support.
Disjoint bounded intervals may count separately. File basenames are treated
conservatively because original absolute paths are not stored.

[`best_match`](../../crates/bazza-diarize/src/identify.rs) ranks the nearest profile
before verification. Each eligible profile must have mutually consistent samples;
its floor is derived from the weakest sample's agreement with the centroid of
the other samples, with the unchanged 0.62 global lower bound. A nearest profile
that lacks support or fails verification cannot hand its query to a weaker
candidate. Numerical ties or more than one passing profile produce no suggestion.
The score is cosine similarity, not an identity probability.

Successful evidence mutations clear **all unconfirmed pending suggestions** in
the same store transaction, since changing profile B can change whether A is
nearest or unambiguous. This includes sample addition/removal/recomputation,
profile upsert/deletion and a newly confirmed meeting observation. The global
invalidation itself preserves confirmed identities and transcript text; explicit
profile deletion still clears that profile's own links. A display-only profile rename,
creation of an empty profile or a failed transaction retains pending guesses.
Rerunning diarization derives fresh suggestions from the current gallery.

A legacy voiceprint, a single sample, duplicate imports or an inconsistent sample
set may remain visible while providing insufficient evidence for a suggestion.
The user can add independent samples or correct/remove an erroneous sample.
Existing profiles and audio are retained; this policy does not download models or
send voice data to a provider.

Diarization preserves engine clusters within the eight-speaker limit instead of
merging them solely for short duration or similar embeddings. If an over-limit
merge is needed, every retained contributor must independently verify the same
profile before the merged group receives a known-name suggestion or reusable
enrollment embedding. An individual unknown voice remains available for explicit
user naming. Missing/invalid embeddings retain unknown turns; the sidecar omits
nonfinite, zero or wrong-dimension vectors from
its JSON output. These safeguards do not repair the engine's cluster boundaries
or establish real-person identification quality.

Nemotron queries additionally carry versioned TitaNet evidence for each original
cluster, before any forced merge. Five-second windows (tails of at least one
second) are sampled across four regions of the complete source timeline, up to
16 windows per cluster. No window bridges different turns or repeats/pads speech.
Malformed, missing or numerically invalid scheduled evidence blocks name
suggestions, the microphone-owner shortcut and pooled-query reuse. A positive,
unambiguous nearest profile changing between sampled windows also blocks reuse;
when the aggregate independently verifies a name, every window must agree with it.
Relative ranks never establish a name, and the original verifier is unchanged.

At least two valid windows and coverage of every eligible timeline region are
needed for pooled-query reuse. With only one rankable profile, every window must
also pass the existing identity verifier, and the aggregate must verify that
profile too. A valid original full-pool query with
insufficient observations can retain its previous pending name suggestion, but
cannot teach Voices. Newly recovered queries never use that compatibility rule.
With no compatible gallery, explicit bounded sample enrollment remains available;
meeting naming alone does not supply a reusable pooled observation.

Renaming uses only a checked meeting query, never a stored profile centroid.
Renaming or accepting a suggestion without such a query still changes the label
without adding a voice sample. Merging two labels discards both cached queries;
a later rename cannot turn the combined audio into an enrolled voice. Existing
profiles and individually managed samples are preserved.
The disposable query cache is keyed by the current policy. Older unversioned
meeting queries are not read for learning; rerun speaker detection to derive new
ones. Reprocessing invalidates checked queries before starting and checks final
cache persistence, so a failed/newly rejected result cannot silently retain a
previous reusable vector.

## Explicit sample processing

Meeting diarization uses the downloaded Nemotron 3 Q8 model in the local sidecar,
with one continuous stream and a uniform short-turn policy across native history
compaction. TitaNet Small still extracts up to the first 60 seconds of detected
speech per cluster, using bounded WAV windows. Its artifact, frontend,
normalization, model identity and name-verification rules are unchanged. This
change needs no voice database conversion or schema migration.
The Nemotron voiceprint path verifies the catalog TitaNet checksum before native
initialization. A missing/changed artifact or recoverable extractor-load failure
keeps successful Nemotron turns anonymous; it does not invent a compatible vector.

A valid original full-pool TitaNet vector is retained without changing its bits.
If it is invalid, a pool of at least six seconds can be retried as a complete,
chronological partition with at least two balanced tiles, each 3–30 seconds.
Every tile must be valid and cover the entire original pool exactly once. The
normalized tile vectors are combined by duration using double precision and
normalized again. Failed tiles remain explicit evidence; they are never dropped
to obtain a desirable match. Recovery does not change the model, frontend, saved
voice space, or identity threshold. Explicit sample enrollment keeps its existing
one-voice validation path.

Committed frame positions are checked for gaps/backward movement and complete
audio coverage. If native history cannot compact, the sidecar rejects a retained
window above 360,000 frames (about one hour of uncompacted probabilities), instead
of allocating without bound or publishing a truncated result. This is a retained
history limit, not a fixed recording-duration limit. Truncated WAV input and
nonfinite audio/probabilities also fail explicitly.

pyannote remains the enrollment/validation backend described below. It also
handles meetings until Nemotron is explicitly downloaded (and on macOS before
13.3). An installed Nemotron that fails to load/process produces an optional
stage error; media and existing voice samples remain available.

The user chooses a local audio/video file or a recording and an interval on the
Voices page. FFmpeg converts only that interval to 16 kHz mono PCM. The existing
local pyannote/TitaNet sidecar must return exactly one detected voice with a valid
embedding, including rejecting a second voice too short to produce an embedding.
Missing local models give a visible error; sample creation does not download
models or call an external provider.

Conversion has a 60 second timeout, recognition a 300 second timeout. Process
stdout/stderr are bounded. A failed operation removes its temporary audio and
does not add a database sample. Clip removal stages its file before the database
transaction and restores it on failure. Opening Voices or requesting sample
playback reconciles interrupted deletions and removes inactive temporary/orphan
sample files. Active extraction files are protected from that recovery.

Recording exports include neither voice sample files nor global profile
embeddings. Profile renaming changes future suggestions; existing transcript
labels are edited in the meeting. Deleting a profile clears its links and
suggestions while preserving the transcript text.

## Verification and observed limits

Useful automated checks:

```powershell
cargo test -p bazza-store -p bazza-diarize
cargo test -p bazza --lib commands::speakers::voice_tests
pnpm exec vitest run src/routes/Voices.test.tsx src/components/SpeakersPanel.test.tsx
pnpm typecheck
```

Tests cover migration from v5, compatible sample centroids, deletion/forgetting,
invalid input, ambiguous matching, staged audio rollback/recovery, bounded child
processes, and UI interval/error handling. These checks use synthetic vectors,
files and processes; they do not evaluate a real person's voice.

The [2026-11-02 native Windows follow-up](test-reports/2026-11-02-full-automatic-acceptance.md)
used actual pyannote/TitaNet models with generated voices. It exercised file and
meeting-interval extraction, two profiles with multiple samples, playback,
incorrect-sample removal/recomputation, known-voice suggestions and an unknown
synthetic voice. The full local processing pipeline was also run on a generated
dialogue. These results do not establish recognition quality on real people,
noise or overlapping speech. No threshold/backend change is claimed; other
devices and macOS require their own native validation.

For the separately saved six-speaker Russian Soniox TTS set, follow the
[local corpus guide](test-plans/2026-11-02-soniox-voice-corpus.md). It defines known
versus unknown enrollment, independent sample/podcast text and stress variants.
Corpus generation and audio integrity checks do not constitute Bazza recognition
acceptance; record native results separately before claiming new quality coverage.
The subsequent [native matching report](test-reports/2026-11-02-voice-matching.md)
records the native before/after results. That baseline
confirmed false unknown-name suggestions and codec processing errors. The main
fixed run withheld every tested unknown and removed wrong-name suggestions, but
missed known voices, especially in noisy/codec audio. Main lifecycle/restart checks
passed. The separate confirmation/global-invalidation probe and delivered-build
reopen checks also passed. The report distinguishes the tested executable snapshots
and records the remaining known-voice recall and engine-quality limits.

The later [Nemotron application acceptance](test-reports/2026-11-02-nemotron-integration.md)
keeps the same TitaNet identity and matching policy, with continuous-stream meeting
diarization. Native enrollment, model download, three real-actor gallery rotations,
long-recording continuity, explicit confirmation, failure recovery and controlled
capture/restart were exercised in an isolated Windows app. Contextual short returns
improved on that control; isolated short replies still missed names. The synthetic
mixed scene still produced wrong pending suggestions, and some codec inputs lacked
valid voiceprints. Earlier unknown-rejection results do not transfer automatically
to a new diarizer; user confirmation remains necessary.

The [numerical/coherence follow-up](test-reports/2026-11-02-voice-reliability.md)
recovers all six observed invalid codec pools without changing valid primary
vectors. The recovered codec queries still produce no accepted identity under
the existing verifier. The bounded rank veto removes the observed mixed-cluster
proposal while retaining contextual short-return controls. It still misses
subsecond intrusions and some unknown voices whose local ranks all agree with
one known profile. Isolated short-only identification is unchanged. These are
diagnostic results on reused actors/audio, with separate CLI and actual native-app
evidence; they do not establish universal purity or independent real-world accuracy.
