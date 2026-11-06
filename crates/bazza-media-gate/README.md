# Bazza media gate

This primitive coordinates media readers and purge owners. Store whole-meeting
purge now acquires its exclusive guard before deleting owned artifacts. The crate
itself does not authorize a worker, expose media, enforce TTL, delete files, or
activate a normal GUI path.

`MediaReadGuard::acquire(root, meeting_id)` and
`MediaPurgeGuard::acquire(root, meeting_id)` require an absolute existing ordinary
library directory, resolved canonically before access. Only the fixed
`.bazza-service-media-gates` directory and empty gate files may be created.
Names are lowercase SHA256 of `b"bazza-media-gate-v1\0"` followed by the exact
meeting ID UTF-8 bytes, with `.gate` appended. IDs contain 1–128 bytes and no NUL;
opaque IDs and Unicode retain their exact spelling. Existing malformed entries
are rejected and preserved. Callers must never replace or unlink gates or their
directory.

On Windows readers open read access with `FILE_SHARE_READ`; an exclusive owner
opens read/write with no sharing. On Unix readers and exclusive owners use
nonblocking shared/exclusive file locks. Clones and explicitly inherited read
files retain ownership until their final handle/file-description closes. Do not
explicitly unlock a returned file or any clone. Unix locks coordinate cooperating
processes; they are not a filesystem security sandbox.

The synthetic child test passes a read file as stdin and drops every configured
parent copy before checking exclusion. It proves this limited inherited-file
lifetime, not native extra-handle lists, full parent-crash behavior, capture-writer
drainage, or native GUI/worker activation.

Store `service_run_purge` holds `MediaPurgeGuard` for a whole meeting before
removing the first recording, clip, staged candidate or publication, and retains
it through the final database commit. Gate I/O runs outside the database mutex.
A busy or I/O-failed gate leaves cleanup queued for retry; malformed gates fail
before deletion and preserve the existing gate and artifact bytes. Content-only
cleanup keeps its existing scope and per-stage locking.

The Store integration parses meeting UUIDs into their canonical spelling for the
gate key while retaining the original SQL ID and recorded paths. This prevents
uppercase/lowercase UUID paths that alias on Windows from selecting different
gates. The primitive itself continues to hash the exact supplied ID bytes, so
callers coordinating the same meeting must agree on the same identity spelling.

The Windows primitive suite passed 11 tests and Clippy. Its symlink-creation
privilege subcase was unavailable with Windows error 1314 and is not claimed as
exercised. Focused Store integration checks passed 20 tests, and native Clippy
passed. These results cover this primitive and purge boundary; they do not
establish native reader activation, parent-crash inheritance or capture drain.
