# Import recordings

Imports stay in the local library. Importing a ready transcript does not start
transcription, diarization, summary generation, agents or webhooks.

In the desktop Library, drop an audio or video file from Explorer to start the
same import as the **Import** button. The imported meeting appears in All
meetings while transcription continues. Library views backed by the service
currently have no media import controls.

## Portable Bazza packages

Use the existing export/import controls for a version-1 package with the
`.bazza` extension and `app: "bazza"` in the manifest. Other extensions and
product identifiers are rejected before import. A package preserves media, original and cleaned transcript
text, timestamps, speaker labels, summary, tags and highlights. Selected STT
provider/model/language and confirmed transcript state are preserved when present.
Manual cleaned-text corrections, including empty text, travel separately from
the original with `clean_text_manual`. This additive v1 field defaults false
when absent; a manual flag without a cleaned value is rejected. Imported manual
corrections retain protection from automatic cleanup. Explicit re-transcription
replaces segments and their corrections after a warning in the meeting UI.
Folder membership
and links to your local voice profiles are not transferred. Voice samples and
embeddings are not included.

Packages may also include an additive typed transcript-version archive. It
preserves the initial subtitles, alternative versions, per-row audio provenance
and the published combined result with current cleaned corrections. Old v1
packages remain readable. Version history is validated before import and stored
in the same transaction as the meeting; local canonical IDs are remapped.
Participant WAVs, worker permissions and private settings are not part of that
archive. Once a combined result is published, create a new transcript version
instead of replacing its raw text through the older re-transcription action.

An existing meeting ID is skipped. Other meetings in the same package can still
be imported; the report separates imported meetings, existing meetings and errors.
An invalid package structure is rejected before files are written. File or database
failure rolls back the affected meeting so the corrected package can be retried.
Unrelated files already occupying its destination are preserved.
New staging directories carry an ownership marker and an exclusive file lease.
A later import that stages media reclaims abandoned, marked directories after a
process interruption; active imports and unmarked folders are preserved.
Unmarked staging folders from older builds cannot be safely identified and are
left for manual inspection.
Finish recording and processing before exporting: active meetings cannot be
exported or imported as a package. Ready and failed meetings are portable.

Version 1 accepts `mix.wav`, optional `mic.wav` / `sys.wav`, and optional
`screen.mp4`. The mix and all declared files must be present and nonempty.
Video is included only when the meeting marks it as available. A retained file
from a failed video capture stays local; audio can still be exported and imported.
Limits are 1,000 meetings, a 16 MiB manifest, 16 GiB per media file and 64 GiB total
uncompressed media/metadata. Unsupported versions, paths and duplicate archive
entries are rejected. Media-only export accurately reports that video was omitted.
The exporter supports ZIP64 for media larger than 4 GiB. The optional STT metadata
uses an optional version-1 field; Bazza packages may omit it.
Only documented transcript fields are copied; private settings and API keys are
never copied as provider provenance.

## Media with an existing transcript

In Library, choose **Media + transcript**, select one audio/video file, then its
UTF-8 SRT or WebVTT file. The media is converted into the library's normal playback
files, and the supplied transcript is saved as original text. No model is needed.
Ordinary **Import** continues to run the normal media transcription pipeline.

Subtitle files may be up to 16 MiB / 100,000 cues. Cues need chronological start
times, positive durations and timestamps within the selected media's duration.
Overlapping speech is allowed. A mismatched file is rejected without trimming
the transcript to fit.

WebVTT spans such as `<v Ann>Hello</v>` become the speaker label `Ann` and original
text `Hello`. A cue with several voice spans must first be split into separate
cues. SRT has no standard speaker field: prefixes such as `Ann: Hello` remain in
the original text. These labels do not create entries in the voice database.
Corrected SRT export omits deliberately blank corrected rows and renumbers the
remaining cues; original-text export still uses the original words. A transcript
with all corrected rows blank produces no subtitle cues.

Automatic checks cover parsing, damaged packages, rollback, retry, metadata/media
roundtrip, abandoned/active staging and reopening a temporary library.
[Windows application checks](dev/test-reports/2026-11-02-automated-product-acceptance.md)
also cover synthetic SRT/WebVTT imports through native IPC, actual H.264/AAC
playback and seek before/after restarting the tested build.
[Windows package checks](dev/test-reports/2026-11-02-bundle-runtime-acceptance.md)
also cover package transfer between isolated libraries, metadata/media integrity,
partial failure, transaction rollback, retry, duplicate skipping and native
playback/restart.
The [2026-11-02 native UI follow-up](dev/test-reports/2026-11-02-full-automatic-acceptance.md)
also exercised SRT/WebVTT file choosers, cancellation at both selection steps,
package import/repeat and ShareDialog export using artificial fixtures. All four
exported media hashes matched; manifest comparison accounts for cleared local
folder/profile links and regenerated database segment IDs.
Other codecs, real user files, very large packages and interruption/power loss
during an import remain separate acceptance work.

The [Bazza rename checks](dev/plans/2026-11-02-bazza-rename.md) separately cover
`.bazza` output and rejection of unsupported formats. Historical native results
above retain their recorded hashes and measurements; editorial name changes do
not claim acceptance of a later executable or file-format policy.
