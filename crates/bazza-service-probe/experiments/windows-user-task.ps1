param(
    [Parameter(Mandatory = $true)][string]$ProbeExecutable,
    [Parameter(Mandatory = $true)][string]$FixturePath,
    [Parameter(Mandatory = $true)][string]$EvidencePath
)

# Explicit phase-0 experiment. Registers only a uniquely named temporary task
# for the current interactive user and removes it in finally. No production paths.
$ErrorActionPreference = 'Stop'
$probePath = (Resolve-Path -LiteralPath $ProbeExecutable).Path
$fixtureRoot = (Resolve-Path -LiteralPath $FixturePath).Path
if ([System.IO.Path]::GetFileName($probePath) -ne 'bazza-service-probe.exe') {
    throw 'Expected the synthetic probe executable'
}
if ([System.IO.File]::ReadAllText((Join-Path $fixtureRoot '.bazza-synthetic-fixture')) -cne "bazza-mcp-v8-fixture-v1`n") {
    throw 'Synthetic v8 fixture required'
}
if (Test-Path -LiteralPath $EvidencePath) { throw 'Evidence output already exists' }
if (Test-Path -LiteralPath (Join-Path $fixtureRoot '.bazza-service-probe.json')) {
    throw 'Broker experiment requires a fresh fixture with no prior probe status'
}

$taskName = 'BazzaMcpProbe-' + [Guid]::NewGuid().ToString('N')
$expectedInstanceId = [Guid]::NewGuid().ToString()
$userSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$registrationAttempted = $false
$launcher = $null
$primaryFailure = $null
$cleanupErrors = [System.Collections.Generic.List[string]]::new()
$experimentPhase = 'registration'
$evidence = [ordered]@{
    schema_version = 1
    mechanism = 'Task Scheduler InteractiveToken Limited on demand'
    task_name = $taskName
    probe_sha256 = (Get-FileHash -LiteralPath $probePath -Algorithm SHA256).Hash
    result = 'not_run'
    phase = $experimentPhase
}

function Read-ProbeStatus {
    $statusPath = Join-Path $fixtureRoot '.bazza-service-probe.json'
    if (Test-Path -LiteralPath $statusPath) {
        Get-Content -LiteralPath $statusPath -Raw | ConvertFrom-Json
    }
}

try {
    $action = New-ScheduledTaskAction -Execute $probePath -Argument ('run --fixture "' + $fixtureRoot + '" --lifetime-ms 15000 --instance-id ' + $expectedInstanceId)
    $principal = New-ScheduledTaskPrincipal -UserId $userSid -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Seconds 30) -MultipleInstances IgnoreNew -Hidden
    $task = New-ScheduledTask -Action $action -Principal $principal -Settings $settings
    $registrationAttempted = $true
    Register-ScheduledTask -TaskName $taskName -InputObject $task | Out-Null
    $experimentPhase = 'start_from_launcher'

    # Only the generated UUID task name enters this script, never path/user text.
    # -EncodedCommand avoids nesting shell quoting; credentials are not passed.
    $launcherCode = "Start-ScheduledTask -TaskName '$taskName'; Start-Sleep -Seconds 30"
    $encodedLauncher = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($launcherCode))
    $launcher = Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoProfile', '-NonInteractive', '-WindowStyle', 'Hidden', '-EncodedCommand', $encodedLauncher) -WindowStyle Hidden -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds(8)
    $first = $null
    while ([DateTime]::UtcNow -lt $deadline) {
        $candidate = Read-ProbeStatus
        if ($candidate -and $candidate.instance_id -eq $expectedInstanceId -and -not $candidate.finished) { $first = $candidate; break }
        if ($launcher.HasExited) { throw 'Task launcher exited before service readiness' }
        Start-Sleep -Milliseconds 50
    }
    if (-not $first) { throw 'Task did not publish a live probe status before deadline' }
    $experimentPhase = 'kill_launcher_tree'
    & taskkill.exe /PID $launcher.Id /T /F | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Owned launcher tree could not be terminated' }
    $afterKill = Read-ProbeStatus
    Start-Sleep -Milliseconds 250
    $after = Read-ProbeStatus
    if (-not $after -or $after.finished -or $after.instance_id -ne $first.instance_id -or $after.heartbeat -le $afterKill.heartbeat) {
        throw 'Scheduled probe did not survive launcher tree termination'
    }
    $evidence.result = 'survived_launcher_tree_kill'
    $evidence.instance_id = $after.instance_id
    $evidence.service_pid = $after.process_id
    $evidence.heartbeat_after_kill = $afterKill.heartbeat
    $evidence.heartbeat_later = $after.heartbeat
} catch {
    $primaryFailure = $_
    $evidence.result = 'unavailable_or_failed'
    $evidence.error = $_.Exception.Message
} finally {
    $evidence.phase = $experimentPhase
    try {
        if ($launcher -and -not $launcher.HasExited) {
            # This handle belongs only to our temporary launcher, never to a status PID.
            $launcher.Kill()
            if (-not $launcher.WaitForExit(3000)) { throw 'Owned launcher cleanup timed out' }
        }
    } catch {
        $cleanupErrors.Add('launcher: ' + $_.Exception.Message)
    }
    if ($registrationAttempted) {
        try { Stop-ScheduledTask -TaskName $taskName -ErrorAction Stop }
        catch { $cleanupErrors.Add('task stop: ' + $_.Exception.Message) }
        try { Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction Stop }
        catch { $cleanupErrors.Add('task removal: ' + $_.Exception.Message) }
    }
    try {
        $remaining = @(Get-ScheduledTask -TaskPath '\' -ErrorAction Stop | Where-Object TaskName -CEQ $taskName)
        $evidence.task_removed = $remaining.Count -eq 0
        if (-not $evidence.task_removed) { throw 'Temporary task remains registered' }
    } catch {
        $evidence.task_removed = $false
        $cleanupErrors.Add('task verification: ' + $_.Exception.Message)
    }
    $evidence.cleanup_errors = $cleanupErrors.ToArray()
    # CreateNew also protects against an output appearing after the initial check.
    $outputFile = $null
    $writer = $null
    try {
        $outputFile = [IO.File]::Open($EvidencePath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        $writer = [IO.StreamWriter]::new($outputFile, [Text.UTF8Encoding]::new($false))
        $writer.WriteLine(($evidence | ConvertTo-Json -Depth 4))
    } catch {
        $cleanupErrors.Add('evidence write: ' + $_.Exception.Message)
    } finally {
        if ($writer) { $writer.Dispose() }
        elseif ($outputFile) { $outputFile.Dispose() }
    }
}
if ($primaryFailure) { throw $primaryFailure }
if ($cleanupErrors.Count -gt 0) { throw ('Broker cleanup failed: ' + ($cleanupErrors -join '; ')) }
