#Requires -Version 7.4
param([switch]$ShowErrors)

$ErrorActionPreference = 'Stop'

function Clear-PortableDirectory([IO.DirectoryInfo]$Directory, [string[]]$Keep) {
    foreach ($entry in Get-ChildItem -LiteralPath $Directory.FullName -Force) {
        $expected = [IO.Path]::GetFullPath((Join-Path $Directory.FullName $entry.Name))
        if ($entry.FullName -ine $expected -or [IO.Path]::GetDirectoryName($expected) -ine $Directory.FullName) {
            throw 'Сброс профиля остановлен после проверки пути.'
        }
        if ($Keep -icontains $expected) { continue }

        $isLink = $entry.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)
        $containsKeptFile = $Keep | Where-Object { $_.StartsWith("$expected\", [StringComparison]::OrdinalIgnoreCase) }
        if ($entry.PSIsContainer -and !$isLink -and $containsKeptFile) {
            Clear-PortableDirectory $entry $Keep
        } elseif ($entry.PSIsContainer -and $isLink) {
            [IO.Directory]::Delete($expected)
        } else {
            Remove-Item -LiteralPath $expected -Force -Recurse:($entry.PSIsContainer)
        }
    }
}

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
    $runtime = Join-Path $root.FullName "builds/$($release.build)"
    $exe = Join-Path $runtime 'bazza.exe'
    if ((Get-FileHash -LiteralPath $exe).Hash -ine $release.exe_sha256) { throw 'Файл сборки повреждён.' }

    $running = @(Get-Process -Name bazza,hark -ErrorAction SilentlyContinue)
    if ($running | Where-Object { $_.Path -ine $exe }) {
        throw 'Закройте предыдущую Bazza перед запуском свежей portable-сборки.'
    }
    $profile = Join-Path $root.FullName 'profile'
    $library = Join-Path $profile 'library'
    $webview = Join-Path $profile 'webview'
    foreach ($path in $profile, $library, $webview) {
        if (Test-Path -LiteralPath $path) {
            $directory = Get-Item -LiteralPath $path -Force
            if (!$directory.PSIsContainer -or $directory.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint) -or
                $directory.FullName -ine [IO.Path]::GetFullPath($path)) {
                throw 'Профиль Bazza Portable находится за пределами каталога запуска.'
            }
        }
    }
    $marker = Join-Path $profile 'build.txt'
    $profileBuild = if (Test-Path -LiteralPath $marker -PathType Leaf) { (Get-Content -LiteralPath $marker -Raw).Trim() }
    if ($profileBuild -ne $release.build) {
        if ($running.Count -gt 0) { throw 'Закройте Bazza Portable перед сбросом медиатеки.' }
        $model = Join-Path $env:LOCALAPPDATA 'Bazza-Review/model-cache/ggml-large-v3-turbo.bin'
        if ((Get-Item -LiteralPath $model).Length -ne $release.model_size -or
            (Get-FileHash -LiteralPath $model).Hash -ine $release.model_sha256) {
            throw 'Проверка кэша Whisper Large v3 Turbo не прошла.'
        }
        if (Test-Path -LiteralPath $profile) {
            $old = Get-Item -LiteralPath $profile -Force
            if (!$old.PSIsContainer -or $old.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint) -or
                $old.FullName -ine [IO.Path]::GetFullPath($profile) -or $old.Parent.FullName -ine $root.FullName) {
                throw 'Сброс профиля остановлен после проверки пути.'
            }
            # Credential Manager binds both integrations to this exact library path.
            $attendeePath = Join-Path $library 'attendee-integration.json'
            $keep = @((Join-Path $library 'discord-integration.json'), $attendeePath)
            if (Test-Path -LiteralPath $attendeePath -PathType Leaf) {
                $settings = Get-Content -LiteralPath $attendeePath -Raw | ConvertFrom-Json
                foreach ($tlsFile in $settings.certificatePath, $settings.privateKeyPath) {
                    if ($tlsFile -and [IO.Path]::IsPathFullyQualified($tlsFile)) {
                        $keep += [IO.Path]::GetFullPath($tlsFile)
                    }
                }
            }
            Clear-PortableDirectory $old $keep
        }
        $models = Join-Path $library 'models'
        New-Item -ItemType Directory -Path $models -Force | Out-Null
        New-Item -ItemType HardLink -Path (Join-Path $models 'ggml-large-v3-turbo.bin') -Target $model | Out-Null
        Set-Content -LiteralPath $marker -Value $release.build -Encoding ascii
    }

    Start-Process -FilePath $exe -WorkingDirectory $runtime -ArgumentList '--portable' -WindowStyle Normal -Environment @{
        BAZZA_DATA_DIR = $library
        WEBVIEW2_USER_DATA_FOLDER = $webview
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = $null
        BAZZA_SMOKE = $null
    }
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
