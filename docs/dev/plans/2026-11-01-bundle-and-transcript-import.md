# Reliable bundle and transcript import

Status: complete — software and synthetic native checks; manual playback acceptance remains.
Owner: local import work; no publication.

## Outcome and boundaries

Roadmap stage 3 / R6–R7: preserve version-1 `.hark` packages while validating
untrusted input, make each meeting retryable, and import media with existing
SRT/WebVTT transcripts without mandatory model processing. See
[user behavior and limits](../../import.md).

## Implementation

- [x] Validate version, canonical UUID IDs, allowed media paths, declared/nonempty
  files, duplicate IDs/entries, related IDs and resource limits before extraction.
- [x] Stage media, save all associated metadata in one SQLite transaction, install
  files with rollback, preserve unrelated destination files and support interrupted
  import retry with an importer-owned pending marker.
- [x] Reclaim pre-install crash staging on a later import using durable ownership
  markers and exclusive OS leases. Preserve active imports and unmarked folders.
- [x] Preserve raw/clean text, speakers, summary, tags/highlights and retrieval
  chunks and optional selected STT provenance. Skip existing IDs; do not import
  global voice links/embeddings or credentials.
- [x] Add UTF-8 SRT/WebVTT parsing, timestamp validation and explicit Library
  media-plus-transcript flow. Ready transcript imports bypass post-call processing.
- [x] Add roundtrip/negative/corrupt-media/rollback/retry tests and a synthetic
  FFmpeg test that imports then reopens a temporary library without inference.
- [x] Complete combined frontend/Tauri checks after parallel contracts settle.
- [ ] Perform native dialog/playback acceptance with the user.

## Verification

Current implementation passed 16 `hark-share` and 25 `hark-store` release unit tests,
including the concurrent voice migration tests. Release Clippy for both crates
with all targets and warnings denied passed.

Added negative tests exposed that zip 2 indexes entries by name and collapses
duplicates. The importer now scans bounded raw central-directory records before
reading payloads. A corrected media corruption fixture exercises CRC failure.
EOCD/ZIP64 preflight bounds entry counts before the ZIP index is allocated; a
ZIP64 directory fixture verifies compatibility. Large media exports opt into ZIP64.
Roundtrip includes optional Soniox provenance, rejects partial selected captions
and strips extra setting fields. Tests also create a managed voice sample and fake
private credentials, then verify neither is included in the package.
Meeting deletion also removes selected and previous STT settings transactionally;
its regression verifies unrelated meetings and private credentials are preserved.
Two staging regressions cover a process-interruption equivalent (keep TempDir,
release its lease), an active lease and unrelated/incorrect ownership markers.

The ignored `media_and_transcript_import_survives_library_reopen_without_inference`
test requires `HARK_TEST_FFMPEG` and generates a silent synthetic WAV. It opens no
capture devices and makes no provider calls. Real media playback remains separate.
The test passed in the combined verification; the library reopened with its raw
text, speaker label and search index intact. See the [combined report](../test-reports/2026-11-01-independent-product-work.md).

## Handoff

IPC uses the existing `import_recording` command with an optional `transcriptPath`;
media-only callers retain existing behavior. The current-format ZIP limits are
explicit compatibility choices. Subtitles containing multiple voice spans in one
cue are rejected rather than assigning multiple voices to one speaker.
Legacy staging directories without the new ownership marker are not reclaimed:
their origin cannot safely be established. A crash before the new marker is
durable can leave an empty directory; no media is written before marker creation.
