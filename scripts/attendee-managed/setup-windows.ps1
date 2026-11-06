param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Setup', 'Start')]
    [string] $Mode,

    [Parameter(Mandatory = $true)]
    [string] $StateDirectory,

    [Parameter(Mandatory = $true)]
    [string] $ProvisionScript,

    [ValidatePattern('^BazzaAttendeeManaged(?:Test[A-Za-z0-9]{1,24})?$')]
    [string] $DistroName = 'BazzaAttendeeManaged'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$utf8 = New-Object System.Text.UTF8Encoding($false)
[Console]::OutputEncoding = $utf8
$OutputEncoding = $utf8
$env:WSL_UTF8 = '1'
$failureCode = 20
$script:latestStage = ''
$callbackProbe = $null
$mutex = $null
$ownsMutex = $false

function Send-Stage([string] $Stage) {
    if ($script:latestStage -eq $Stage) { return }
    $script:latestStage = $Stage
    [Console]::Error.WriteLine("BAZZA_PROGRESS $Stage")
}

function Invoke-Wsl([string[]] $Arguments) {
    # In Windows PowerShell 5.1 native stderr becomes ErrorRecord objects.
    # Handle the native exit code here and never forward its text to the UI.
    $ErrorActionPreference = 'Continue'
    $lines = @(& $script:wslPath @Arguments 2>&1)
    $code = $LASTEXITCODE
    $output = ($lines | ForEach-Object { [string] $_ }) -join "`n"
    [pscustomobject] @{ Code = $code; Output = $output.Replace("`0", '') }
}

function Read-Port([string] $Name, [Net.IPAddress] $Address) {
    $path = Join-Path $script:statePath $Name
    if ([IO.File]::Exists($path)) {
        $port = 0
        if (![int]::TryParse([IO.File]::ReadAllText($path), [ref] $port) -or $port -lt 1024 -or $port -gt 65535) {
            throw 'Invalid managed port.'
        }
        return $port
    }
    if ($Mode -ne 'Setup') { throw 'The managed port is missing.' }
    $probe = New-Object Net.Sockets.TcpListener($Address, 0)
    try {
        $probe.Start()
        $port = ([Net.IPEndPoint] $probe.LocalEndpoint).Port
        [IO.File]::WriteAllText($path, [string] $port, $utf8)
        return $port
    } finally {
        $probe.Stop()
    }
}

function Write-Pem([string] $Name, [string] $FirstLine) {
    $result = Invoke-Wsl -Arguments ($script:distroArgs + @('--exec', 'cat', "/opt/bazza-attendee/$Name"))
    $pem = $result.Output.Trim()
    if ($result.Code -ne 0 -or !$pem.StartsWith($FirstLine)) { throw 'Invalid managed callback TLS material.' }
    $path = Join-Path $script:statePath $Name
    if (![IO.File]::Exists($path) -or [IO.File]::ReadAllText($path).Trim() -cne $pem) {
        [IO.File]::WriteAllText($path, "$pem`n", $utf8)
    }
    return $path
}

function Test-ManagedApi([string] $Url, [string] $Key) {
    Add-Type -AssemblyName System.Net.Http
    $handler = New-Object System.Net.Http.HttpClientHandler
    $handler.UseProxy = $false
    $handler.AllowAutoRedirect = $false
    $client = New-Object System.Net.Http.HttpClient($handler)
    $client.Timeout = [TimeSpan]::FromSeconds(2)
    $client.DefaultRequestHeaders.Authorization = New-Object System.Net.Http.Headers.AuthenticationHeaderValue('Token', $Key)
    try {
        for ($i = 0; $i -lt 25; $i++) {
            try {
                $response = $client.GetAsync($Url).GetAwaiter().GetResult()
                try {
                    if ([int] $response.StatusCode -eq 200) { return }
                    if ([int] $response.StatusCode -in 401, 403) { break }
                } finally { $response.Dispose() }
            } catch [System.Management.Automation.MethodInvocationException] {
                # The WSL loopback forwarder may still be starting.
            }
            Start-Sleep -Milliseconds 700
        }
        throw 'The managed Attendee API did not accept its local key.'
    } finally { $client.Dispose() }
}

try {
    Send-Stage preparing
    if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() -ne 'X64') {
        $failureCode = 10
        throw 'Managed Attendee requires x64 Windows.'
    }
    $script:wslPath = Join-Path $env:SystemRoot 'System32\wsl.exe'
    if (![IO.File]::Exists($script:wslPath)) {
        $failureCode = 10
        throw 'WSL is unavailable.'
    }
    $missingWsl = 'HCS_E_HYPERV_NOT_INSTALLED|HCS_E_SERVICE_NOT_AVAILABLE|WSL_E_WSL_OPTIONAL_COMPONENT_REQUIRED|WSL_E_KERNEL_COMPONENT_MISSING|WSL_E_VM_MODE_INVALID_STATE|0x80370102|0x8007019e|0x800701bc'
    $status = Invoke-Wsl -Arguments @('--status')
    # Some WSL builds report failure from --status with no distributions yet.
    if ($status.Code -ne 0 -and ($status.Output -match $missingWsl -or (Invoke-Wsl -Arguments @('--version')).Code -ne 0)) {
        $failureCode = 10
        throw 'WSL is not ready.'
    }

    if ($StateDirectory -notmatch '^[A-Za-z]:[\\/]' -or ![IO.File]::Exists($ProvisionScript)) {
        throw 'Invalid managed installation paths.'
    }
    $script:statePath = [IO.Path]::GetFullPath($StateDirectory)
    $distroPath = Join-Path $script:statePath 'distribution'
    $script:distroArgs = @('--distribution', $DistroName, '--user', 'root')
    $mutex = New-Object System.Threading.Mutex($false, "Local\BazzaAttendeeProvision-$DistroName")
    try { $ownsMutex = $mutex.WaitOne(0) } catch [System.Threading.AbandonedMutexException] { $ownsMutex = $true }
    if (!$ownsMutex) { throw 'Managed setup is already in progress.' }

    $registrations = @(Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss\*' -ErrorAction SilentlyContinue |
        Where-Object { $_.DistributionName -ieq $DistroName })
    if ($registrations.Count -gt 0) {
        $registration = $registrations[0]
        $basePath = ([string] $registration.BasePath) -replace '^\\\\\?\\', ''
        if ($registration.Version -ne 2 -or $basePath.TrimEnd('\') -ine $distroPath.TrimEnd('\')) {
            throw 'A distribution with the managed name already exists at a different path.'
        }
    } else {
        if ($Mode -ne 'Setup') { throw 'The managed distribution has not been installed.' }
        [void] [IO.Directory]::CreateDirectory($script:statePath)
        [void] [IO.Directory]::CreateDirectory($distroPath)
        Send-Stage downloading
        $rootfsUri = 'https://cloud-images.ubuntu.com/wsl/releases/noble/20240423/ubuntu-noble-wsl-amd64-24.04lts.rootfs.tar.gz'
        $rootfsSha = '2a790896740b14d637dbdc583cce1ba081ac53b9e9cdb46dc09a2f73abbd9934'
        $archive = Join-Path $script:statePath 'ubuntu-noble-wsl-amd64.tar.gz'
        $partial = "$archive.partial"
        if (![IO.File]::Exists($archive) -or (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash -ine $rootfsSha) {
            [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
            try {
                Invoke-WebRequest -UseBasicParsing -Uri $rootfsUri -OutFile $partial -TimeoutSec 900
                if ((Get-FileHash -Algorithm SHA256 -LiteralPath $partial).Hash -ine $rootfsSha) {
                    throw 'The Ubuntu root filesystem failed its checksum.'
                }
                if ([IO.File]::Exists($archive)) { [IO.File]::Delete($archive) }
                [IO.File]::Move($partial, $archive)
            } finally {
                if ([IO.File]::Exists($partial)) { [IO.File]::Delete($partial) }
            }
        }
        Send-Stage installing
        $import = Invoke-Wsl -Arguments @('--import', $DistroName, $distroPath, $archive, '--version', '2')
        if ($import.Code -ne 0) {
            if ($import.Output -match $missingWsl) {
                $failureCode = 10
            }
            throw 'WSL could not import the managed distribution.'
        }
        [IO.File]::Delete($archive)
    }

    # The default WSL2 NAT gateway belongs to Windows. A normal LAN gateway
    # (for example in mirrored mode) must never become the callback target.
    $network = Invoke-Wsl -Arguments ($script:distroArgs + @('--exec', 'ip', '-4', 'route', 'show', 'default'))
    if ($network.Code -ne 0 -and $network.Output -match $missingWsl) {
        $failureCode = 10
        throw 'WSL2 could not start the managed distribution.'
    }
    if ($network.Code -ne 0 -or $network.Output -notmatch '(?m)^default via (\d{1,3}(?:\.\d{1,3}){3})\s') {
        $failureCode = 11
        throw 'WSL NAT routing is unavailable.'
    }
    $windowsHost = [Net.IPAddress]::Parse($Matches[1])
    $localIps = @([Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces() |
        ForEach-Object { $_.GetIPProperties().UnicastAddresses } |
        ForEach-Object { $_.Address })
    if ($localIps -notcontains $windowsHost) {
        $failureCode = 11
        throw 'The WSL gateway is not assigned to Windows.'
    }

    $apiPort = Read-Port 'api-port' ([Net.IPAddress]::Loopback)
    $callbackPort = Read-Port 'callback-port' $windowsHost
    if ($Mode -eq 'Setup') {
        $callbackProbe = New-Object Net.Sockets.TcpListener($windowsHost, $callbackPort)
        $callbackProbe.Start()
        Send-Stage installing
    } else {
        Send-Stage starting
    }
    $linuxPathResult = Invoke-Wsl -Arguments ($script:distroArgs + @('--exec', 'wslpath', '-a', '-u', $ProvisionScript))
    $linuxPath = $linuxPathResult.Output.Trim()
    if ($linuxPathResult.Code -ne 0 -or $linuxPath -notmatch '^/[^\r\n]+$') {
        throw 'The managed provisioner is not visible inside WSL.'
    }
    $result = Invoke-Wsl -Arguments ($script:distroArgs + @('--exec', 'bash', '--', $linuxPath, $Mode, $windowsHost.ToString(), "$apiPort", "$callbackPort"))
    if ($result.Code -ne 0) {
        # Provision emits no credentials. Keep diagnostics in the local profile.
        [IO.File]::WriteAllText((Join-Path $script:statePath 'provision-error.log'), $result.Output, $utf8)
        if ($result.Code -eq 11) { $failureCode = 11 }
        throw 'The managed Attendee services failed to start.'
    }
    if ($callbackProbe) { $callbackProbe.Stop(); $callbackProbe = $null }

    if ($Mode -eq 'Setup') { Send-Stage starting }
    Send-Stage checking
    $apiUrl = "http://127.0.0.1:$apiPort"
    $keyResult = Invoke-Wsl -Arguments ($script:distroArgs + @('--exec', 'cat', '/opt/bazza-attendee/api-key'))
    $apiKey = $keyResult.Output.Trim()
    if ($keyResult.Code -ne 0 -or $apiKey -cnotmatch '^[a-zA-Z0-9]{32}$') { throw 'The managed API key is missing.' }
    Test-ManagedApi "$apiUrl/api/v1/bots" $apiKey

    $certificatePath = Write-Pem 'callback.pem' '-----BEGIN CERTIFICATE-----'
    $privateKeyPath = Write-Pem 'callback.key' '-----BEGIN PRIVATE KEY-----'
    $json = [ordered] @{
        apiUrl = $apiUrl
        apiKey = $apiKey
        callbackUrl = "wss://bazza-attendee.local:$callbackPort"
        bindAddress = "${windowsHost}:$callbackPort"
        certificatePath = $certificatePath
        privateKeyPath = $privateKeyPath
    } | ConvertTo-Json -Compress
    [Console]::Out.WriteLine($json)
    exit 0
} catch {
    if ($script:statePath -and [IO.Directory]::Exists($script:statePath)) {
        try {
            [IO.File]::WriteAllText((Join-Path $script:statePath 'last-failure.txt'), "$script:latestStage ($failureCode)`n", $utf8)
        } catch { }
    }
    exit $failureCode
} finally {
    if ($callbackProbe) { $callbackProbe.Stop() }
    if ($mutex) {
        if ($ownsMutex) { $mutex.ReleaseMutex() }
        $mutex.Dispose()
    }
}
