# Synthetic MCP service fixtures

`create_v8(&new_directory)` creates a disposable, deterministic Bazza library.
The directory must not exist, its parent must exist, and existing data is never
overwritten. The generator does not discover a default library, read a pointer,
capture audio, start a network connection, load a model, or run inference.

The frozen SQL contains released migrations v1–v8. It deliberately does not
follow the current Store migrations, so tests exercise a real version upgrade.
The marker `.bazza-synthetic-fixture` contains exactly
`bazza-mcp-v8-fixture-v1` followed by a newline. A marker identifies test data;
it is not an authorization credential or proof that an arbitrary path is safe.

The fixture includes:

- A ready recorded meeting with RU/EN raw text, no overlay, a manual correction,
  an intentionally empty manual correction, and an automatic cleaned overlay.
- A failed recorded meeting whose audio is missing, a recording-status orphan,
  and a failed meeting with deliberately corrupt WAV bytes.
- Fixed timestamps, participants/calendar linkage, folders/tags, effective
  retrieval chunks and vectors, summaries, a chat and citations.
- Enabled meeting and clip shares plus a disabled share, a confirmed synthetic
  speaker, a voice sample and a provider-neutral Soniox provenance setting.
- Valid deterministic PCM WAV files for the ready meeting, orphan and sample.
  The MP3 clip is a deliberately corrupt sentinel for file lifecycle checks.

V8 has no typed summary origin, user-note repository or transcript-source
history. Summary origins are represented only by explicit synthetic model and
structured-JSON markers; they do not claim production origin semantics. No
real key, credential, personal recording, transcript or embedding is included.
The small artificial vectors exercise persistence, not voice recognition.

Run `cargo test -p bazza-service-test-support` from the repository root. The
tests check v8, refusal to overwrite, migration/reopen preservation of raw and
manual/empty overlays, provider provenance, summaries, shares and voice samples,
and byte-for-byte preservation of all generated media. These tests do not
validate capture hardware, actual MP3 playback, service detachment or MCP clients.
