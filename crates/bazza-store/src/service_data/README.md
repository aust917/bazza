# Service data core

This module is the transactional data API for the service. It does not enroll
legacy `Store` writers: the GUI and native workers retain their existing behavior
until the stage-3 cutover. `bazza-core::Repository` still opens only explicitly
marked synthetic libraries. Private RPC now exposes bounded desktop
content/library/configuration/templates and registered capture/post-process
operations; public MCP CRUD and external grants remain disconnected.

Service schemas append through v16. Ordinary native startup uses
`Store::open_legacy`, which upgrades only through v9 and preserves supported newer
unmarked data without adding later DDL. Normal `bazza.db` opens share the canonical
owner guard; any service identity entry blocks current legacy opens. Historical
binaries that ignore this interlock remain unsupported. Explicit stopped-owner
adoption and the full GUI/worker cutover are still required for production.

The append-only v10 migration creates separate service tables, preserving v1–v9
and every legacy column. Existing meetings are recorded, including recordings
with missing media. Raw timed sources and existing clean/manual overlays are
backfilled without changing the current projection or media files. Summary origin
is conservatively `legacy`, with automatic replacement disabled until an explicit
policy decision. New text meetings are ready without creating a media directory.

Each mutation runs under `BEGIN IMMEDIATE`: expected revision, targeted updates,
search projection, change events and minimal idempotency outcome commit together.
The trusted caller supplies `MutationContext.principal`; it is not a grant or a
tool-input field. The database scopes receipts to one library; principal, tool,
key and canonical argument hash distinguish repeats. Successful outcomes replay
for seven days from the original commit, before checking a now-stale revision.
Conflicting payloads fail. Outcomes contain IDs/revisions/job IDs, not document
bodies. After expiry the old deduplication guarantee ends; replays do not extend it.

Transcript sources are immutable raw snapshots. Replacement archives old overlays;
corrections update only clean text and rebuild chunks once per batch. Empty manual
text is preserved. Permanent deletion erases all source bodies, provider caption
copies, chunks/vectors and affected citations. Persistent absent/deleted state
revisions prevent ABA. Summary replacement archives the previous version; deletion
erases history and retains automatic-generation suppression. Generated commits
check both input and target revisions and cannot overwrite a manual document.

Search backfill is atomic with explicit readiness. Current title/transcript/
summary/user-note documents are indexed together; raw and effective transcript
representations are separate, and identical `both` hits are merged. Cursors bind
to persistent library scope, query/filter/representation and index generation.
Edits reject stale continuations. Lists return note metadata; snippets are limited
to 2048 UTF-8 bytes. Full public filters, opaque cursor encoding, source URIs and
fragment continuation belong to stages 4–5. Internal transcript pages contain
whole segments. Desktop `content.rs` reads add fragment paging for oversized
legacy segments; the older compatibility method keeps its whole-segment contract.

Jobs persist waiting/running/terminal state, cancellation intent and expiring
claim tokens. Reclaim fences old workers. Capture and mandatory cleanup cannot be
cancelled through the generic entry point. Staging writes unique private files,
checks claims/generations before and after I/O, and publishes only after the final
transaction. These are worker artifacts, not the managed share snapshot publisher
planned for stage 6. Permanent deletion revokes matching publication dependencies;
meeting deletion revokes all meeting shares. Ordinary edits retain existing shares.

Meeting deletion hides the tombstone immediately and queues mandatory purge.
Filesystem I/O runs outside the DB mutex with per-artifact locks, fixed relative
paths and symlink/reparse checks. Whole-meeting purge also holds an exclusive
[media guard](../../../bazza-media-gate/README.md) before deleting any recordings,
clips, staged candidates or publications, and keeps it through the final database
commit. A live cooperating read guard or its final clone prevents all those
deletions. Gate Busy/I/O failures leave the job queued for retry; malformed gates
fail before deletion while preserving gate/artifact bytes. UUID gate keys use
canonical spelling to cover Windows path casing without changing SQL IDs or
recorded paths. Content-only cleanup retains its previous scope and locking.
Locked Windows files leave a retryable job;
physical completion is distinct from logical deletion. Calendar cache replacement
is atomic per source; failed refresh preserves the last successful data as stale.
No calendar network request or background job dispatcher is started by this module.

Focused Windows checks for the media-guard purge integration passed 20 Store tests;
native Clippy passed. The primitive's 11-test/Clippy evidence includes an inherited
stdin child holding a gate after parent copies close, but does not prove a full
parent crash or native extra-handle lists. The symlink privilege subcase could not
run with Windows error 1314. Native GUI/worker activation and capture-writer drain
remain separate acceptance gates.

External permissions, automatic dispatch, production adoption and native worker
cutover remain later gates in the
[implementation plan](../../../../docs/dev/plans/2026-11-02-mcp-implementation.md).

Registered post-process transcript input reuses `content::transcript_on` inside
the same deferred transaction that authenticates the current worker and validates
its live binding and original source fences. It cannot select another meeting or
an archive. Renewing a lease changes the job revision without changing the content
cursor's pinned source. Read calls create no receipt or other SQL mutation. The
separate liveness query is a conservative bounded idle-retention signal only;
it does not authorize a worker, replace token/source checks or prove writer drain.

`postprocess_media.rs` adds database-only admission for the private
[seven-file media contract](../../../bazza-service-api/src/postprocess_media.rs),
without a migration. `service_post_process_media_context` uses one deferred read
snapshot: current registered transport PID/session/epoch and post-process
capability precede target reads, followed by the live binding, original
source/lifecycle, expected provider and actual job expiry. The stored expiry must
equal the caller's original lease exactly; a forged longer or shorter expiry is
refused. The bounded context returns persistent library scope, original capture
worker identity, checked confirmed sequence, provider and stored expiry. The
historical capture registration need not still be live.

This method performs no filesystem I/O, SQL mutation, receipt insertion or lease
renewal. Core calls it before filesystem inspection and again afterward, while
holding its own shared media gate and verifying the separate kernel-PID media
owner proof outside recordings. The read-only terminal snapshot never rewrites
the capture journal/owner and must match the stored sequence/provider and capture
identity. Private grant paths are redacted from Debug and excluded from public
MCP DTOs; Store returns no paths. WAV signature screening is not decoding, and raw
video is not a successful processing result. Seven targeted Store media tests
passed, including full-row no-write snapshots, source replacement, renew/reclaim,
forged expiry and original capture-worker identity. Full service-process and
all-target Clippy checks for this increment remain pending. Native engines,
additional inherited `HANDLE_LIST` integration, parent-crash acceptance, writer
drain and the zero-Store production cutover remain open.

The first stage-3 increment appends v11 registered workers/capture leases and
durable finalize receipts in `workers.rs`. The core verifies the released artifact
lock and sealed journal/manifest before any capture commit. Selected provider,
raw captions and one waiting-for-worker processing job commit atomically. Exact
receipts survive service restart and meeting purge; late prepare cannot recreate
the meeting. This does not enroll the existing GUI writers or implement native
processing dispatch, cloud permission epochs or unsealed orphan recovery.

The second stage-3 increment appends v12 desktop preference revision state in
`desktop.sql` and bounded projections in `desktop.rs`. Only locale, close-to-tray
and hotkey can be patched; unrelated settings survive. Rename writes only title
and metadata revision, including while capture is active, preserving lifecycle,
raw text and processing fields. Reads stream a byte-limited prefix and keep a
continuation at an unrepresentable legacy row instead of skipping it. Media
checks happen outside the DB mutex, reject links/reparse points and create no
directories. Scoped change pages reject gaps, including a pruned tail. No new
index changes the legacy meeting-list tie order. These operations do not enroll
legacy GUI writers into service revisions or invalidation.

`content.rs` adds read-only transcript/summary fragments without a schema change.
SQLite slices BLOB bytes before Rust allocation; both current and archived reads
use the selected immutable source and overlays, including source order/count.
The serialized page budget includes JSON escaping and continuation metadata.
Empty manual text, empty summaries and multibyte boundaries retain their meaning.
Consumers supply the expected document revision and persistent scoped cursor.

Legacy/service transcript replacement/deletion and meeting deletion now share a
transaction-local disposable voice-query cleanup helper. It matches complete IDs
in the legacy and policy-versioned namespaces without wildcard/suffix ambiguity;
meeting deletion also removes its highlights. Global voice profiles/samples,
other meetings and cleaned-text-only edits survive. This cleanup is not a worker
lease or enrollment of the remaining legacy GUI writes in service consistency.

Append v14 retains folder/tag revisions independently of change-event retention.
Closed library mutations preserve hierarchy, NOCASE tag and template-assignment
semantics. Template assignment/read and template change events accept exact SQL
IDs up to 512 UTF-8 bytes without NUL; meeting/folder path IDs keep their existing
grammar.

Append v15 combines `templates.sql` and `worker_jobs.sql` in one migration.
Template state/generations survive row deletion and receipt expiry. Explicit
uploads retain identity and intent, verify chunk order/offset/SHA-256 and the
required transcript marker, then commit metadata/body with target CAS in one
transaction. Names, descriptions and bodies preserve exact bytes, including NUL;
existing builtin/created-at values survive updates, builtin deletion is a no-op,
and explicit seeding inserts only missing IDs. New prototype writes are bounded
to an 8 MiB body, 512-byte name, 8192-byte description, 4096 chunks and a 32 KiB
serialized append request. Legacy larger fields use bounded UTF-8 field pages.
Admission allows 16 open uploads and 64 MiB of declared body storage per principal.
After one hour, explicit service housekeeping purges due payloads while retaining
upload identity and retry hashes. The host calls it at startup and every 500 ms;
an indexed due check avoids a write transaction when no upload needs cleanup.
The migration and housekeeping do not seed builtins.

Registered post-process list/claim/renew/fail requires the current transport PID,
worker secret/session/epoch, live registration and post-process capability before
receipt replay. Bindings retain hashes of the client fence, internal job token and
original input snapshot. Every claim rotation retires a previous active binding.
Eligibility uses the finalized capture receipt's lifecycle, exact transcript/source
and a processing meeting; pages pin eligibility time and monotonic change sequence.
Fail checks current ownership and inputs, then commits job/meeting failure,
lifecycle change and minimal receipt atomically without changing raw/media data.
No registered-worker execution, document/media publication or successful job
completion is exposed by those control operations.

`postprocess_result.rs` adds one separate metadata-only automatic-title commit
without a schema change. Shape checks require a nonblank, no-NUL title of at most
512 UTF-8 bytes; the exact text is stored without trimming. Request Debug hides
title text and credentials. Under one `BEGIN IMMEDIATE`, current worker
authorization precedes historical replay; a new result then checks the live
claim/fence/source, expected metadata revision and `title_auto=1`. Manual titles
are refused even when the caller supplies their current metadata revision.

The commit changes only title and metadata revision, refreshing only the title
search document before appending the change event and minimal seven-day receipt.
It preserves `title_auto=1`, raw/provider/clean/manual data, source/lifecycle and
job revisions/leases. `service_idempotency` uses a server-derived worker principal,
operation UUID key and fixed tool `post_process_auto_title`; it does not extend
the claim/renew/fail receipt namespace. Cached outcomes contain no title/body/token
and never refresh ownership. All receipt pins are validated before commit.
Existing file staging remains a separate internal primitive; this result opens
no body/media publication, execution, success or job completion.

Append v16 separates immutable capture anchors from current per-job input
checkpoints. Original capture payload, finalize receipt and provider remain
unchanged; the anchor fingerprints their bounded metadata. Current lifecycle,
transcript/source/document state, phase and opaque checkpoint revision are stored
separately. Descriptors, leases and binding hashes include the required checkpoint
revision. Candidate reads validate both the original anchor and current live
state without adopting a missing or externally changed input.

Migration backfills only candidates that pass the previous strict finalized
capture checks. Malformed or stale candidates remain unavailable; operational
SQLite failures roll back the migration. Old active post-process bindings are
retired, and running post-process jobs become interrupted with rotated revision
and cleared token/expiry. Generic jobs and historical receipt bytes are preserved.
New successful capture finalization initializes its checkpoint in the same
transaction as captions, source, job and finalize receipt.

`postprocess_cleanup.rs` adds closed Begin/Append/Status/Commit/Abort operations
for a whole automatic-cleanup result. Private limits are an 8 MiB encoded body,
10,000 decoded updates, 32 KiB serialized requests, 4096 chunks, and a one-hour
upload TTL. Admission reserves at most 16 open uploads and 64 MiB server-wide.
Each append has an exact sequence, byte offset and SHA-256; staged fragments are
never visible as cleaned transcript. These prototype bounds do not redefine
ordinary native recording or cleanup behavior.

Every operation authenticates the current registered worker before global
worker/tool/operation receipt replay. Exact replies are immutable historical
metadata, including an earlier Open status; obtaining current status requires a
new operation ID and current binding. Fresh operations check the actual lease
expiry, claim/fence and checkpoint. Explicit renewal can retain the same upload
only for the same claim/input; reclaim cannot adopt it. A retained upload UUID
and intent never reopen after payload retirement or ordinary receipt expiry.

Commit validates the complete body before applying its ordered sparse updates in
one transaction. Duplicate segment IDs use their last value; missing/foreign IDs
are scoped skips. Either representation's manual flag protects its overlay,
including manual empty text. Automatic nonnull empty text also stays empty;
whitespace and NUL are preserved. Legacy and canonical automatic projections
change together, then chunks/FTS/search rebuild once. Raw text, timings, speakers,
original Soniox/Local provider and STT settings, voice-query caches, saved voice
profiles/samples and media remain unchanged.

The same transaction advances `captured` to `automatic_cleanup` once, rotates
checkpoint/job revision and internal token/binding hash, and returns a minimal
receipt with the new lease. Claim/fence, actual expiry and reclaim attempts remain
unchanged. A truthful no-op preserves transcript revision while still fencing old
callbacks. Commit scrubs its payload and invalidates other uploads from the old
checkpoint; abort/expiry also scrub payloads. Permanent transcript or meeting
deletion clears all dependent cleanup payloads and invalidates checkpoints in the
delete transaction. Retained identities, append hashes and seven-day receipts
contain no transcript text; Debug omits staged text.

This opens no native inference or successful job completion. A future lifecycle
checkpoint must preserve native Ready-before-optional-cleanup ordering without
treating Ready as all jobs finished. Ordinary GUI/worker activation remains a
separate cutover. Targeted checks passed four checkpoint tests, sixteen Store
cleanup tests and two actual cleanup IPC tests. The combined schema16 lean suite
passed 558 tests with one ignored helper; all-targets Clippy passed. Native runtime
acceptance and the historical packaged GUI scope are recorded separately in the
stage-3 report.

## Private chat/history reads (schema17)

Append17 adds durable chat metadata/messages/erasure revisions, chat/message
generations, retained tombstones and byte counts, observed by passive SQL
triggers. Reads never initialize or repair these projections. Five closed
operations expose bounded headers and exact UTF-8 fields, preserving empty/NUL
values and arbitrary stored citation JSON. Exact scopes distinguish SQL NULL
from empty text; AllCatalog/LikeChat reach giant legacy scope metadata without
allocating it for a filter or hashing bodies on each read.

Pagination seeks original SQLite timestamp text through the pinned rowid and
uses the native rusqlite date decoder for headers. It never assumes UTC-wire
timestamps or rowids alone define ordering. Erasure fences every native
library-wide scope, including meeting/None and pending empty assistants, while
retaining history and the existing citation scrub semantics. Future completion
writes must consume these fences. Native chat writes/events are not rerouted.

Twelve Store and two owned-process chat cases passed; the combined schema17
suite passed 590 tests with one ignored helper and lean/native all-targets Clippy.

## Private automatic local ASR publication (schema18)

Append18 extends the closed checkpoint phases, preserves existing rows, and
adds immutable local source lineage keyed by source and job. Existing staged
cleanup identities are tagged as cleanup. Local ASR and cleanup share the
same 16-open/64 MiB quota, expiry, append verification and scrub rules. A
payload or UUID for one kind cannot be adopted by the other.

For an originally local finalized capture, a registered live claim can install
one exact result under its captured checkpoint. The transaction retains the
original immutable source and overlays, replaces current segments and STT
projection, rebuilds chunks/FTS/search, and clears meeting query voice caches.
Saved voice profiles, samples and the original capture selection stay intact.
Complete-empty and no-model each have an explicit current source and distinct
outcomes. The no-model label records an attempted quality ID. Public provenance
displays an actual model for a complete result.

Commit advances to `local_asr`, rotates checkpoint/job revision and token with
the same actual expiry, records a bodyless receipt, and scrubs sibling uploads.
Cleanup can advance the new source with the new lease. Reclaim preserves the
current stage and local result while checking the original capture anchor
separately. The meeting remains Processing. This is a fixture publication
boundary and engine execution and Ready transition still follow in the plan.

## Anonymous speaker outcome and Ready (schema19)

Append19 adds source-owned nullable speaker overlays and an immutable job
outcome. Existing checkpoints, upload kinds, chunks and receipts are copied
with foreign keys enabled. Speaker staging shares the existing byte and
upload quotas and accepts assignments only in the current local ASR phase.
Coverage follows immutable ASR order, including out-of-order timestamps.
Anonymous labels are derived in first-appearance order. An explicit null
overlay clears a previous source label. Raw text, timing and cleaned
representations are retained.

Commit updates the current segment projection, unconfirmed speaker rows,
affected search and retrieval projections and the transcript/checkpoint pins.
The meeting's query voice caches are cleared. Saved profiles and samples are
outside this operation. A visible no-op retains the transcript revision but
persists the speaker outcome and rotates the worker lease. Archival source
overlays remain available until the owning transcript is erased.

The separate Ready transaction checks the registered claim and stored
speaker outcome, sets meeting status and rotates the lifecycle/checkpoint/job
revisions while retaining the active claim and original expiry. Optional
cleanup and auto-title can continue with the returned lease. The fatal
processing failure transition only applies while the meeting is Processing.
Historical fixture cleanup before this new sequence remains available on
the earlier phases. Native engine routing and job completion are separate.

## Generated summary and fixture job completion (schema20)

Append20 preserves the staged upload tables and adds independent summary and
completion evidence. The generated summary shares the existing staging quotas
and uses a 2 MiB body limit. Publication uses the current post-cleanup lease
for a nonempty local transcript and checks the summary revision and template
revision and generation. Manual and external summaries and manual removal
remain protected. The body, search projection, summary marker and minimal
receipt commit together.

Completion requires saved post-Ready cleanup or a verified empty transcript.
It records the cleanup and summary outcomes, records embeddings and webhook
as outside the fixture scope, marks the job succeeded, retires the claim and
scrubs staged bodies in one transaction. The meeting remains Ready. Historical
receipts remain readable by exact replay after worker authentication. The
fixture summary and structure reads use bounded public projections and
epoch-bound cursors.
