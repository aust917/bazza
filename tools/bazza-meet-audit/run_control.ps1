param(
    [Parameter(Mandatory)] [string] $IngressExe,
    [Parameter(Mandatory)] [switch] $ConfirmDedicatedRoom,
    [ValidateSet(2, 8)] [int] $Speakers = 2,
    [ValidateSet('legacy', 'baseline', 'candidate')] [string] $Variant = 'legacy',
    [ValidateSet('direct', 'relay')] [string] $Transport = 'direct',
    [string] $RoomFixture,
    [switch] $Timing,
    [ValidateRange(5, 170)] [int] $AdmissionSeconds = 120,
    [ValidateRange(0, 65)] [int] $PreconnectSeconds = 60
)

$ErrorActionPreference = 'Stop'
if (!$ConfirmDedicatedRoom) { throw 'Confirm the dedicated synthetic test room first' }
$timingMode = if ($Timing) { 'timing' } else { 'standard' }
$auditRoot = 'E:\WSL\BazzaMeetAudioAuditQa'
$mainRoot = 'E:\Claude\Development\Hark'
$evidence = Join-Path $mainRoot '.git\local-tests\bazza-call-acceptance'
$service = Get-Content -LiteralPath (Join-Path $evidence 'meet-service.json') -Raw | ConvertFrom-Json
if ($RoomFixture) {
    $RoomFixture = (Resolve-Path -LiteralPath $RoomFixture).Path
    if ([IO.Path]::GetDirectoryName($RoomFixture) -ine $auditRoot -or [IO.Path]::GetExtension($RoomFixture) -ine '.url') {
        throw 'A replacement room fixture must be a .url file in the isolated audit root'
    }
} else {
    $RoomFixture = Join-Path $evidence 'meet-room.url'
}
$room = (Get-Content -LiteralPath $RoomFixture -Raw).Trim()
if ($room -notmatch '^https://meet\.google\.com/[a-z]{3}-[a-z]{4}-[a-z]{3}/?$') {
    throw 'Invalid dedicated test-room URL'
}
$apiUrl = 'http://127.0.0.1:23497'
$workerList = @(& wsl.exe -d BazzaMeetAudioAuditQa -u root --exec docker ps --filter label=com.docker.compose.service=worker --format '{{.Names}}')
if ($LASTEXITCODE -ne 0 -or $workerList.Count -ne 1 -or $workerList[0] -notmatch '^bazza-attendee-local-worker-[1-9][0-9]*$') {
    throw 'Start exactly one isolated recorder worker first'
}
$receiverWorker = $workerList[0]
$mount = @(& wsl.exe -d BazzaMeetAudioAuditQa -u root --exec docker inspect --format '{{range .Mounts}}{{if eq .Destination "/attendee/bots/bot_controller/bot_controller.py"}}{{.Source}}{{end}}{{end}}' $receiverWorker)
$mountRoot = '/mnt/e/WSL/BazzaMeetAudioAuditQa' + $(if ($Variant -ne 'legacy') { '/worker-' + $Variant })
if ($LASTEXITCODE -ne 0 -or $mount.Count -ne 1 -or $mount[0] -cne "$mountRoot/provider-overlay/bots/bot_controller/bot_controller.py") {
    throw 'Recorder worker does not use the requested audit variant'
}
if ($Timing) {
    $browserMount = @(& wsl.exe -d BazzaMeetAudioAuditQa -u root --exec docker inspect --format '{{range .Mounts}}{{if eq .Destination "/attendee/bots/google_meet_bot_adapter/google_meet_chromedriver_payload.js"}}{{.Source}}{{end}}{{end}}' $receiverWorker)
    if ($Variant -ne 'candidate' -or $LASTEXITCODE -ne 0 -or $browserMount.Count -ne 1 -or $browserMount[0] -cne '/mnt/e/WSL/BazzaMeetAudioAuditQa/rtp-audit.js') {
        throw 'Recorder worker does not use the timing overlay'
    }
}
$callback = [uri]$service.callbackUrl
$bindAddress = $service.bindAddress
if ($Transport -eq 'relay') {
    $bridge = (& wsl.exe -d BazzaMeetAudioAuditQa -u root --exec docker network inspect --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}' bazza-attendee-local_default)
    if ($LASTEXITCODE -ne 0 -or $bridge -notmatch '^172\.(1[6-9]|2[0-9]|3[01])\.\d{1,3}\.\d{1,3}$' -or $callback.Host -ne 'bazza-attendee.local') {
        throw 'Isolated callback bridge not available'
    }
    $resolved = @(& wsl.exe -d BazzaMeetAudioAuditQa -u root --exec docker exec $receiverWorker getent ahostsv4 $callback.Host)
    if ($LASTEXITCODE -ne 0 -or !$resolved -or ($resolved[0] -split '\s+')[0] -ne $bridge) {
        throw 'Recorder worker does not use the isolated callback bridge'
    }
    $bindAddress = '127.0.0.1:' + $callback.Port
}
$key = ((& wsl.exe -d BazzaMeetAudioAuditQa -u root --exec cat /opt/bazza-attendee/api-key) -join '').Trim()
if ($LASTEXITCODE -ne 0 -or !$key -or $key -match '\s') { throw 'Audit snapshot credentials unavailable' }
$scenario = Join-Path $auditRoot "control-$Speakers\scenario.json"
if (!(Test-Path -LiteralPath $scenario)) { throw 'Generate the short scenario before running it' }
$run = Join-Path $auditRoot ('run-' + [guid]::NewGuid().ToString('N').Substring(0, 10))
New-Item -ItemType Directory -Path $run | Out-Null
$recording = Join-Path $run 'receiver'
$config = Join-Path $run 'receiver-config.json'
$speakerLog = Join-Path $run 'speakers.jsonl'
$startSignal = Join-Path $run 'start.signal'
$admissionSignal = Join-Path $recording 'connected.signal'
$adoptSignal = Join-Path $recording 'adopt.signal'
$adoptedSignal = Join-Path $recording 'adopted.signal'
$stopSignal = Join-Path $recording 'stop.signal'
$relayReady = Join-Path $run 'relay-ready.signal'
$relayStop = Join-Path $run 'relay-stop.signal'
@{
    api_url = $apiUrl
    callback_url = $service.callbackUrl
    bind_address = $bindAddress
    certificate_path = $service.certificatePath
    private_key_path = $service.privateKeyPath
    meeting_url = $room
    max_seconds = 285
} | ConvertTo-Json | Set-Content -LiteralPath $config -Encoding utf8

function Start-Hidden([string] $File, [string[]] $Arguments, [hashtable] $Variables, [string] $Label) {
    $info = [Diagnostics.ProcessStartInfo]::new($File)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardError = $true
    $info.RedirectStandardOutput = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    foreach ($name in $Variables.Keys) { $info.Environment[$name] = [string]$Variables[$name] }
    $process = [Diagnostics.Process]::Start($info)
    [pscustomobject]@{
        Process = $process
        Label = $Label
        Out = $process.StandardOutput.ReadToEndAsync()
        Err = $process.StandardError.ReadToEndAsync()
    }
}

function Wait-Ready([scriptblock] $Condition, $Child, [int] $Seconds, [string] $Failure) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (& $Condition) { return }
        if ($Child.Process.HasExited) { break }
        Start-Sleep -Milliseconds 150
    }
    throw $Failure
}

function Save-Child($Child) {
    if ($Child -and $Child.Process.HasExited) {
        [IO.File]::WriteAllText((Join-Path $run ($Child.Label + '.stdout.log')), $Child.Out.GetAwaiter().GetResult())
        [IO.File]::WriteAllText((Join-Path $run ($Child.Label + '.stderr.log')), $Child.Err.GetAwaiter().GetResult())
    }
}

$receiver = $null
$sender = $null
$relay = $null
$logsSince = [DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ss.fffZ')
try {
    Write-Output "Run folder: $run"
    @{ variant = $Variant; speakers = $Speakers; transport = $Transport; timing = [bool]$Timing } | ConvertTo-Json -Compress | Set-Content -LiteralPath (Join-Path $run 'control.json') -Encoding utf8
    if ($Transport -eq 'relay') {
        $relay = Start-Hidden (Get-Command python).Source @(
            '-B', (Join-Path $PSScriptRoot 'relay_tcp.py'), '--distribution', 'BazzaMeetAudioAuditQa',
            '--bind', $bridge, '--port', [string]$callback.Port, '--target-port', [string]$callback.Port,
            '--ready-file', $relayReady, '--stop-file', $relayStop, '--seconds', '480'
        ) @{} 'relay'
        Wait-Ready { Test-Path -LiteralPath $relayReady } $relay 10 'Isolated callback relay did not bind'
    }
    $receiver = Start-Hidden (Resolve-Path -LiteralPath $IngressExe).Path @(
        '--live', $recording, $config, '--confirm-room'
    ) @{ BAZZA_AUDIT_ATTENDEE_API_KEY = $key } 'receiver'
    Wait-Ready { Test-Path -LiteralPath $admissionSignal } $receiver $AdmissionSeconds 'The recorder did not reach Connected within the admission window'
    Write-Output 'The isolated Meet recorder was admitted'
    $admittedAt = [DateTime]::UtcNow
    & wsl.exe -d BazzaMeetAudioAuditQa -u root --exec bash /mnt/e/Claude/Development/Bazza/tools/bazza-meet-audit/start_snapshot_worker.sh ($Speakers + 1) $Variant $Transport $timingMode
    if ($LASTEXITCODE) { throw 'Could not scale the isolated workers' }
    $sender = Start-Hidden (Get-Command python).Source @(
        '-B', (Join-Path $mainRoot 'tools\bazza-meet-probe\probe.py'), $scenario, '--confirm-test-meeting',
        '--log', $speakerLog, '--join-timeout', '105', '--start-file', $startSignal, '--start-timeout', '90'
    ) @{
        BAZZA_ATTENDEE_API_KEY = $key
        BAZZA_ATTENDEE_API_URL = $apiUrl
        BAZZA_TEST_MEET_URL = $room
    } 'sender'
    while (([DateTime]::UtcNow - $admittedAt).TotalSeconds -lt $PreconnectSeconds) {
        if ($receiver.Process.HasExited -or $sender.Process.HasExited) { throw 'Recorder or speaking bots exited before adoption' }
        Start-Sleep -Milliseconds 150
    }
    New-Item -ItemType File -Path $adoptSignal | Out-Null
    Wait-Ready { Test-Path -LiteralPath $adoptedSignal } $receiver 8 'Recorder did not acknowledge adoption'
    Wait-Ready {
        (Test-Path -LiteralPath $speakerLog) -and
        [bool](Select-String -LiteralPath $speakerLog -Pattern '"event": "ready_to_start"' -Quiet)
    } $sender 112 'Speaking bots did not finish admission'
    if ($receiver.Process.HasExited) { throw 'Recorder exited before test playback' }
    New-Item -ItemType File -Path $startSignal | Out-Null
    Write-Output 'Short control is playing in the dedicated room'
    if (!$sender.Process.WaitForExit(80000) -or $sender.Process.ExitCode) { throw 'Speaker control did not complete successfully' }
} finally {
    if ($receiver -and !$receiver.Process.HasExited -and (Test-Path -LiteralPath $recording)) {
        if (!(Test-Path -LiteralPath $stopSignal)) { New-Item -ItemType File -Path $stopSignal | Out-Null }
        [void]$receiver.Process.WaitForExit(35000)
    }
    Save-Child $receiver
    Save-Child $sender
    if ($relay -and !$relay.Process.HasExited) {
        New-Item -ItemType File -Path $relayStop | Out-Null
        [void]$relay.Process.WaitForExit(10000)
    }
    Save-Child $relay
    $logCommand = "docker logs --since $logsSince $receiverWorker 2>&1 | grep -E 'BAZZA_MEET_AUDIT_(BROWSER|WSS) ' || true"
    $counters = @(& wsl.exe -d BazzaMeetAudioAuditQa -u root --exec bash -lc $logCommand |
        ForEach-Object { if ($_ -match 'BAZZA_MEET_AUDIT_(BROWSER|WSS) \{.*\}$') { $Matches[0] } })
    [IO.File]::WriteAllLines((Join-Path $run 'provider-audit.log'), [string[]]$counters)
    if ($Timing) {
        $timingWorkers = @(& wsl.exe -d BazzaMeetAudioAuditQa -u root --exec docker ps --filter label=com.docker.compose.service=worker --format '{{.Names}}')
        $timingLog = foreach ($worker in $timingWorkers) {
            if ($worker -notmatch '^bazza-attendee-local-worker-[1-9][0-9]*$') { continue }
            $timingCommand = "docker logs --since $logsSince $worker 2>&1 | grep 'BAZZA_MEET_AUDIT_TIMING ' || true"
            & wsl.exe -d BazzaMeetAudioAuditQa -u root --exec bash -lc $timingCommand |
                ForEach-Object { if ($_ -match 'BAZZA_MEET_AUDIT_TIMING (\{.*\})$') { $Matches[1] } }
        }
        [IO.File]::WriteAllLines((Join-Path $run 'timing-audit.jsonl'), [string[]]@($timingLog))
    }
}
