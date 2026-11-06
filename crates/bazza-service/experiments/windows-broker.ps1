param(
    [Parameter(Mandatory = $true)][string]$ServiceExecutable,
    [Parameter(Mandatory = $true)][string]$FixturePath,
    [Parameter(Mandatory = $true)][string]$EvidencePath
)

# Explicit Windows integration acceptance. Only a new marked synthetic fixture
# and a newly registered current-user task are used. No default library/pointer.
$ErrorActionPreference = 'Stop'
$servicePath = (Resolve-Path -LiteralPath $ServiceExecutable).Path
$fixtureRoot = (Resolve-Path -LiteralPath $FixturePath).Path
if ([IO.Path]::GetFileName($servicePath) -ne 'bazza-service.exe') { throw 'Expected bazza-service.exe' }
if ([IO.File]::ReadAllText((Join-Path $fixtureRoot '.bazza-synthetic-fixture')) -cne "bazza-mcp-v8-fixture-v1`n") { throw 'Synthetic fixture required' }
foreach ($name in @('.bazza-service-endpoint.json', '.bazza-service-broker.json')) {
    if (Test-Path -LiteralPath (Join-Path $fixtureRoot $name)) { throw 'Fresh fixture required' }
}
if (Test-Path -LiteralPath $EvidencePath) { throw 'New evidence output required' }
$evidence = [ordered]@{schema_version=1;result='not_run';service_sha256=(Get-FileHash -LiteralPath $servicePath -Algorithm SHA256).Hash}
$cleanupErrors = [Collections.Generic.List[string]]::new()
$failure = $null
$registered = $false
$launcher = $null
$taskName = $null

function Invoke-Service([string]$action, [string[]]$extra = @()) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $servicePath
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($arg in (@($action, '--library', $fixtureRoot) + $extra)) { $start.ArgumentList.Add($arg) }
    $process = [Diagnostics.Process]::Start($start)
    try {
        if (-not $process.WaitForExit(10000)) { $process.Kill(); $process.WaitForExit(); throw 'Owned service CLI timeout' }
        $output = $process.StandardOutput.ReadToEnd()
        $errorOutput = $process.StandardError.ReadToEnd()
        if ($process.ExitCode -ne 0) { throw ('Service CLI failed: ' + $action + ' ' + $errorOutput) }
        if ($output.Trim()) { $output | ConvertFrom-Json }
    } finally { $process.Dispose() }
}

try {
    $migrated = Invoke-Service 'migrate-fixture'
    $evidence.library_id = $migrated.library_id
    # Persisted setup intent permits cleanup even if OS registration is interrupted.
    $registered = $true
    Invoke-Service 'register-fixture-broker' | Out-Null
    $spec = Get-Content -LiteralPath (Join-Path $fixtureRoot '.bazza-service-broker.json') -Raw | ConvertFrom-Json
    $taskName = $spec.task_name

    $launcherCode = @'
$ErrorActionPreference = 'Stop'
$config = $env:BAZZA_FIXTURE_LAUNCH | ConvertFrom-Json
$arguments = 'start-fixture --library "' + $config.root + '"'
function Start-Client {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $config.exe
    $info.Arguments = $arguments
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $info.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    [Diagnostics.Process]::Start($info)
}
$one = Start-Client
$two = Start-Client
if (-not $one.WaitForExit(10000) -or -not $two.WaitForExit(10000)) { throw 'Owned clients timed out' }
[IO.File]::WriteAllText((Join-Path $config.root 'client-one.json'), $one.StandardOutput.ReadToEnd())
[IO.File]::WriteAllText((Join-Path $config.root 'client-two.json'), $two.StandardOutput.ReadToEnd())
[IO.File]::WriteAllText((Join-Path $config.root 'client-one.err'), $one.StandardError.ReadToEnd())
[IO.File]::WriteAllText((Join-Path $config.root 'client-two.err'), $two.StandardError.ReadToEnd())
if ($one.ExitCode -ne 0 -or $two.ExitCode -ne 0) { throw 'Owned client bootstrap failed' }
[IO.File]::WriteAllText((Join-Path $config.root 'launcher-ready'), 'ready')
Start-Sleep -Seconds 30
'@
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = (Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell/v1.0/powershell.exe')
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    foreach ($arg in @('-NoProfile', '-NonInteractive', '-WindowStyle', 'Hidden', '-EncodedCommand', [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($launcherCode)))) { $start.ArgumentList.Add($arg) }
    $start.Environment['BAZZA_FIXTURE_LAUNCH'] = @{exe=$servicePath;root=$fixtureRoot} | ConvertTo-Json -Compress
    $launcher = [Diagnostics.Process]::Start($start)
    $deadline = [DateTime]::UtcNow.AddSeconds(12)
    while (-not (Test-Path -LiteralPath (Join-Path $fixtureRoot 'launcher-ready'))) {
        if ($launcher.HasExited -or [DateTime]::UtcNow -ge $deadline) { throw 'Concurrent broker bootstrap did not become ready' }
        Start-Sleep -Milliseconds 50
    }
    $one = Get-Content -LiteralPath (Join-Path $fixtureRoot 'client-one.json') -Raw | ConvertFrom-Json
    $two = Get-Content -LiteralPath (Join-Path $fixtureRoot 'client-two.json') -Raw | ConvertFrom-Json
    if ($one.process_id -ne $two.process_id -or $one.library_epoch -ne $two.library_epoch -or $one.library_id -ne $migrated.library_id) { throw 'Concurrent clients did not connect to one owner' }
    if (-not $one.development_fixture_only -or $one.authentication -ne 'local_os_user') { throw 'Unexpected service boundary' }
    $evidence.shared_service_pid = $one.process_id
    $evidence.first_epoch = $one.library_epoch
    & (Join-Path ([Environment]::SystemDirectory) 'taskkill.exe') /PID $launcher.Id /T /F | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Owned launcher tree could not be terminated' }
    Start-Sleep -Milliseconds 250
    $survivor = Invoke-Service 'status'
    $meeting = Invoke-Service 'get-meeting' @('--meeting-id', '00000000-0000-4000-8000-000000000001')
    if ($survivor.process_id -ne $one.process_id -or $survivor.library_epoch -ne $one.library_epoch -or $meeting.id -ne '00000000-0000-4000-8000-000000000001') { throw 'Authenticated service did not survive launcher tree kill' }
    Invoke-Service 'shutdown' | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while (Test-Path -LiteralPath (Join-Path $fixtureRoot '.bazza-service-endpoint.json')) {
        if ([DateTime]::UtcNow -ge $deadline) { throw 'Graceful shutdown deadline' }
        Start-Sleep -Milliseconds 30
    }
    $restarted = Invoke-Service 'start-fixture'
    if ($restarted.library_id -ne $one.library_id -or $restarted.library_epoch -eq $one.library_epoch) { throw 'Restart identity/epoch mismatch' }
    $evidence.restarted_epoch = $restarted.library_epoch
    $evidence.restarted_service_pid = $restarted.process_id
    $evidence.result = 'one_owner_survived_launcher_tree_and_restarted'
    Invoke-Service 'shutdown' | Out-Null
} catch { $failure = $_; $evidence.result = 'failed'; $evidence.error = $_.Exception.Message }
finally {
    try {
        if ($launcher -and -not $launcher.HasExited) { $launcher.Kill(); if (-not $launcher.WaitForExit(3000)) { throw 'Owned launcher cleanup timeout' } }
    } catch { $cleanupErrors.Add('launcher: ' + $_.Exception.Message) }
    if ($registered -and (Test-Path -LiteralPath (Join-Path $fixtureRoot '.bazza-service-broker.json'))) {
        try {
            # Simulate an API rebuild: safe unregister must accept an old build ID.
            $path = Join-Path $fixtureRoot '.bazza-service-broker.json'
            $oldSpec = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
            $taskName = $oldSpec.task_name
            $oldSpec.build_id = 'previous-development-build'
            [IO.File]::WriteAllText($path, ($oldSpec | ConvertTo-Json -Compress))
            Invoke-Service 'unregister-fixture-broker' | Out-Null
        } catch { $cleanupErrors.Add('broker: ' + $_.Exception.Message) }
    }
    try {
        $remaining = @(Get-ScheduledTask -TaskPath '\' -ErrorAction Stop | Where-Object TaskName -CEQ $taskName)
        $evidence.task_removed = $remaining.Count -eq 0
        if (-not $evidence.task_removed) { throw 'Temporary broker remains registered' }
    } catch { $cleanupErrors.Add('task verification: ' + $_.Exception.Message) }
    $evidence.cleanup_errors = $cleanupErrors.ToArray()
    $file = $null; $writer = $null
    try {
        $file = [IO.File]::Open($EvidencePath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        $writer = [IO.StreamWriter]::new($file, [Text.UTF8Encoding]::new($false))
        $writer.WriteLine(($evidence | ConvertTo-Json -Depth 4))
    } catch { $cleanupErrors.Add('evidence: ' + $_.Exception.Message) }
    finally { if ($writer) { $writer.Dispose() } elseif ($file) { $file.Dispose() } }
}
if ($failure) { throw $failure }
if ($cleanupErrors.Count) { throw ('Cleanup failed: ' + ($cleanupErrors -join '; ')) }
