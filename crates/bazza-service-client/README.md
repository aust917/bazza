# Private service and repository clients

This crate has no Store, SQLite, Tauri, capture engine or model dependency.
`ServiceClient` implements strict authenticated local RPC. `RepositoryClient`
is the cloneable desktop facade over that connection. The current service opens
only explicitly marked synthetic libraries; the production GUI still has its
legacy Store until all writers are migrated.

The private client accepts storage through schema 15. Ordinary native startup
uses the legacy schema-9 opener and shares the current-build owner lock; a
persistent service identity refuses legacy access even after the service stops.
The client does not adopt a production library or bypass that interlock.

For portable export consumers, `bazza-share` with `default-features = false`
provides HTML rendering and subtitle parsing without Store/SQLite. Its default
`backend` feature retains the existing bundle and LAN server APIs.

## RepositoryClient

`connect(root)` selects an explicit existing canonical library. It neither creates
a library nor follows the application's default-data pointer. The separate
`connect_or_start_fixture(root)` uses the configured verified session broker;
it has no ordinary child-process or SQLite fallback.

The facade serializes calls and reconnect through one async mutex. Every read and
mutation returns `RepositorySnapshot<T>` with the selected library ID, service
epoch and persistent cursor scope. Pages carry the same identity, including final
pages without a continuation. A UI consumer must discard an old snapshot after
reconnect and compare entity revisions before applying asynchronous results.

`reconnect()` is explicit. It drops the old connection first, checks the original
library ID and cursor scope, and accepts a fresh epoch. A failed reconnect leaves
the facade disconnected. It does not silently select another library or replay
a mutation. `identity()` describes the selected context, not connection health.

The first desktop boundary provides:

- Bootstrap and narrow preferences: UI locale, close-to-tray and hotkey.
- A bounded meeting list with folder/subtree, tag and content-kind filters.
- A meeting header with metadata/lifecycle/transcript/summary revisions and media
  availability flags; it contains no nominated media paths.
- A bounded consecutive change feed that detects retention gaps.
- Targeted title rename and preference patch with expected revision and durable
  idempotency outcome.
- Raw/effective transcript fragments and Markdown/structured-JSON summary pages.
  Content reads require the header's expected document revision. Archived source
  selection is explicit; source text never comes from mutable legacy projections.
  Manual empty overlays and multibyte byte offsets are preserved. The facade binds
  every continuation to library/epoch/scope and never assembles a whole document.
- Mixed folder/tag pages and closed organization mutations, with durable revisions
  independent of change-feed retention. Atomic folder updates, cycle checks,
  template assignment, meeting moves and named-tag association preserve legacy
  organization semantics without rebuilding transcript bodies.
- Template list/describe and exact UTF-8 field pages, plus explicit upload
  begin/append/commit/abort, delete and seed-missing-builtins requests. Template
  continuations bind library/epoch/scope. Callers choose each page, chunk and retry;
  the facade never assembles a whole body or replaces the original target revision.
  Template IDs retain Unicode/whitespace SQL identity through folder assignments
  and change events. New prototype writes have an 8 MiB body, 512-byte name and
  8192-byte description; larger legacy fields remain readable through pages.

The separate private native configuration call transports the native settings
value. Its revision binds the exact legacy blob and shared preference
state; full and narrow edits invalidate each other. Unknown saved fields survive
updates and receipts contain no settings. The bounded hotkey guard protects
bootstrap. Provider/consent changes are temporarily forbidden until distributed
cloud cancellation is implemented; no native side effects are applied here.

It does not provide generic setting keys, cloud credentials or capture controls.
Preference patches preserve unrelated legacy settings. Mutations return a minimal
receipt, not a post-commit full object that could mix different revisions. Retry
with the same key and immutable arguments after an ambiguous response; a stale
revision with a new key is a conflict. Never fetch a newer revision merely to
overwrite it with a stale UI draft.

## Pages and errors

Private desktop envelopes are at most 64 KiB; data pages reserve envelope overhead
and are at most 65,024 bytes and 100 items. The transport's independent 128 KiB
frame limit remains unchanged. Meeting continuation binds library/epoch/scope,
normalized query, index/change snapshot and offset. A change cursor advances only
through consecutive events. Changes, retention gaps, foreign scope or old epoch
require refetch/resynchronization rather than mixing pages. The client never
automatically collects an unbounded library.

An oversized or malformed legacy header fails explicitly when encountered; its
stored text is not silently shortened. Diagnostic text alone has a truthful
truncation flag. Media flags are volatile filesystem availability, not a decoder
or immutable media snapshot. The private diagnostic `error` field may contain
legacy paths and must not be reused as an unsanitized public MCP error.

`ServiceClient` validates envelope identity, item/byte limits, continuation progress,
reply kind and correlation. Bad replies, partial frames, cancellation, timeout or
EOF poison the stream; use explicit reconnect. Domain conflicts retain a correctly
framed connection. All desktop mutations run as the trusted local application
principal, stable across its windows/processes; the wire cannot choose an actor
or grant itself authority. External MCP grants remain a separate later boundary.

Registered post-process calls live on `ServiceClient`, outside the desktop facade.
The caller retains its verified `RegisteredWorker`, proof and immutable original
request. Reply validators bind each page or receipt to its request, worker/epoch
and snapshot or operation/lease fields; malformed replies poison the connection. A historical
receipt is inert and never refreshes a lease. These calls expose no executor,
provider authorization, internal claim token or job completion. The separate
automatic-title result below opens only a metadata commit.

`validate_fixture_root(root)` additionally checks an absolute existing canonical
directory and its exact bounded regular synthetic marker, without creating a
library. A fixture GUI must call it before connecting even when a service already
runs, and again on explicit reconnect. A successful local handshake alone is not
a substitute for selecting the marked development fixture. `api` re-exports the
pure contract crate for native adapters without a Store dependency.

`ServiceClient::post_process_transcript` takes the retained registration, proof,
live lease/fence and explicit bounded content request. It validates the original
input source on all pages and reuses content progress/budget checks. Renewed
leases may resume an unchanged source cursor; the client does not renew, retry,
refresh a source or assemble the transcript implicitly. This worker call is not
part of the trusted desktop `RepositoryClient` facade.

`ServiceClient::acquire_post_process_media(worker, root, input)` returns
`OwnedPostProcessMedia`, which retains a local `MediaReadOwner` and lends its
private grant through `grant()` or `envelope()`. The helper checks the registered
current PID, explicit canonical root and selected service endpoint before creating
the owner. The service checks claim/source/provider and exact stored expiry before
filesystem access and again afterward. The owner proves its kernel PID and fresh
nonce outside recordings; this is separate from capture artifact ownership.

The [closed media contract](../bazza-service-api/src/postprocess_media.rs) exposes
only `mix.wav`, `mic.wav`, `sys.wav`, `screen.raw.mp4`,
`screen.raw.timeline.json`, `screen.raw.clock.txt` and `mix.clock.json`. Exactly
one mix is required; missing optionals are omitted and empty optionals report zero
bytes. The SDK validates the grant's identities, source, provider, nonce, revision,
expiry, budget and fixed paths beneath the selected recording directory. Paths
are private and redacted from Debug, never public MCP DTO fields. A RIFF/WAVE
signature check is not decoding, and raw video is not a successful job result.

Retain the returned owner for the whole media use. `try_clone_file()` can retain
the shared gate after the SDK object drops; ownership ends only when the final
copy closes. Do not explicitly unlock a clone. The low-level
`ServiceClient::post_process_media` requires a separately retained matching owner;
grant metadata alone does not pin files. Neither call implicitly renews, retries,
refreshes a source or starts a native engine. Windows `OwnedMediaChild` now has
synthetic parent-crash and handle-lifetime acceptance; native engines still need
to adopt it. Writer drain and the production zero-Store cutover remain open.
The media increment passed the full owned-process suite and all-target Clippy;
the stage-3 report records its exact scope.

`ServiceClient::commit_post_process_auto_title` takes the retained registration
and immutable `CommitPostProcessAutoTitleRequest`, including the original lease,
fence, operation UUID and expected metadata revision. Titles preserve exact bytes,
are nonblank/no-NUL and are limited to 512 UTF-8 bytes so desktop headers remain
readable. The client validates the minimal receipt's identity, operation and new
metadata revision. It never refreshes metadata, retries or applies native effects
implicitly; request Debug omits the title text and credentials.

The service authenticates before replay, then checks live ownership and metadata
CAS for a new result. A manual title stays protected. Accepted automatic titles
retain `title_auto=1` and leave job/source/lifecycle revisions unchanged. Receipts
use the separate fixed `service_idempotency` tool `post_process_auto_title` for
seven days and contain no title/body. No body/media staging, execution, success or
job completion is connected by this result.

Schema16 worker inputs require a service-owned `checkpoint_revision`, including
media owner proofs. Renew keeps it; accepted cleanup advances it even for a no-op.
The private `post_process_cleanup` call stages an exact UTF-8 JSON body through
closed Begin/Append/Status/Commit/Abort operations. Its receipt carries the new
immutable lease with the same actual expiry. Manual overlays and raw text remain
protected. Callers explicitly keep the new lease; the client never retries,
rebases or renews authority implicitly. Historical upload snapshots and commit
receipts grant no current authority. These prototype limits do not change native
capture or cleanup defaults, and no native cleanup executor is activated here.

Schema18 adds `ServiceClient::post_process_source` for staged local ASR
publication. Keep the returned lease for subsequent reads and writes. The
new lease pins the installed transcript source, while media admission still
checks the original captured provider and terminal evidence. The SDK requires
a matching live worker registration and validates the receipt before releasing
it to the caller. The caller can repeat the exact original request for its
historical receipt.
After a fresh claim, the listed stage and local outcome distinguish work
remaining after the service restarts.

Schema19 adds `post_process_speakers` and `mark_post_process_ready`.
Use the new lease returned by each accepted action. Speaker results retain
the current raw source and publish anonymous labels or a closed skip reason.
Ready rotates the lifecycle revision while leaving the optional job live.
An exact replay returns the original receipt. Fresh claim descriptors after
restart carry the speaker outcome and whether cleanup already followed Ready.

Schema20 adds `post_process_summary` and `complete_fixture_post_process`.
Stage the exact generated Markdown and structured JSON against the current
post-cleanup lease and the expected summary and template state. The receipt
identifies the installed summary revision. Completion accepts the current
lease after cleanup or for a verified empty transcript. It reports persisted
cleanup and summary evidence, retires the claim and returns a bodyless receipt.
An exact retry after a lost reply returns that receipt. Keep the original
request for replay. Fresh Ready claims report an already committed summary
revision so workers can resume without regenerating it.

## Verification

Private chat/history reads use `ServiceClient::desktop_chats` and the closed
`RepositoryClient::chats` / `continue_chats` facade. Each call returns one page.
Opaque continuations retain library/epoch/scope, the initial query and exact
issued cursor, generations/revisions and field byte totals/presence. They do not
collect fields or retry/rebase stale reads. Titles, roles, scope metadata and raw
citation JSON can be read through UTF-8 fragments; native interpretation stays
with the later application adapter. These calls provide no send/completion path.

```powershell
cargo test -p bazza-service-client -p bazza-core -p bazza-service -p bazza-store
```

The [stage-3 report](../../docs/dev/test-reports/2026-11-02-mcp-phase-3.md) records
the verified fixture scope. Full GUI routing, remaining vector/import
transport, native worker dispatch and production zero-direct-write acceptance
remain open gates in the [plan](../../docs/dev/plans/2026-11-02-mcp-implementation.md).

Endpoint discovery and broker setup manifests publish completed same-directory
tempfiles atomically. Windows replacement errors 5/32/33 retry the same file for
at most 250 ms; persistent failure preserves the old complete target. The helper
never removes, truncates or opens the destination for writing. Controlled handle
tests verify both eventual replacement and bounded failure.
