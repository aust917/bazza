# Windows PowerShell 5.1 compatible: the desktop shortcut uses the built-in powershell.exe.
param([switch]$ShowErrors)

$ErrorActionPreference = 'Stop'

$mutex = [Threading.Mutex]::new($false, 'Local\BazzaPortable')
$locked = $false
try {
    try { $locked = $mutex.WaitOne(15000) } catch [Threading.AbandonedMutexException] { $locked = $true }
    if (!$locked) { throw 'Дождитесь окончания обновления Bazza Portable.' }

    $root = Get-Item -LiteralPath $PSScriptRoot
    $owner = Join-Path $root.FullName '.bazza-portable'
    if ($root.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint) -or
        (Get-Content -LiteralPath $owner -Raw).Trim() -ne 'bazza-portable-v1') {
        throw 'Каталог Bazza Portable не прошёл проверку.'
    }
    $release = Get-Content -LiteralPath (Join-Path $root.FullName 'current.json') -Raw | ConvertFrom-Json
    if ($release.build -cnotmatch '^[a-f0-9]{64}$' -or $release.exe_sha256 -cnotmatch '^[a-f0-9]{64}$') {
        throw 'Неверный идентификатор сборки.'
    }
    $runtime = Join-Path $root.FullName "builds\$($release.build)"
    $exe = Join-Path $runtime 'bazza.exe'
    if ((Get-FileHash -LiteralPath $exe).Hash -ine $release.exe_sha256) { throw 'Файл сборки повреждён.' }

    $running = @(Get-Process -Name bazza,hark -ErrorAction SilentlyContinue)
    if ($running | Where-Object { $_.Path -ine $exe }) {
        throw 'Закройте предыдущую Bazza перед запуском свежей portable-сборки.'
    }

    # The meeting library is permanent: updates never reset or replace it (library.json).
    $config = Get-Content -LiteralPath (Join-Path $root.FullName 'library.json') -Raw | ConvertFrom-Json
    $library = [string]$config.library
    $backups = [string]$config.backups
    foreach ($path in $library, $backups) {
        if ($path -notmatch '^[A-Za-z]:\\[^"<>|?*]+$') { throw "Неверный путь в library.json: $path" }
    }
    if (!(Test-Path -LiteralPath $library -PathType Container)) {
        throw "Папка библиотеки встреч недоступна: $library"
    }
    $profileDir = Join-Path $root.FullName 'profile'
    $webview = Join-Path $profileDir 'webview'

    $marker = Join-Path $profileDir 'build.txt'
    $profileBuild = if (Test-Path -LiteralPath $marker -PathType Leaf) { (Get-Content -LiteralPath $marker -Raw).Trim() }
    if ($profileBuild -ne $release.build) {
        if ($running.Count -gt 0) { throw 'Закройте Bazza Portable перед первым запуском новой сборки.' }
        # A new build may migrate the database forward; keep a restorable copy first.
        if (Test-Path -LiteralPath (Join-Path $library 'bazza.db') -PathType Leaf) {
            & (Join-Path $root.FullName 'Backup Bazza Library.ps1') -Library $library -Destination $backups -Kind update
        }
        New-Item -ItemType Directory -Path $profileDir -Force | Out-Null
        Set-Content -LiteralPath $marker -Value $release.build -Encoding ascii
    }

    $model = Join-Path $library 'models\ggml-large-v3-turbo.bin'
    if (!(Test-Path -LiteralPath $model -PathType Leaf) -or (Get-Item -LiteralPath $model).Length -ne $release.model_size) {
        $cache = Join-Path $env:LOCALAPPDATA 'Bazza-Review\model-cache\ggml-large-v3-turbo.bin'
        if ((Get-Item -LiteralPath $cache).Length -ne $release.model_size -or
            (Get-FileHash -LiteralPath $cache).Hash -ine $release.model_sha256) {
            throw 'Проверка кэша Whisper Large v3 Turbo не прошла.'
        }
        New-Item -ItemType Directory -Path (Split-Path -Parent $model) -Force | Out-Null
        Copy-Item -LiteralPath $cache -Destination "$model.part" -Force
        Move-Item -LiteralPath "$model.part" -Destination $model -Force
    }

    # Start-Process inherits this process environment (PowerShell 5.1 has no -Environment).
    $env:BAZZA_DATA_DIR = $library
    $env:WEBVIEW2_USER_DATA_FOLDER = $webview
    Remove-Item Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, Env:BAZZA_SMOKE -ErrorAction SilentlyContinue
    Start-Process -FilePath $exe -WorkingDirectory $runtime -ArgumentList '--portable' -WindowStyle Normal
} catch {
    if ($ShowErrors) {
        Add-Type -AssemblyName System.Windows.Forms
        [Windows.Forms.MessageBox]::Show($_.Exception.Message, 'Bazza Portable') | Out-Null
    }
    throw
} finally {
    if ($locked) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
