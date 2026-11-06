# Bazza MCP adapter

`bazza-mcp` is a read-only stdio server for an explicitly selected Bazza
library. It uses RMCP 3.8.0 and supports MCP revisions
[2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning)
and [2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle).
The adapter connects to an already running Bazza service. The service owns
the database and checks the request and response bounds. A selected library ID
stays pinned for the adapter lifetime. Each call opens a fresh authenticated
pipe so a pause between calls can outlast the connection's idle timeout.

The tool catalog exposes status, meeting list and metadata, lexical search
across titles, transcripts, summaries and notes, paged raw or effective
transcript reads, paged summaries, folders and tags, and note list and body reads.
Use a transcript search hit's `meeting_id` and `source_id` to
read that source. With `representation: "both"`, a split segment yields raw
text first and effective text second. Join each representation's fragments
for the same segment ID. The byte and segment limits can be adjusted across
transcript pages. Summary reads select Markdown or the exact stored JSON text.
Library structure can be paged as a combined catalog or filtered to folders or
tags. Use a note search hit's `note_id` to open its Markdown. The note list
can be scoped to one meeting, standalone notes, or the library. A stale cursor
requires starting that read again.

## Build the Windows binaries

Run the following from the repository root in PowerShell 7 with the Rust
development environment configured. The helper waits for administrative
commands and captures their stdout because
`bazza-service.exe` uses the Windows GUI subsystem.

```powershell
$target = Join-Path (Get-Location) '.git/local-tests/20261102-mcp/target'
cargo build --locked --target-dir $target -p bazza-service -p bazza-mcp
$service = Join-Path $target 'debug/bazza-service.exe'
$mcp = Join-Path $target 'debug/bazza-mcp.exe'

function Invoke-BazzaService([string[]]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new($service)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($info)
    try {
        $receipt = $process.StandardOutput.ReadToEnd()
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) { throw "Bazza service command failed: $($process.ExitCode)" }
        $receipt
    } finally {
        $process.Dispose()
    }
}
```

## Run with a managed library on Windows

The Windows installer prepares the managed library and demand-start service
with MCP access off for new installations. An interactive fresh install offers
access to Codex and to both Claude clients. Silent and passive installs skip
these choices. In-place updates preserve existing grants and upgrade the
managed schema under the library owner lock before registering the new service.

The installer uses `bazza-mcp install-mcp` to provision the library and service.
The `--enable-codex` and `--enable-claude` flags opt in to the corresponding
clients. Access can also be enabled per client in Bazza Settings.

### Manually selected library

Choose a new absolute library directory under an existing parent. Initialize it
and start its service with the separate MCP read endpoint enabled.

```powershell
$library = 'D:\Bazza\Meetings'
Invoke-BazzaService @('init-library', '--library', $library)
& $service serve-library-external --library $library --idle-timeout-ms 60000
```

Launch the Bazza desktop executable with `--service-library` followed by the
same path to record into this library. Run the following command while the
service is running to grant read access to an MCP client.

```powershell
$grant = Invoke-BazzaService @('grant-library-read', '--library', $library, '--client-label', 'Local MCP host') | ConvertFrom-Json
```

Point the MCP host at the absolute executable path stored in `$mcp` and pass
the following arguments. Replace `<connection ID>` with `$grant.connection_id`.

```text
serve-library
--library
D:\Bazza\Meetings
--connection-id
<connection ID>
```

The grant command stores the secret in Windows Credential Manager. The MCP
host configuration contains the library path and public connection ID.
Each connection is scoped to the selected library. Use the admin commands to
list grant metadata, revoke access, and shut down the service.

```powershell
Invoke-BazzaService @('list-library-read-grants', '--library', $library, '--limit', '20')
Invoke-BazzaService @('revoke-library-read', '--library', $library, '--connection-id', $grant.connection_id)
Invoke-BazzaService @('shutdown-library', '--library', $library)
```

Pass a list response's `next_after` as `--after` to continue the list.
If the service stopped while idle, rerun `serve-library-external` for the same
library before connecting again.

## Run on a disposable Windows fixture

Create and explicitly migrate a new synthetic library.

```powershell
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('bazza-mcp-' + [Guid]::NewGuid())
cargo run --locked --target-dir $target -p bazza-service-test-support --example create_v8 -- $fixture
Invoke-BazzaService @('migrate-fixture', '--library', $fixture)
& $service serve-fixture --library $fixture --idle-timeout-ms 60000
```

While the service is running, point a stdio client at the absolute path to
`debug/bazza-mcp.exe` inside that target with the following three arguments.
The fixture path is the same one passed to the service.

```text
serve-fixture
--library
<absolute synthetic fixture path>
```

Closing stdio ends the adapter. The fixture service has its own idle timeout.
It also supports explicit `shutdown --library <path>`.

## Try a granted connection on Windows

Stop the fixture service and start the same migrated fixture with
`serve-fixture-external` instead. This enables the separate read endpoint
and requires a grant for MCP reads. Issue a grant while the service is running:

```powershell
$grant = Invoke-BazzaService @('grant-fixture-read', '--library', $fixture, '--client-label', 'Local MCP host') | ConvertFrom-Json
```

If the pairing receipt is lost, use the admin list to find the connection by
its label and creation time. The page includes revoked connections and never
contains credentials. Pass the returned `next_after` with `--after` to continue.

```powershell
Invoke-BazzaService @('list-fixture-read-grants', '--library', $fixture, '--limit', '20')
```

Configure the MCP host to launch the adapter with
`serve-fixture-granted --library <fixture path> --connection-id <connection ID>`.
The command stores the secret in Windows Credential Manager. The host
configuration needs only the public ID. An advanced host may supply the secret
through `BAZZA_MCP_READ_SECRET` and is subject to the same service checks.
Revoke the grant when finished. This denies subsequent reads, including on
an open connection:

```powershell
Invoke-BazzaService @('revoke-fixture-read', '--library', $fixture, '--connection-id', $grant.connection_id)
```

The fixture marker selects development data. See the
[implementation plan](../../docs/dev/plans/2026-11-02-mcp-implementation.md)
for the desktop cutover status and verification history.

## Checks

`cargo test --locked -p bazza-mcp` launches the adapter as a real child and
exercises it over stdio against the fixture host's local IPC. It covers both
protocol revisions, catalog and output schemas, all four lexical fields,
source-pinned transcript fragments, summary and note fragments, folder and tag filters,
write rejection, stale cursors after service restart, grant revocation and EOF. Test data and
service lifetimes are scoped to disposable roots.
