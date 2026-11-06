# Experimental local service (stages 1–3)

The host and private client operate only on explicitly marked synthetic fixtures.
The read-only [MCP fixture adapter](../bazza-mcp/README.md) uses a closed
fixture read projection over this service.
The GUI still owns its production Store until the stage-3 cutover. No production
library default, recording, model loading/download, external provider, calendar,
or share listener is enabled by this host.

## Reproduce on Windows

Run from the repository root. Use a new path whose parent already exists:

```powershell
$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('bazza-service-' + [Guid]::NewGuid())
cargo run -p bazza-service-test-support --example create_v8 -- $fixtureRoot
cargo run -p bazza-service -- migrate-fixture --library $fixtureRoot
cargo run -p bazza-service -- register-fixture-broker --library $fixtureRoot
cargo run -p bazza-service -- start-fixture --library $fixtureRoot
cargo run -p bazza-service -- get-meeting --library $fixtureRoot --meeting-id 00000000-0000-4000-8000-000000000001
cargo run -p bazza-service -- desktop-bootstrap --library $fixtureRoot
cargo run -p bazza-service -- shutdown --library $fixtureRoot
cargo run -p bazza-service -- unregister-fixture-broker --library $fixtureRoot
```

`migrate-fixture` is explicit; `serve-fixture` and bootstrap require the current
schema and never upgrade implicitly. Store also exposes separate guarded create
and existing-open APIs. Missing paths do not create an empty library. A newer
schema is rejected before migrations/WAL setup; effective committed WAL is read
when the main-file header is older. Migrations 1–9 remain intact.
The service schema is now 21. Ordinary native startup still upgrades only through
legacy schema 9 until explicit service adoption and GUI/worker cutover.

`register-fixture-broker` is a local setup operation. It creates a demand-only
Task Scheduler task as the current interactive user at limited privilege, with
no password, logon trigger or boot trigger. The persisted bounded setup intent
contains executable/argument paths and a public API build fingerprint, no secret.
Start/unregister verify the task definition and lifetime settings. An old build's
matching spec remains unregisterable; moving the executable requires explicitly
unregistering its old task first. Unregister stops the fixture service and removes
its task/configuration. Re-registration is serialized with its own setup lock.

No production task is installed by building or running tests. The separate
[Windows broker experiment](experiments/windows-broker.ps1) registers a temporary
fixture broker, starts two clients concurrently, kills their launcher tree, reads
metadata through authenticated IPC, verifies shutdown/restart and unregisters in
cleanup. Its parameters require an explicit fresh fixture, service executable and
new evidence output. It must be invoked separately from automated unit tests.

## Rehearse a stopped-library upgrade

Close the current library owner before using the rehearsal. Pass an explicit
absolute path to a legacy library and a new absolute destination whose parent
already exists outside that library.

```powershell
cargo run -p bazza-service -- rehearse-adoption --library $legacyRoot --rehearsal $newOutput
```

The command holds the current build's exclusive library lock while taking a
consistent SQLite snapshot. The source database opens read-only. Success
publishes `snapshot/rollback/bazza.db` at the original schema and
`snapshot/candidate/bazza.db` at the service schema with its search index ready.
The copies contain the original meeting data and stored settings. The rehearsal
copies database contents only, preserves source identity and schema, and leaves
service startup and library enrollment to the cutover. An older executable
without the ownership interlock must also be stopped before snapshotting.

## Private RPC and ownership

The [transport](../bazza-service-transport/README.md) authenticates the OS user and
checks the kernel-reported peer PID. Windows pipes have a protected DACL for the
current SID and SYSTEM and reject remote clients; the service still checks the
peer user. A discovery PID/public endpoint never authenticates a connection.

Handshake checks API major/minor, supported schema range, the API-source build
fingerprint, persistent library ID, fresh service/library epoch and the required
fixture capabilities/OS-user context. Requests have
UUID correlation, an epoch, absolute deadlines and typed methods. Frames are
bounded at 128 KiB, calls at five seconds, and admitted connections at sixteen.
The client discards an in-flight stream on external cancellation, timeout, I/O,
correlation/epoch failure or a reply of the wrong kind. There is no SQL fallback.

Hello, status, meeting metadata, explicit shutdown and the experimental typed
worker and desktop operations below are implemented. These are private application/control
operations authenticated as the local OS user;
they are not public MCP tools or an external library grant. Secrets, voiceprints,
filesystem paths and generic invoke/SQL/shell do not appear in the metadata DTO.
The external read role has a separate endpoint and wire protocol.

Stage 2 adds a [transactional data core](../bazza-store/src/service_data/README.md)
and `bazza-core::Repository::data()` for fixture-only domain tests. It includes
document/note mutations, revisions/replay, lexical search, durable job/file
primitives and a calendar cache. These methods are not exposed by the private
RPC yet. Opening a synthetic repository atomically backfills its search projection
when not ready. Schema v10 is explicit, and the private client accepts up to the
shared `rpc::STORAGE_SCHEMA_MAX` without linking Store/SQLite.

The first stage-3 increment appends schema v11 and adds register/renew worker,
prepare/reattach/finalize capture RPC. The transport supplies the real peer PID;
worker credentials and fences are random secrets stored only as hashes. Core
verifies the [durable artifact handoff](../bazza-capture-handoff/README.md) before
finalize; terminal/caption bodies cannot be nominated through this RPC. A lost
ACK replays a minimal durable receipt, including after restart. Startup/periodic
recovery requires a released OS artifact lock and sealed terminal. A live raw
owner keeps the host alive across client EOF. A sealed failed capture persists
its partial evidence and receipt without scheduling post-processing. The
sealed reader holds the existing lock through commit, scans the journal by
frame, rejects a partial tail and leaves owner metadata untouched. The
production GUI and recorder still use their legacy paths.

The journal can retain 64 MiB/200,000 records, while the current DB finalize
transaction accepts 8 MiB/10,000 captions. Larger handoffs remain durable without
ACK until staged finalization is implemented. An unsealed begun capture remains
pending; pristine unused reservations can be compensated under v13 below.
A heartbeat alone never permits recovery. These are explicit
prototype limits, not production recording defaults.

The second stage-3 increment appends v12 preference revision state and adds seven
private desktop methods: bootstrap, preferences, update preferences, meeting page,
meeting header, change page and rename. The cloneable
[RepositoryClient](../bazza-service-client/README.md) returns library/epoch/scope
snapshots and binds continuations to that identity. Reconnect is explicit, drops
the old stream first and rejects another library; it never retries a mutation or
falls back to SQLite. Replies are bounded to 64 KiB and 100 items. Concurrent
renames/preferences require the revision from the original edit snapshot, with
transactional minimal receipts under the stable trusted desktop principal.

Preferences cover only locale, close-to-tray and hotkey; existing recording/STT
settings and unknown keys are preserved. Persisting these fields does not apply
native hotkey/tray effects until the GUI adapter is connected. Meeting headers
carry revisions and media-existence flags without file paths, create no media or
model directory, and support text-only domain cards. Legacy diagnostic text is
private and may contain machine paths; public errors still require sanitization.
This facade is tested against owned fixture processes. The production GUI still
uses its legacy Store and the new API does not yet cover all GUI reads/writers.

Private transcript/summary RPC now returns revision-pinned UTF-8 fragments within
the same desktop budget. Both current and archived transcript reads use immutable
source text plus overlays. Long legacy segments, empty manual cleanup and escaped
JSON fit through explicit continuations. Structured summaries are sliced as stored
JSON text; page reads never parse the complete document. The `desktop-bootstrap`
CLI prints only the narrow bootstrap envelope for explicit fixture diagnostics.

Append schema v13 separates reserved capture from begun capture. Begin requires
live kernel-authenticated artifact ownership; pristine unused prepare can abort
without file deletion, jobs or inference. An aborted capture ID retains its
minimal receipt and cannot be reused. Startup reconciliation can compensate old
unused reservations; periodic recovery never aborts a live current reservation.
The isolated GUI uses this admission for its explicit microphone capture.

Append schema v14 adds durable folder/tag revisions. Private closed organization
operations provide bounded mixed pages, atomic edits, hierarchy/template checks,
NOCASE named tags and meeting associations with revision/idempotency receipts.
Folder/tag deletion updates all affected revisions in the same transaction;
raw transcript, capture lifecycle and voice profiles remain separate.

Private native configuration read/update preserves the native settings document,
unknown saved fields and shared CAS with narrow preferences. It is separate from
public MCP projections. Provider/consent changes remain forbidden until the
permission/cancel ACK protocol; bounded hotkeys protect bootstrap. It does not
apply native hotkey/tray/integration effects or switch the ordinary GUI owner.

Append schema v15 combines private template state/uploads and registered
post-process ownership. Template RPC provides bounded list/field pages and
explicit begin/append/commit/abort, delete and seed-missing-builtins operations.
It preserves exact UTF-8 metadata/body bytes, including NUL, and SQL template IDs
up to 512 bytes without NUL. New prototype writes have an 8 MiB body, 512-byte
name, 8192-byte description, 32 KiB serialized append requests and 4096 chunks;
larger existing fields remain paged. Admission allows 16 open uploads and 64 MiB
of declared body storage per trusted principal. Uploads expire after one hour.
Startup and the 500 ms maintenance pass purge due upload payloads while retaining
identity/retry hashes; no write transaction runs when none are due. This cleanup
does not seed templates or alter the ordinary GUI library.

Post-process list/claim/renew/fail authenticates each current registered worker
against the transport PID, epoch, secret, expiry and capability. Claims bind the
finalized capture lifecycle, transcript/source and internal token hash. Reclaim
retires old bindings; historical receipts do not refresh ownership. Candidate
pages pin eligibility time and the change high-watermark. Failure atomically
changes only the owned processing job and meeting failure lifecycle, preserving
raw text and media. Native execution, provider permission, document/media
publication and successful job completion remain unopened boundaries.

Private post-process transcript reads authenticate the live registered claim and
read only its pinned original source in one Store snapshot. They share the bounded
desktop content reader, with no native execution, media path or output commit.
At the actual idle boundary, a current live registered processing claim retains
the host across client EOF until its job/worker deadline. Waiting jobs and worker
registrations alone remain idle-safe; failure, revoke and expiry release that
retention. This is a retention check, not worker authorization or a drain barrier.

The private `PostProcessMedia` RPC grants only seven fixed captured inputs:
`mix.wav`, `mic.wav`, `sys.wav`, `screen.raw.mp4`, `screen.raw.timeline.json`,
`screen.raw.clock.txt` and `mix.clock.json`. One mix is required; absent optional
files are omitted and an empty optional file reports zero bytes. No caller can
nominate a filename or role. The bounded grant contains private paths with
redacted Debug output; these paths never enter public MCP DTOs or the fixture GUI
asset protocol.

Registered PID/session/epoch, live claim, original source, expected provider and
the exact stored job expiry are checked in Store before filesystem access and
rechecked after it. Core retains its own shared media gate while verifying a
guard-bound challenge against the worker's kernel PID. Owner rendezvous and gate
files live outside recordings. Core reads the sealed terminal snapshot without
reopening the confirmed journal or rewriting capture-owner metadata, and compares
the original capture identity, provider/model and stored confirmed sequence.
Ordinary-file/reparse checks and the mix's RIFF/WAVE signature do not implement a
media decoder; `screen.raw.mp4` is raw input, not successful processing output.

The [SDK](../bazza-service-client/README.md) returns a guard-bearing result; its
owner or explicitly retained file clone must outlive consumption. Whole-meeting
purge remains pending until the final cooperating read copy closes. This opens
no native engine or success commit. Windows reader-child lifetime pins and
parent-crash behavior have synthetic acceptance; engine adoption, writer drain
and zero-Store production routing remain open.

The separate `CommitPostProcessAutoTitle` RPC commits only title metadata from a
registered post-process owner. Exact titles are nonblank/no-NUL and at most
512 UTF-8 bytes. The service authenticates the current worker before historical
replay. New results require the live claim/fence/source, original metadata CAS
and `title_auto=1`; stale or manually pinned titles cannot be overwritten.
Title, metadata revision, title-only search projection, change event and minimal
seven-day receipt commit together. Automatic titles keep `title_auto=1`, while
job/source/lifecycle revisions and leases remain unchanged. Receipts use the fixed
`service_idempotency` tool `post_process_auto_title`, separate from claim/renew/fail,
and contain no title/body/token. This result does not connect body/media staging,
native execution, success or job completion.

## Isolated GUI fixture entry

For a rehearsal with the ordinary Library, picker and recording bar, launch
`bazza.exe --service-desktop <absolute synthetic library path>` after starting
the service for that fixture. This launch uses a separate application identifier
and WebView profile. List, rename, delete, settings for the picker and recording
commands use the service. The preview exposes the routes supported by this owner.
Meeting detail, playback and the ordinary inference pipeline still require the
remaining desktop migration work.

The native development application accepts:

```powershell
bazza.exe --service-fixture C:\absolute\synthetic-library
```

This selects the existing marker-validated library before `data_dir`, Store or
AppState startup. It connects through RepositoryClient, optionally demand-starting
only an already configured verified fixture broker. Prestarting `serve-fixture`
works without installing a task. Missing/invalid roots or service failures never
fall through to the normal application. The fixture loads its native settings,
locale and built-in template catalog through the service before opening its
incognito WebView. The window uses identifier `app.bazza.mcp.fixture` and sixteen
typed invoke handlers. It skips ordinary routes, detection, calendar, sharing and
hotkey plugins. Its explicit core capability reads the fixture application
identifier. Media asset access is disabled.

Capture starts with the system's default microphone unless the user selects
the saved audio devices from the startup snapshot. The user can also include
the primary screen with the saved video quality. Full-system audio requires the saved
all-audio mode. Pause and Resume share the ordinary recorder's audio and
video timing transition, and Stop works while paused. The service owns the
meeting reservation and final database commit. The native writers keep the
artifact lock from Begin acknowledgement through drain and terminal
publication. A pending finalization can be retried without reopening devices.
The v2 seal preserves optional raw video evidence independently of audio.
Post-processing remains a separate worker stage.

Meeting deletion requires confirmation and the displayed lifecycle revision.
The service rejects deletion during capture or processing. An accepted
request hides the meeting and queues physical cleanup under the service owner.
The same request returns its original receipt if the reply was lost.

Preference persistence and rename are explicit actions using the displayed
revision/key. Content/list/change pages advance only on request; reconnect clears
old identities/cursors. This development entry exercises a real service GUI,
but the normal desktop application still uses Store. It does not close production
zero-direct-write, capture regression or public MCP/grant acceptance gates.

## Service lifecycle

One canonical library guard is consumed by Store and retained until SQLite has
closed. The host publishes bounded discovery atomically after acquiring ownership
and binding its endpoint. Restart retains library ID, changes epoch and replaces
stale discovery. The Windows service pipe is epoch-bound, so retained old client
handles cannot block a new first-instance bind; broker identity stays stable.
macOS retains its guarded stable Unix socket. EOF closes a client, while the independent service remains until
its idle timeout (default 30 seconds after the last client), provided no raw
artifact owner remains. Waiting native jobs do not keep it alive. Future shares
will add their own keepalive reason; share runtime is not implemented here.

The fixture marker is a development gate, not an authorization credential. Older
GUI binaries ignore the new guard. Current-build normal `bazza.db` opens now hold
that guard, and any persistent service identity blocks legacy direct access even
after service shutdown. Custom unmarked snapshot filenames remain available to
internal tooling; this is not a sandbox against arbitrary same-user SQLite access.
Production enabling still requires
the stopped-owner snapshot, version interlock and full GUI/worker cutover in the
[implementation plan](../../docs/dev/plans/2026-11-02-mcp-implementation.md).

## Opt in to external reads on a fixture

After explicitly migrating a disposable fixture, launch the service with its
separate external listener enabled. The fixture path must be absolute.

```powershell
cargo run -p bazza-service -- serve-fixture-external --library $fixtureRoot
```

The ordinary `serve-fixture` and fixture broker start the private application
listener alone and deny grant issuance, listing or revocation. The opted-in host binds
both listeners to the same owning service. Discovery supplies the public library
ID, epoch and process ID. The client derives a separate endpoint name for the
external role and verifies the server's PID through the local transport. The
external protocol accepts an authenticated Hello and the closed read catalog.
Grant issuance, revocation, worker operations and shutdown use private
application messages on the private listener.
The opted-in host disables the older private MCP read method. Its desktop
methods retain the local application trust boundary.

An explicit private administrative request issues a read-only credential for
one library and returns it once. Schema21 retains only its digest, connection ID,
client label and grant timestamps. The host verifies the credential on every
read and orders committed revocation against the publication of responses.
This authorization protects the external protocol and its data projections.
The local OS account remains the trust boundary for private administration.
The Windows `grant-fixture-read` and `revoke-fixture-read` commands use
Credential Manager. Production library admission and macOS Keychain
integration remain separate work.

## Verification boundaries

Schema17 exposes only bounded private chat/history reads and revision/generation
projections. Both erasure paths also fence pending library-wide completions,
including meeting/None, without removing history. Future write/completion APIs
must check those fences. The ordinary GUI and token/done events are not rerouted.

Schema16 adds an immutable capture anchor and a separate current worker
checkpoint. Private staged automatic cleanup commits its complete UTF-8 result,
both transcript projections, indices and the next lease in one transaction.
Manual text/raw/provider/voice state and the actual lease expiry stay protected;
historical receipts never refresh authority. Upload expiry is maintained without
starting an executor. Native Ready ordering and engine activation remain separate.

The media increment passed the full Windows owned-process suite and all-target
Clippy. The [stage-3 report](../../docs/dev/test-reports/2026-11-02-mcp-phase-3.md)
records current checkpoint/cleanup verification and the historical packaged GUI
scope; these checks do not establish production recording acceptance.

```powershell
cargo test -p bazza-service -p bazza-service-client -p bazza-service-transport -p bazza-core -p bazza-service-api -p bazza-store
```

Process tests use disposable fixtures and three private clients as stand-ins for
an application and two adapters. They do not claim real GUI/MCP cutover. macOS UDS
code is platform guarded but has not received native acceptance on this Windows
machine; its broker/install path is not implemented. Actual host MCP protocol
compatibility, hardware capture and production sharing remain later gates.
