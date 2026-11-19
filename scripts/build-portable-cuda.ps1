# Windows PowerShell 5.1. Builds the standalone CUDA runtime that update-portable.ps1 publishes.
# Machine-local inputs live in -BuildEnv (default <git common dir>\local-builds\cuda1322):
#   toolkit\  CUDA 13.2 unpacked from the archives in cuda-toolkit-13.2.json (not a system CUDA)
#   ninja\, target\ (reused Cargo cache), models\ggml-silero-v5.1.2.bin
#   <NativeInputs>\ verified CUDA DLLs, ffmpeg and NeMo runtime with their cuda-runtime.json
# Rust/cargo/pnpm shim come from -Tools (default <git common dir>\local-tools).
param(
    [Parameter(Mandatory)][ValidatePattern('^[a-z0-9-]+$')][string]$RunName,
    [ValidateSet('All', 'Snapshot', 'Build', 'Stage')][string]$Action = 'All',
    [string]$BuildEnv,
    [string]$Tools,
    [string]$NativeInputs = 'candidate-screen-output-5457d863c5c9-20261104170106',
    [string]$VsPath = 'C:\Program Files\Microsoft Visual Studio\18\Community',
    [string]$MsvcVersion = '14.50'
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$gitDir = (git -C $repo rev-parse --path-format=absolute --git-common-dir).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Not a git checkout.' }
$gitDir = [IO.Path]::GetFullPath($gitDir)
if (!$BuildEnv) { $BuildEnv = Join-Path $gitDir 'local-builds\cuda1322' }
if (!$Tools) { $Tools = Join-Path $gitDir 'local-tools' }
$run = Join-Path $gitDir "local-builds\$RunName"
$source = Join-Path $run 'source'
$runtime = Join-Path $run 'runtime'
$prior = Join-Path $BuildEnv $NativeInputs
$toolkit = Join-Path $BuildEnv 'toolkit'
$vadModel = Join-Path $BuildEnv 'models\ggml-silero-v5.1.2.bin'
$hostTriple = 'x86_64-pc-windows-msvc'
$bins = Join-Path $source 'src-tauri\binaries'
$resources = Join-Path $source 'src-tauri\resources'
$sidecars = @('bazza-diarize', 'bazza-mcp', 'bazza-service')
# The prebuilt CUDA native family was verified with this lockfile (ab19d34).
$lockCommit = 'ab19d34'

function Test-Preflight {
    $problems = @()
    foreach ($path in $toolkit, (Join-Path $BuildEnv 'ninja'), $prior, $vadModel, (Join-Path $Tools 'cargo\bin\cargo.exe'),
        (Join-Path $Tools 'rustup'), (Join-Path $Tools 'pnpm-shim\pnpm.cmd'), (Join-Path $VsPath 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')) {
        if (!(Test-Path -LiteralPath $path)) { $problems += "missing $path" }
    }
    if (Test-Path -LiteralPath $toolkit) {
        $nvcc = & (Join-Path $toolkit 'bin\nvcc.exe') --version | Select-String 'release 13\.2'
        if (!$nvcc) { $problems += 'toolkit nvcc is not CUDA 13.2' }
    }
    # CUDA 13.2 rejects MSVC _MSC_VER >= 1960; pin a known toolset instead of the newest one.
    if (!(Get-ChildItem -LiteralPath (Join-Path $VsPath 'VC\Tools\MSVC') -Directory -Filter "$MsvcVersion.*" -ErrorAction SilentlyContinue)) {
        $problems += "MSVC toolset $MsvcVersion not installed"
    }
    if ((Test-Path -LiteralPath $vadModel) -and
        (Get-FileHash -LiteralPath $vadModel).Hash -ine '29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf') {
        $problems += 'VAD model hash mismatch'
    }
    git -C $repo diff --quiet $lockCommit -- Cargo.lock
    if ($LASTEXITCODE -ne 0) { $problems += "Cargo.lock differs from $lockCommit; rebuild the native CUDA inputs first (docs/dev/testing.md)" }
    if ($problems) { throw "Build preflight failed:`n - $($problems -join "`n - ")" }
}

if ($Action -in 'All', 'Snapshot') {
    Test-Preflight
    if (Test-Path -LiteralPath $run) { throw "Build run already exists: $run" }
    git -C $repo diff --quiet HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Commit or stash tracked changes first; the snapshot must match a commit.' }
    $commit = (git -C $repo rev-parse HEAD).Trim()
    $null = New-Item -ItemType Directory -Path $source
    Set-Content -LiteralPath (Join-Path $run 'commit.txt') -Value $commit -Encoding ascii
    $sourceFiles = @(git -C $repo -c core.quotepath=false ls-files --cached)
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $hashes = foreach ($entry in $sourceFiles | Sort-Object -Unique) {
        $from = [IO.Path]::GetFullPath((Join-Path $repo $entry))
        $to = [IO.Path]::GetFullPath((Join-Path $source $entry))
        if (!$from.StartsWith("$repo\", [StringComparison]::OrdinalIgnoreCase) -or
            !$to.StartsWith("$source\", [StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid source entry' }
        if (!(Test-Path -LiteralPath $from -PathType Leaf)) { continue }
        if ((Get-Item -LiteralPath $from -Force).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw 'Unexpected source link' }
        $null = New-Item -ItemType Directory -Path (Split-Path -Parent $to) -Force
        Copy-Item -LiteralPath $from -Destination $to
        "$((Get-FileHash -LiteralPath $to).Hash) $entry"
    }
    [IO.File]::WriteAllLines((Join-Path $run 'source-sha256.txt'), [string[]]$hashes, (New-Object System.Text.UTF8Encoding $false))
    $null = New-Item -ItemType Junction -Path (Join-Path $source 'node_modules') -Target (Join-Path $repo 'node_modules')
    $null = New-Item -ItemType Directory -Path $bins -Force
    $manifest = Get-Content -LiteralPath (Join-Path $prior 'cuda-runtime.json') -Raw | ConvertFrom-Json
    foreach ($file in $manifest.files) {
        $name = [string]$file.path
        if ($name -eq 'ffmpeg.exe') {
            $to = Join-Path $bins "ffmpeg-$hostTriple.exe"
        } elseif ($name -match '^nemo-speech[\\/]' -or $name -match '^[\w.-]+\.dll$') {
            $to = [IO.Path]::GetFullPath((Join-Path $resources $name))
            if (!$to.StartsWith("$resources\", [StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid resource entry' }
        } else {
            continue
        }
        $from = Join-Path $prior $name
        if ((Get-FileHash -LiteralPath $from).Hash -ine $file.sha256) { throw "Input hash mismatch: $name" }
        $null = New-Item -ItemType Directory -Path (Split-Path -Parent $to) -Force
        Copy-Item -LiteralPath $from -Destination $to
    }
    # Post-call Silero VAD lives next to bazza.exe (state.rs vad_model); same sha256 as prepare-bundle.mjs.
    Copy-Item -LiteralPath $vadModel -Destination $resources
    "Snapshot $commit, source files: $($hashes.Count)"
}

if ($Action -in 'All', 'Build') {
    Test-Preflight
    Import-Module (Join-Path $VsPath 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')
    Enter-VsDevShell -VsInstallPath $VsPath -SkipAutomaticLocation -DevCmdArguments "-arch=amd64 -host_arch=amd64 -vcvars_ver=$MsvcVersion"
    $env:CARGO_HOME = Join-Path $Tools 'cargo'
    $env:RUSTUP_HOME = Join-Path $Tools 'rustup'
    $env:CARGO_TARGET_DIR = Join-Path $BuildEnv 'target'
    $env:CARGO_NET_OFFLINE = 'true'
    $env:CARGO_BUILD_JOBS = '6'
    $env:CUDA_PATH = $toolkit
    $env:CUDACXX = Join-Path $toolkit 'bin\nvcc.exe'
    $env:CUDAToolkit_ROOT = $toolkit
    $env:CUDAARCHS = '89'
    $env:CMAKE_GENERATOR = 'Ninja'
    $env:CMAKE_BUILD_PARALLEL_LEVEL = '4'
    $env:LIBCLANG_PATH = Join-Path $VsPath 'VC\Tools\Llvm\x64\bin'
    $env:COREPACK_ENABLE_AUTO_PIN = '0'
    $env:CI = 'true'
    $env:BAZZA_VARIANT = 'cuda'
    $env:Path = "$Tools\pnpm-shim;$toolkit\bin;$toolkit\bin\x64;$BuildEnv\ninja;$env:CARGO_HOME\bin;$env:LIBCLANG_PATH;$env:Path"
    Set-Location -LiteralPath $source
    cmd /c "cargo build --locked --offline --release -p bazza-service -p bazza-mcp -p bazza-diarize-cli 2>&1" |
        Tee-Object -FilePath (Join-Path $run 'sidecars.log')
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    foreach ($name in $sidecars) {
        Copy-Item -LiteralPath (Join-Path $env:CARGO_TARGET_DIR "release\$name.exe") -Destination (Join-Path $bins "$name-$hostTriple.exe")
    }
    cmd /c "corepack pnpm@10.33.0 tauri build --no-bundle --features cuda -- --locked --offline 2>&1" |
        Tee-Object -FilePath (Join-Path $run 'desktop.log')
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Copy-Item -LiteralPath (Join-Path $env:CARGO_TARGET_DIR 'release\bazza.exe') -Destination (Join-Path $run 'bazza.exe')
}

if ($Action -in 'All', 'Stage') {
    $manifest = Get-Content -LiteralPath (Join-Path $prior 'cuda-runtime.json') -Raw | ConvertFrom-Json
    $null = New-Item -ItemType Directory -Path $runtime -Force
    Copy-Item -LiteralPath (Join-Path $run 'bazza.exe') -Destination $runtime
    foreach ($name in @('ffmpeg') + $sidecars) {
        Copy-Item -LiteralPath (Join-Path $bins "$name-$hostTriple.exe") -Destination (Join-Path $runtime "$name.exe")
    }
    foreach ($dll in Get-ChildItem -LiteralPath $resources -File -Filter '*.dll') {
        Copy-Item -LiteralPath $dll.FullName -Destination $runtime
    }
    Copy-Item -LiteralPath (Join-Path $resources 'ggml-silero-v5.1.2.bin') -Destination $runtime
    Copy-Item -LiteralPath (Join-Path $resources 'nemo-speech') -Destination $runtime -Recurse
    # A complete CUDA payload is required; never publish an exe-only update.
    foreach ($name in 'ggml-cuda.dll', 'cudart64_13.dll', 'cublas64_13.dll', 'cublasLt64_13.dll', 'llama.dll', 'sherpa-onnx-c-api.dll', 'onnxruntime.dll', 'ggml-silero-v5.1.2.bin') {
        if (!(Test-Path -LiteralPath (Join-Path $runtime $name) -PathType Leaf)) { throw "Missing CUDA/native runtime: $name" }
    }
    foreach ($name in $sidecars) {
        $release = Join-Path $BuildEnv "target\release\$name.exe"
        if ((Get-FileHash -LiteralPath (Join-Path $runtime "$name.exe")).Hash -ine (Get-FileHash -LiteralPath $release).Hash) {
            throw "The runtime sidecar is not the newly built release: $name"
        }
    }
    $scanStarted = Get-Date
    Start-MpScan -ScanType CustomScan -ScanPath $runtime -ErrorAction Stop
    $events = Get-WinEvent -FilterHashtable @{
        LogName = 'Microsoft-Windows-Windows Defender/Operational'; Id = 1116, 1117; StartTime = $scanStarted
    } -ErrorAction SilentlyContinue
    if ($events | Where-Object { $_.Message.IndexOf($runtime, [StringComparison]::OrdinalIgnoreCase) -ge 0 }) {
        throw 'Windows Defender detected a staged file'
    }
    foreach ($name in @('bazza', 'ffmpeg') + $sidecars) {
        if (!(Test-Path -LiteralPath (Join-Path $runtime "$name.exe") -PathType Leaf)) { throw "Missing $name after scan" }
    }
    $files = @(Get-ChildItem -LiteralPath $runtime -File -Recurse | Sort-Object FullName | ForEach-Object {
        [ordered]@{
            path = $_.FullName.Substring($runtime.Length).TrimStart([char]92)
            bytes = $_.Length
            sha256 = (Get-FileHash -LiteralPath $_.FullName).Hash
        }
    })
    $receipt = [ordered]@{
        schema = 1
        staged_utc = [DateTimeOffset]::UtcNow.ToString('o')
        commit = (Get-Content -LiteralPath (Join-Path $run 'commit.txt') -Raw).Trim()
        runtime = $runtime
        exe = Join-Path $runtime 'bazza.exe'
        sha256 = ($files | Where-Object { $_.path -eq 'bazza.exe' }).sha256
        source_sha256 = (Get-FileHash -LiteralPath (Join-Path $run 'source-sha256.txt')).Hash
        features = @('cuda')
        variant = 'cuda'
        toolkit = $manifest.toolkit
        architecture = 'sm_89'
        service_profile = 'release'
        files = $files
    }
    [IO.File]::WriteAllText((Join-Path $runtime 'cuda-runtime.json'), ($receipt | ConvertTo-Json -Depth 6), (New-Object System.Text.UTF8Encoding $false))
    $receipt | Select-Object runtime, commit, sha256, @{n = 'file_count'; e = { $_.files.Count } }
}
