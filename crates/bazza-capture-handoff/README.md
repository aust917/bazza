# Capture artifact handoff

Tauri-free primitives for a capture worker and the service's eventual recovery
path. They do not start capture, recovery, network access, models or database
writes. The crate has no default data-directory lookup and opens only an explicit,
existing recording directory whose leaf equals the meeting UUID. The caller
must first resolve that directory beneath its canonical library root. The
development fixture gate remains the service's responsibility.

`ArtifactGuard::acquire(directory, identity)` takes a persistent exclusive OS file
lock. Keep this non-cloneable guard until every audio/video/caption writer has
closed and joined. An expired service heartbeat does not release this lock.
Recovery takes the same lock through `SealedCaptureGuard` and cannot proceed
while a live worker holds it. Never unlink or replace the lock file, including
after a crash.
`ArtifactGuard::probe` reports Free or Busy with bounded owner metadata. The
metadata is nonsecret and is not an authentication credential. A live guard also
owns a bounded local proof listener: verification checks the kernel-reported
server PID, fresh challenge, capture identity and rendezvous nonce. The proof
listener closes before the artifact lock is released. The service separately
validates its authenticated peer, worker credentials and capture fence.

`append_confirmed` appends hash-chained, framed raw provider segments and returns
an ACK only after `sync_all`. Sequence IDs start at one. Exact retries return the
same digest; gaps, conflicting replays and changes of provider/model fail.
The journal is bounded at 64 MiB / 200,000 records, each frame at 64 KiB and raw
text at 32 KiB UTF-8. A complete malformed/altered frame fails closed; only an
incomplete final frame is recoverable and removed before subsequent append/seal.
Checksums provide integrity detection, not authentication against a compromised
process of the same OS user. Text is preserved verbatim, never cleaned here.

After stopping/joining raw writers, `publish_terminal` syncs declared fixed media
files and atomically publishes a bounded version-1 manifest. It records library,
meeting, worker session and capture generation UUIDs, selected provider/model,
duration/end time, completion/failure state, bounded error code, fixed media availability and the last
confirmed journal sequence. It cannot nominate paths, shell commands, credentials
or URLs. A successful terminal seal is immutable and idempotent; later caption
appends fail. Windows publication uses non-replacing `MoveFileExW` with
`MOVEFILE_WRITE_THROUGH`; Unix also syncs the parent directory after publication.

`publish_terminal_with_evidence` uses version 2 at the same terminal path.
It can record the screen writer outcome and lengths of fixed raw files, a
closed Soniox outcome, or both. For screen evidence it syncs the present
`screen.raw.mp4`, `screen.raw.timeline.json`, `screen.raw.clock.txt` and
`mix.clock.json`. Empty files and missing files are distinct. Raw screen
evidence leaves the published video flag false. The Soniox outcome describes
provider finalization independently of the audio writer and requires a selected
Soniox model. Version 1 remains readable, including older Soniox seals.

`publish_terminal_with_local_live` uses version 3 for a nonempty journal of
completed Local live captions. It preserves optional raw screen evidence and
marks the confirmed journal as the initial transcript source. Version 1 and 2
seals keep their original interpretation.
`append_local_live_confirmed` acknowledges only synced rows. If a write or sync
fails, retry and sealing verify the acknowledged prefix and discard the failed
append's bytes, including a complete frame that the live view never confirmed.

The service uses `SealedCaptureGuard` for finalize and sealed recovery. It opens
the existing lock without creating files, publishing owner metadata, or starting
a proof listener. It scans one bounded frame at a time and requires the journal
to match the seal through EOF. It can also verify raw evidence against the
fixed files while retaining ownership. A partial tail is rejected after a seal and can
still be repaired by the unsealed writer. The service keeps the lock through
database publication. Database finalize/dedup/job registration, worker credentials,
permission epochs and live capture reattach are separate service responsibilities.
No production recorder is switched to this protocol just by adding this crate.

The fixture service uses a reserved → begun protocol. A worker holds its guard
and receives Begin ACK before starting raw writers. An unused pristine reservation
can be compensated without deleting files; a begun capture cannot use that path.
Prepared-reservation compensation runs only during startup reconciliation, while
ongoing handoff recovery handles begun captures. Neither relies on a heartbeat
as evidence that a raw writer has stopped.

The separate `media_read::MediaReadOwner` acquires a shared
[media gate](../bazza-media-gate/README.md) and publishes a unique nonce-bound
proof listener outside recordings. `verify_owner` checks the kernel-reported
server PID, fresh challenge and exact library/epoch/claim/checkpoint/source identity. Job
revision is deliberately absent from that identity so explicit claim renewal can
reuse an owner; the service still requires the current job receipt. Metadata is
only rendezvous data and cannot authorize media access. Drop joins the proof
server and removes its metadata before releasing the gate. A cloned read file
retains that gate until its final copy closes; callers must not explicitly unlock
it. The SDK keeps this owner inside its guard-bearing result.

Media-reader metadata/proofs and their rendezvous endpoint namespace now use
version 2. `MediaReadIdentity.checkpoint_revision` is a required bounded revision;
missing/invalid checkpoints and version-1 proofs fail closed. Explicit renewal
may reuse an owner while its checkpoint stays unchanged; a checkpoint advance,
including a no-op phase advance, requires a new owner identity. Persistent media
gate keys are unchanged so older retained reader files still prevent purge.

`read_terminal_snapshot(directory, capture_identity)` reads the bounded common
terminal fields. `read_terminal_snapshot_with_evidence` also returns historical
raw screen and Soniox evidence. These functions do not acquire `ArtifactGuard`, reopen or repair the
confirmed journal, or rewrite capture-owner metadata. Full journal validation
already occurred at finalize. The caller must retain its media gate, compare the
stored confirmed sequence and provider/model, and authenticate the live registered
claim/source/provider and exact stored expiry both before filesystem access and
afterward. The [private media contract](../bazza-service-api/src/postprocess_media.rs)
allows only seven fixed captured files; its private paths are Debug-redacted and
never public MCP DTOs. Core's mix check is a RIFF/WAVE signature, not a decoder;
raw video availability is not processing success.

Ten targeted Windows media proof/terminal tests passed. This does not establish native
engine integration, additional child `HANDLE_LIST` inheritance, full media-reader
parent-crash acceptance, writer drain or production zero-Store routing. Full
service-process and all-target Clippy checks for the combined media increment
remain pending.

```powershell
cargo test -p bazza-capture-handoff
```

Capture artifact tests use synthetic files. They cover live-owner exclusion,
owned process crash,
journal retry/corruption/torn tails, terminal identity/provider/version checks,
fixed media publication, bounds and a real Windows directory junction. The child
test entry is inert unless its parent test supplies its isolated temporary root.
