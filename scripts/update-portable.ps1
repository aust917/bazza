#Requires -Version 7.4
param([Parameter(Mandatory)][string]$Runtime)

$ErrorActionPreference = 'Stop'
$rootPath = Join-Path $env:LOCALAPPDATA 'Bazza-Portable'
$owner = Join-Path $rootPath '.bazza-portable'
if (!(Test-Path -LiteralPath $rootPath)) {
    New-Item -ItemType Directory -Path $rootPath | Out-Null
    Set-Content -LiteralPath $owner -Value 'bazza-portable-v1' -Encoding ascii
}
$root = Get-Item -LiteralPath $rootPath -Force
if ($root.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint) -or
    (Get-Content -LiteralPath $owner -Raw).Trim() -ne 'bazza-portable-v1') {
    throw 'Refusing to update an unrecognized portable directory.'
}
$source = (Resolve-Path -LiteralPath $Runtime).Path
$manifest = Get-Content -LiteralPath (Join-Path $source 'cuda-runtime.json') -Raw | ConvertFrom-Json
$hash = ([string]$manifest.sha256).ToLowerInvariant()
if ($manifest.schema -ne 1 -or $hash -cnotmatch '^[a-f0-9]{64}$' -or
    $hash -ine (Get-FileHash -LiteralPath (Join-Path $source 'bazza.exe')).Hash) {
    throw 'The staged CUDA runtime does not match its manifest.'
}
$payload = ($manifest.files | Sort-Object path | ForEach-Object { "$(([string]$_.sha256).ToLowerInvariant()) $($_.path)" }) -join "`n"
$build = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($payload))).ToLowerInvariant()
$catalog = Get-Content -LiteralPath (Join-Path $PSScriptRoot '../crates/bazza-models/catalog.json') -Raw | ConvertFrom-Json
$model = $catalog.models | Where-Object id -eq 'whisper-large-v3-turbo'
$modelPath = Join-Path $env:LOCALAPPDATA "Bazza-Review/model-cache/$($model.file)"
if ((Get-Item -LiteralPath $modelPath).Length -ne $model.size_bytes -or (Get-FileHash -LiteralPath $modelPath).Hash -ine $model.sha256) {
    throw 'The cached Whisper Large v3 Turbo failed verification.'
}

$mutex = [Threading.Mutex]::new($false, 'Local\BazzaPortable')
$locked = $false
try {
    try { $locked = $mutex.WaitOne(15000) } catch [Threading.AbandonedMutexException] { $locked = $true }
    if (!$locked) { throw 'Portable launcher is busy.' }
    $builds = New-Item -ItemType Directory -Path (Join-Path $root.FullName 'builds') -Force
    if ($builds.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw 'Unexpected portable builds directory.' }
    $target = Join-Path $builds.FullName $build
    if (!(Test-Path -LiteralPath $target)) {
        $stage = New-Item -ItemType Directory -Path (Join-Path $builds.FullName "stage-$([Guid]::NewGuid())")
        try {
            foreach ($entry in $manifest.files) {
                $from = [IO.Path]::GetFullPath((Join-Path $source $entry.path))
                $to = [IO.Path]::GetFullPath((Join-Path $stage.FullName $entry.path))
                if (!$from.StartsWith("$source\", [StringComparison]::OrdinalIgnoreCase) -or
                    !$to.StartsWith("$($stage.FullName)\", [StringComparison]::OrdinalIgnoreCase)) {
                    throw 'A runtime manifest path escapes its directory.'
                }
                New-Item -ItemType Directory -Path (Split-Path -Parent $to) -Force | Out-Null
                Copy-Item -LiteralPath $from -Destination $to
                if ((Get-FileHash -LiteralPath $to).Hash -ine $entry.sha256) { throw "Runtime file hash mismatch: $($entry.path)" }
            }
            if ((Get-FileHash -LiteralPath (Join-Path $stage.FullName 'bazza.exe')).Hash -ine $hash) { throw 'Missing staged desktop binary.' }
            Move-Item -LiteralPath $stage.FullName -Destination $target
        } finally {
            if ($stage.Parent.FullName -ieq $builds.FullName -and $stage.Name -match '^stage-[a-f0-9-]{36}$' -and
                (Test-Path -LiteralPath $stage.FullName)) {
                Remove-Item -LiteralPath $stage.FullName -Recurse -Force
            }
        }
    } else {
        if ((Get-Item -LiteralPath $target -Force).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
            throw 'Unexpected portable runtime directory.'
        }
        foreach ($entry in $manifest.files) {
            $file = [IO.Path]::GetFullPath((Join-Path $target $entry.path))
            if (!$file.StartsWith("$target\", [StringComparison]::OrdinalIgnoreCase) -or
                (Get-FileHash -LiteralPath $file).Hash -ine $entry.sha256) {
                throw 'Portable build verification failed.'
            }
        }
    }
    if ((Get-FileHash -LiteralPath (Join-Path $target 'bazza.exe')).Hash -ine $hash) { throw 'Portable build verification failed.' }

    $launcher = Join-Path $root.FullName 'Launch Bazza Portable.ps1'
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'launch-portable.ps1') -Destination $launcher -Force
    $temp = Join-Path $root.FullName "current-$([Guid]::NewGuid()).json"
    @{ build = $build; exe_sha256 = $hash; model_size = $model.size_bytes; model_sha256 = $model.sha256 } |
        ConvertTo-Json -Compress | Set-Content -LiteralPath $temp -Encoding utf8NoBOM
    [IO.File]::Move($temp, (Join-Path $root.FullName 'current.json'), $true)

    $desktop = [Environment]::GetFolderPath('DesktopDirectory')
    $shortcutPath = Join-Path $desktop 'Bazza Portable.lnk'
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = (Join-Path $PSHOME 'pwsh.exe')
    $shortcut.Arguments = "-NoProfile -WindowStyle Hidden -File `"$launcher`" -ShowErrors"
    $shortcut.WorkingDirectory = $root.FullName
    $shortcut.IconLocation = "$(Join-Path $target 'bazza.exe'),0"
    $shortcut.Save()

    $active = @(Get-Process -Name bazza,bazza-service,bazza-mcp,bazza-diarize,bazza-wgc,ffmpeg -ErrorAction SilentlyContinue)
    $unknownPath = $active | Where-Object { !$_.Path }
    foreach ($old in Get-ChildItem -LiteralPath $builds.FullName -Directory) {
        if (!$unknownPath -and $old.Name -ne $build -and $old.Name -cmatch '^[a-f0-9]{64}$' -and
            $old.Parent.FullName -ieq $builds.FullName -and !$old.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint) -and
            !($active | Where-Object { $_.Path -and $_.Path.StartsWith("$($old.FullName)\", [StringComparison]::OrdinalIgnoreCase) })) {
            Remove-Item -LiteralPath $old.FullName -Recurse -Force
        }
    }
    [pscustomobject]@{ Build = $build; ExeSha256 = $hash; Launcher = $launcher; Shortcut = $shortcutPath }
} finally {
    if ($locked) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
