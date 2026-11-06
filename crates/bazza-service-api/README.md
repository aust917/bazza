# Bazza service API contracts

This Tauri-free crate contains executable schemas for the 29 future public MCP
tools and a separate experimental private application RPC contract. It does not
register MCP handlers, open a library, start recording, or advertise those 29
tools as implemented service capabilities. The three received-shared-meeting
tools reserved for stage 9 are excluded from this catalog.

Private application contracts additionally cover bounded desktop content,
library organization, templates, reserved/begun capture handoff, registered
post-process claims and private media inputs. These methods do not
become public MCP tools. Folder/tag revisions survive event pruning; page and
mutation validators bind reply kind, identity, operation, target and byte budget.
The [shared domain models](../bazza-domain/README.md) retain private native/bundle
shapes independently of these public projections.

[FixtureRead](src/fixture_read.rs) is a closed bridge for nine read tools on the
explicit synthetic service. It validates public input and output contracts
inside a 65,024-byte private payload budget. Its request enum has no mutation,
worker, admin or share operation. The [adapter](../bazza-mcp/README.md) exposes
only these implemented tools over stdio.

Private native configuration uses a redacted domain value. It may
contain secrets and is never a public MCP projection. The handshake fingerprint
includes the domain sources that define its enum encodings/defaults. Provider and
consent changes through this settings endpoint remain gated. The private
Soniox worker RPC issues a short permit and a redacted credential for an
authorized fixture capture. Renewal returns no credential. Drain acknowledgment
is bound to the original permission and capture. External MCP frames cannot
request any of these operations.

The [contract boundary](../../docs/dev/research/2026-11-02-mcp-contract-boundary.md)
owns the public names, roles and library-grant semantics. The
[implementation plan](../../docs/dev/plans/2026-11-02-mcp-implementation.md) owns
the rollout gates and acceptance requirements.

## Public schemas and validation

[Typed inputs](src/inputs.rs), [output projections](src/outputs.rs) and
[bounded value types](src/types.rs) generate self-contained JSON Schema 2020-12
through pinned Schemars 1.2.2. `tool_schemas()` returns the stable ordered catalog
with `name`, `input_schema` and `output_schema` for every base tool.

Inputs and nested domain objects deny unknown properties. Arrays and strings
have explicit bounds. Output envelopes contain `schema_version`, `library_id`,
`warnings`, and exactly one of `data` or `error`; success and domain-error branches
are disjoint. Error DTOs expose safe codes/messages and retry/revision metadata,
without accepting private settings, credentials or machine paths. A failure may
have a null library ID when discovery/authentication has not selected a library.

Use `validate_tool_input(name, &input)` before deserializing public inputs.
`validate_tool_output(name, &output)` validates responses;
`validate_tool_output_for_input(name, &input, &output)` additionally checks the
requested page limit, byte limit, representation/format and explicit share-URL
request. Serde deserialization alone does not enforce the complete contract:
the validators apply real Draft 2020-12 schemas, timestamp formats and semantic
checks such as interval ordering, distinct correction targets and nonempty
typed patches. Errors do not repeat supplied text.

The pinned jsonschema 0.56.0 dependency has HTTP/file resolution features disabled.
Schema validation uses embedded definitions and performs no network/file fetch.
Future handlers must also enforce service-side grants, expected revisions,
idempotency, current library state and worker fencing; shape validation does not
grant authority or implement transactions.

## Initial bounds and continuation

| Boundary | Limit |
| --- | --- |
| Serialized JSON request / response | 1 MiB each, counted without allocating an oversized serialization |
| Document mutation body | 256 KiB UTF-8 per Markdown/encoded-JSON body |
| Transcript/note/summary read page | 64 KiB UTF-8 content; transcript raw + effective text share this budget |
| Transcript read page | At most 100 segments |
| Timed transcript mutation | At most 1000 real timed segments |
| Atomic correction batch | At most 100 set/reset operations |
| One segment/correction text fragment | 64 KiB UTF-8 |
| Ordinary list/search page and ID arrays | At most 100 items; page limit defaults to 20 |
| Participant arrays | At most 64 labels |

JSON Schema `maxLength` counts Unicode characters, so semantic byte checks are
also required. `slice_utf8_page()` uses byte offsets that must lie on UTF-8
boundaries, returns the next offset when needed, and rejects a budget too small
for the next character. It never advances through a broken character or stalls
on an empty fragment. Wire pages expose bounded opaque cursors and truncation;
long transcript segments also carry a segment continuation descriptor.

Ordinary corrections preserve original raw text and change only cleaned overlays,
including a deliberately empty overlay or explicit reset. Import/replace accept
real timed segments rather than inventing timestamps for plain text. Summary
`structured_json` is bounded encoded JSON, validated on write and read through
the selected paged format, preserving arbitrary legacy structures.

## Permissions and retry contract

`tool_permission()` defines the future service policy separately from caller
inputs:

| Operations | Library permission |
| --- | --- |
| Get/list/search | `read` |
| Create meeting/note | `create` |
| Update metadata/transcript/note, write summary, refresh calendar, request job cancellation | `update` |
| Delete meeting/transcript/summary/note | `delete` |
| Create/update/delete share | `share` |

`bazza_list_shares(include_url=true)` additionally requires `share`; the response
validator rejects an unrequested URL. Job cancellation also requires the
original job's authority and public cancellable-kind policy. Creating a summary
is an update of an existing meeting's persistent summary state. The service
assigns summary provenance, and public writes only return `manual` or
`external_agent` with `auto_replace=false`.

All writes/delete/start-job inputs require `idempotency_key`. Update/delete and
summary/transcript state mutations require `expected_revision`, including absent
or deleted state, to prevent stale ABA writes. Deduplication is scoped by
principal, library, tool and normalized arguments. The fixed target is seven
days from the first durable outcome; retries do not extend it. Receipts expose
`dedup_expires_at` and the target constant is `DEDUP_RETENTION_SECONDS=604800`.
The stage-2 [data core](../bazza-store/src/service_data/README.md) now persists
minimal receipts and verifies replay/expiry on synthetic libraries. The service
still needs handlers for public mutations and external permissions.

## Experimental private RPC

[rpc](src/rpc.rs) is a separate typed application contract with its own
`rpc::ServiceStatus`. It is not the public MCP `outputs::ServiceStatus` projection
and does not use a public external grant. Transport authenticates the OS user;
the wire request cannot select a role. This development boundary is restricted
to explicitly marked synthetic fixtures.

After `Hello`, the original private operations are `GetStatus`,
`GetMeeting` metadata read and `Shutdown`. Stage 3 also adds strict
[worker messages](src/worker.rs): register/renew and
prepare/begin/abort/reattach/finalize capture.
The host supplies the authenticated peer PID; finalize carries identities and
fence, with terminal/journal evidence read by core from locked local artifacts.
Secrets are redacted in Debug and never become public MCP input fields.
The `experimental_fixture_capture_local_live` capability advertises support
for retained Local live captions in the version-3 terminal and the
`captured_live` post-process stage. That stage keeps the confirmed raw source
and speaker labels while settling media and running optional follow-up work.
The second stage-3 increment adds strict [desktop messages](src/desktop.rs):
bootstrap, read/update preferences, meeting page/header, change page and rename.
Preferences expose only UI locale, close-to-tray and hotkey, with expected revision
and idempotency on writes. Meeting IDs remain safe opaque legacy strings. Replies
carry library/epoch identity; pages are limited to 100 items and a 64 KiB envelope.
The 65,024-byte data cap leaves room for that envelope. Cursors bind persistent
scope, query and change sequence; change-feed gaps require resynchronization.
These are private desktop projections, not the public MCP list/get projections.

The `desktop_transcript_versions` capability lets the private desktop read
the original and view-only alternatives and save a completed manual result.
One bounded transfer belongs to the authenticated application socket. Uploads
are staged in memory and committed only with the original meeting lifecycle
and transcript revisions. Disconnecting before commit discards staging. Manual
variants do not replace the canonical transcript used by search, chat or edits.

The current fixture status advertises status, metadata, worker handoff, desktop
repository/content/library, native configuration, templates and post-process
capabilities, all under the `experimental_fixture_` prefix. Shutdown is a private
control operation. API/schema/build/library/epoch checks
and request correlation apply independently of the future public schema catalog.
Private RPC framing has its own smaller transport budget; the 1 MiB public
request/response budget does not enlarge it. `STORAGE_SCHEMA_MAX=22` is shared by
the pure private client, with a core test checking agreement with Store.

[Content messages](src/content.rs) add private transcript and summary reads.
Transcript requests pin the current document revision and select raw or effective
text plus an optional archived source. Both representations read the immutable
source and its overlays. Cursors advance by source segment index and UTF-8 byte
offset; an explicitly empty manual value remains a real segment. Summary Markdown
and stored structured JSON text have independent byte continuations. No handler
assembles or parses a whole large structured summary for a page. Serialized JSON
escaping, metadata and the actual continuation share the same desktop budget.
The extra capability is `experimental_fixture_desktop_content`. The separate
fixture adapter exposes public read projections through `FixtureRead`.

[Chat messages](src/chats.rs) add the private `DesktopChats` read family and
`experimental_fixture_desktop_chats` capability. Closed list, describe, messages,
chat-field and message-field operations expose no send, completion, cancellation
or write operation. Requests and complete tagged reply data fit 65,024 serialized
bytes; list pages contain at most 100 headers. Headers use bounded previews and
full UTF-8 byte counts, with separate chat metadata/messages/erasure revisions
and chat/message generations. Field pages preserve exact title, scope, role,
content and stored citation-JSON bytes, including NUL and empty strings. Citation
fragments may be malformed or incomplete JSON; this layer does not parse them.

Scopes preserve exact kind/optional-ID pairs. `None`, an empty ID and an ID
containing whitespace or NUL remain distinct. Exact filters allow 128-byte kinds
and 8192-byte scope IDs; all-catalog and generation-pinned like-chat selection
also reach legacy keys beyond those filter bounds. SQL chat/message IDs are
nonempty, at most 512 UTF-8 bytes and contain no NUL; they are not path names.
Large metadata remains available through field pages.

Pagination keeps SQLite's original timestamp ordering and rowid tie behavior.
Canonical signed-i64 strings carry row anchors, including zero, negative and
values beyond JavaScript's safe integer range. A continuation resolves the raw
stored timestamp from its row anchor rather than seeking with the header's
UTC-normalized date. Reply validation checks unique rows, exact final anchors,
query/revision/generation pins and advancing UTF-8 byte offsets. Field cursors
also pin total bytes and optional-field presence; absent scope IDs and present
empty IDs cannot be confused. An empty assistant row remains a real row and does
not imply that generation is running. This read contract leaves native token/done
events and the staged, fenced completion boundary separate.

[Template messages](src/templates.rs) add closed list/describe/field-read,
begin/append/commit/abort, delete and explicit seed-missing-builtins operations.
Field pages preserve exact UTF-8 bytes, including NUL in name, description and
body. Template IDs are SQL identities of at most 512 UTF-8 bytes without NUL;
Unicode, whitespace and punctuation do not relax the recording-path ID grammar.
Header previews expose their byte lengths, and large legacy fields remain paged.
New writes are a private prototype with an 8 MiB body, 512-byte name, 8192-byte
description, at most 4096 chunks and a 32 KiB serialized append request. These
write limits do not redefine existing native template behavior. Upload identity
and target generations remain fenced after body cleanup; durable receipts and
append fingerprints contain no template text. Builtin deletion is a successful
no-op, and seeding never overwrites an existing custom collision.

[Post-process messages](src/postprocess.rs) provide only list, claim, renew and
fail for eligible finalized-capture jobs. Each operation authenticates the actual
peer PID, current worker session/epoch, unexpired registration and `post_process`
capability. Mutations additionally bind the original lifecycle/transcript/source,
client fence and internal claim-token hash. Historical receipts do not renew a
lease. Lists pin an eligibility time and change high-watermark across pages;
failure updates only a currently owned processing job and its meeting lifecycle.
Native execution, provider permission, document/media publication and successful
job completion remain unopened boundaries.

Schema-16 private input fences require `checkpoint_revision` in addition to the
immutable capture identity and current lifecycle/transcript/source pins. It is a
bounded revision with no deserialization default. Legacy v15 claim/renew receipts
that lack this field remain incompatible history; a service must not fill them
from today's checkpoint or thereby refresh their authority. Capture finalize
receipts retain their existing contract. This identity change alone opens no
checkpoint advancement or generic rebind operation.

[Staged automatic cleanup](src/postprocess_cleanup.rs) opens a separate private
`PostProcessCleanup` family under `experimental_fixture_post_process_cleanup`.
Closed begin/append/status/commit/abort operations carry a registered claim,
fence, operation UUID and upload UUID; callers cannot select raw/manual fields,
paths, providers or lifecycle transitions. Each serialized request fits 32 KiB,
including the lease and JSON escaping. The exact UTF-8 upload is a JSON update
array of at most 8 MiB and 10,000 positive JavaScript-safe segment IDs, with at
most 4096 append fragments. Empty text and NUL remain exact, duplicate IDs use
their final ordered value, absent IDs are skipped and manual overlays are
preserved. The server reserves at most 16 open uploads and 64 MiB across workers;
body lifetime is one hour.

Upload replies expose bounded progress and a terminal state without text or
hashes. They are inert snapshots: retrying an operation UUID returns its original
receipt; a new status operation UUID is required for fresh progress. A committed
receipt reports applied/no-op and the changed segment count, with a new checkpoint
and job revision but the same claim, capture/source/lifecycle pins and exact lease
expiry. Only an applied result changes the transcript revision. Both outcomes
fence old callbacks; neither renews authority, completes the job or publishes
media. Historical replies remain shape-valid after expiry and every new action
must authenticate before historical replay or live binding checks. Debug hides
cleanup text, hashes and worker secrets. This family provides automatic overlays,
not a generic source replacement or rebind operation.

[Post-process transcript input](src/postprocess_input.rs) is a separate private
read call bound to a currently live registered claim and its original transcript
source. It reuses bounded raw/effective content pages; archives, another meeting
and replaced sources are refused. Every page reauthenticates. This opens no media
path, native executor or output/success operation.

[Post-process media inputs](src/postprocess_media.rs) add the private
`PostProcessMedia` operation and `experimental_fixture_post_process_media`
capability. Requests carry the original live lease/fence, expected provider and
guard-owner nonce; they accept no path, filename or role selector. The closed
artifact kinds map to exactly these captured inputs:

| Kind | Fixed filename |
| --- | --- |
| `audio_mix` | `mix.wav` |
| `audio_mic` | `mic.wav` |
| `audio_system` | `sys.wav` |
| `raw_video` | `screen.raw.mp4` |
| `video_timeline` | `screen.raw.timeline.json` |
| `video_clock` | `screen.raw.clock.txt` |
| `audio_clock` | `mix.clock.json` |

A grant requires exactly one mix and at most seven unique kinds. Absent optional
files are omitted; an existing empty optional file reports zero bytes. Absolute
private paths are bounded to 4096 UTF-8 bytes without NUL and must use their fixed
filename. Byte sizes are JavaScript-safe integers; the complete serialized grant
fits the 65,024-byte data budget. Debug hides private paths. This private
application DTO must never become a public MCP projection.

Core authenticates the registered PID/session/epoch, live claim, original source,
provider and exact stored lease expiry before filesystem access and rechecks the
same context afterward. A guard-bound kernel-PID challenge proves the matching
local media owner outside recordings. The terminal snapshot is read without
journal or capture-owner rewrites and compared with the stored capture identity,
confirmed sequence and provider. The mix check is only a RIFF/WAVE signature,
not a decoder; raw video availability does not establish processing success.
Windows reader-child pins and parent-crash behavior have synthetic acceptance;
native engine adoption, writer drain and production zero-Store cutover remain
open. The media increment passed full owned-process and all-target Clippy checks;
the stage-3 report records current checkpoint/cleanup acceptance separately.

[Automatic-title results](src/postprocess_result.rs) add the separate private
`CommitPostProcessAutoTitle` operation. It accepts an original worker lease/fence,
operation UUID, expected metadata revision and exact title of at most 512 UTF-8
bytes. A title must be nonblank after trimming and contain no NUL; validation
does not trim the stored text. The request's Debug representation hides title
text and worker secrets.

Current worker authorization precedes historical receipt replay. A new result
also requires the live original claim/source fences, metadata CAS and
`title_auto=1`; a manual title is refused even with its current revision.
The commit updates title metadata and its search projection atomically, preserving
`title_auto=1`, job revision/lease, transcript/source and lifecycle. Its minimal
seven-day receipt contains identities/revisions, not title or document text.
The fixed `service_idempotency` tool `post_process_auto_title` is separate from
claim/renew/fail receipts. This metadata result does not expose body/media staging,
provider execution, success or job completion.

[Staged local ASR results](src/postprocess_source.rs) use the separate private
`PostProcessSource` RPC. The registered worker streams one exact UTF-8 JSON
body, with an 8 MiB budget and at most 10,000 final captions. Begin, Append,
Status, Commit and Abort use the same framing, expiry and shared quota as
automatic cleanup. A stored upload has a fixed kind.

The service accepts this result only for a live claim on an originally local
capture in the captured phase. The body carries the actual engine model for a
complete result and the attempted quality model for a no-model result. These
terminal outcomes remain distinct for empty caption arrays. The new receipt
contains the outcome, caption count and rotated lease with its original expiry.
The original capture selection and lifecycle remain pinned. Fresh job
descriptors expose the current stage and local ASR outcome for recovery, while
original operation receipts retain their historical meaning.

[Staged speaker results](src/postprocess_speakers.rs) accept an ordered,
complete assignment list for the current local ASR source or a closed skip
reason. Cluster IDs carry no personal names or profile references. The service
assigns anonymous labels in first-appearance order and handles an explicit
unassigned segment. The same source and lifecycle remain pinned. A changed
label rotates the transcript revision, while either outcome advances the
checkpoint and returns a lease with the original expiry.

[MarkPostProcessReady](src/postprocess_ready.rs) is a separate registered
worker action. It requires the recorded speaker outcome on the current
local source and the active lease. It changes the meeting lifecycle and
returns a new lease for optional processing with the same transcript and
expiry. Ready and post-Ready cleanup are visible in fresh claim descriptors.
The fixture job remains running after Ready.

[Generated summary results](src/postprocess_summary.rs) reuse the private
staging protocol with a separate upload kind and a 2 MiB body limit. They
require saved post-Ready cleanup and pin the source, transcript, target summary
revision, template revision and template generation. The bodyless receipt
records the installed revision. Fresh Ready descriptors expose that revision
as durable evidence for the current source.

[Fixture completion](src/postprocess_complete.rs) consumes the live lease after
post-Ready cleanup or for a verified empty transcript. It returns persisted
cleanup and summary outcomes, with embeddings and webhook outside the fixture
scope. Completion retires the claim. Exact authenticated replay returns a
historical receipt with no lease.

## Review and checks

From the repository root:

```powershell
cargo run -p bazza-service-api --example export_schemas
cargo test -p bazza-service-api --test contracts
```

The example writes a JSON review artifact to stdout with `contract_only=true`,
empty `implemented_capabilities`, and the schema catalog. It is not an MCP stdio
server. Adversarial tests cover all 29 schemas, both output branches, unknown
fields, required revisions/keys, bounds, malformed times, atomic corrections,
UTF-8 paging, explicit share URLs and permission policy.

Generator/validator behavior follows the verified primary documentation for
[Schemars 1.2.2](https://docs.rs/schemars/1.2.2/schemars/generate/struct.SchemaSettings.html)
and [jsonschema 0.56.0](https://docs.rs/jsonschema/0.56.0/jsonschema/draft202012/index.html).
