# Voice profiles and managed samples

The **Voices** page extends the existing cross-meeting speaker memory. Profiles
and their speech samples stay in the selected local library. Naming a speaker in
a meeting still requires user confirmation; matching only offers a suggestion.

## Storage and compatibility

Schema migration v6 preserves existing speaker IDs, names and embeddings. It
records the existing embedding as a removable legacy sample without inventing an
audio recording. New confirmed meeting observations retain their meeting/label
provenance, also without copying full meeting audio.

Explicitly added samples retain a 1–120 second WAV clip in `voice-samples/` under
the library root, together with the source type, source label, optional meeting
ID, source interval, embedding and model identity. An imported file's basename is
retained; its original absolute path is not stored in sample metadata. Deleting
the source meeting does not delete an explicitly retained voice sample.

The model identity includes the embedding artifact's checksum. The canonical
current identity lives in
[`CURRENT_VOICE_MODEL`](../../crates/hark-store/src/voice_samples.rs) and must
change when the embedding artifact/backend changes. Matching requests compatible
profiles explicitly. Equal dimensions alone do not establish compatibility.
Adding a sample with a different model or dimension fails without changing the
profile. No automatic conversion of another model's samples is attempted.

Sample addition, removal and explicit recomputation build a normalized centroid
with equal weight per compatible sample. Removing the last sample clears the
voiceprint while keeping the named profile. Scores based on an older centroid
are invalidated. Numerically tied matches remain unknown; the existing matching
threshold and diarization speaker limit are unchanged pending quality evaluation.

## Explicit sample processing

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

## Verification and remaining acceptance

Useful automated checks:

```powershell
cargo test -p hark-store -p hark-diarize
cargo test -p hark --lib commands::speakers::voice_tests
pnpm exec vitest run src/routes/Voices.test.tsx src/components/SpeakersPanel.test.tsx
pnpm typecheck
```

Tests cover migration from v5, compatible sample centroids, deletion/forgetting,
invalid input, ambiguous matching, staged audio rollback/recovery, bounded child
processes, and UI interval/error handling. These checks use synthetic vectors,
files and processes; they do not evaluate a real person's voice.

Real model extraction and recognition across two control recordings still need
separate acceptance, including unknown speakers, multiple samples and correction
of an incorrect sample. No quality margin or alternative diarization backend is
claimed without those measurements. Other devices and macOS require their own
native validation.
