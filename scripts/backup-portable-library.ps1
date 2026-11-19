# Windows PowerShell 5.1 compatible. Copies the meeting library into a new dated snapshot folder.
# Every snapshot is a complete, restorable copy; files unchanged since the previous snapshot are
# NTFS hard links to it, so daily copies cost only the new data. models\ is skipped (re-downloadable).
param(
    [string]$Library,
    [string]$Destination,
    [ValidateSet('daily', 'update', 'manual')][string]$Kind = 'daily',
    [int]$Keep = 14
)
$ErrorActionPreference = 'Stop'

if (!$Library -or !$Destination) {
    $config = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'library.json') -Raw | ConvertFrom-Json
    if (!$Library) { $Library = [string]$config.library }
    if (!$Destination) { $Destination = [string]$config.backups }
}
$Library = (Get-Item -LiteralPath $Library).FullName.TrimEnd('\')
if (!(Test-Path -LiteralPath (Join-Path $Library 'bazza.db') -PathType Leaf)) { throw "No bazza.db in $Library" }
$Destination = (New-Item -ItemType Directory -Path $Destination -Force).FullName.TrimEnd('\')
$pattern = '^(daily|update|manual)-(\d{8}-\d{6})$'
$previous = Get-ChildItem -LiteralPath $Destination -Directory | Where-Object Name -Match $pattern |
    Sort-Object { $_.Name -replace $pattern, '$2' } | Select-Object -Last 1
$name = "$Kind-$(Get-Date -Format 'yyyyMMdd-HHmmss')"
$partial = Join-Path $Destination "$name.partial"
if (Test-Path -LiteralPath $partial) { throw "Snapshot already in progress: $partial" }

function Copy-LibraryFile([IO.FileInfo]$file) {
    $relative = $file.FullName.Substring($Library.Length + 1)
    $to = Join-Path $partial $relative
    New-Item -ItemType Directory -Path (Split-Path -Parent $to) -Force | Out-Null
    # A retry must replace, never write through, a hard link shared with the previous snapshot.
    if (Test-Path -LiteralPath $to) { Remove-Item -LiteralPath $to -Force }
    $old =if ($previous) { Get-Item -LiteralPath (Join-Path $previous.FullName $relative) -Force -ErrorAction SilentlyContinue }
    if ($old -and $old.Length -eq $file.Length -and $old.LastWriteTimeUtc -eq $file.LastWriteTimeUtc) {
        New-Item -ItemType HardLink -Path $to -Target $old.FullName | Out-Null
    } else {
        Copy-Item -LiteralPath $file.FullName -Destination $to -Force  # keeps LastWriteTime
    }
}

# Bazza may be running: copy the SQLite pair first and again until it did not change meanwhile.
# ponytail: timestamp check, not the SQLite backup API; fine for an idle app, retried while busy.
$database = @('bazza.db', 'bazza.db-wal') | ForEach-Object { Join-Path $Library $_ } | Where-Object { Test-Path -LiteralPath $_ }
$files = @(Get-ChildItem -LiteralPath $Library -Recurse -File -Force | Where-Object {
    !$_.FullName.StartsWith("$Library\models\", [StringComparison]::OrdinalIgnoreCase) -and
    $_.Name -ne 'bazza.db-shm' -and $database -notcontains $_.FullName
})
foreach ($file in $files) { Copy-LibraryFile $file }
for ($attempt = 1; ; $attempt++) {
    $before = $database | ForEach-Object { $item = Get-Item -LiteralPath $_; "$($item.Length)/$($item.LastWriteTimeUtc.Ticks)" }
    foreach ($path in $database) { Copy-LibraryFile (Get-Item -LiteralPath $path) }
    $after = $database | ForEach-Object { $item = Get-Item -LiteralPath $_; "$($item.Length)/$($item.LastWriteTimeUtc.Ticks)" }
    if (($before -join ',') -eq ($after -join ',')) { break }
    if ($attempt -ge 5) { throw 'The library database kept changing during the copy; try again later.' }
    Start-Sleep -Seconds 5
}

$source = @($files) + @($database | ForEach-Object { Get-Item -LiteralPath $_ })
$copied = @(Get-ChildItem -LiteralPath $partial -Recurse -File -Force)
$sourceBytes = ($source | Measure-Object Length -Sum).Sum
$copiedBytes = ($copied | Measure-Object Length -Sum).Sum
if ($copied.Count -ne $source.Count -or $copiedBytes -ne $sourceBytes) {
    throw "Snapshot check failed: $($copied.Count)/$($source.Count) files, $copiedBytes/$sourceBytes bytes in $partial"
}
Rename-Item -LiteralPath $partial -NewName $name

$removed = @()
if ($Kind -eq 'daily') {
    $daily = @(Get-ChildItem -LiteralPath $Destination -Directory | Where-Object Name -Match '^daily-\d{8}-\d{6}$' | Sort-Object Name)
    foreach ($old in $daily | Select-Object -First ([Math]::Max(0, $daily.Count - $Keep))) {
        Remove-Item -LiteralPath $old.FullName -Recurse -Force
        $removed += $old.Name
    }
}
$line = "$(Get-Date -Format s) $name files=$($copied.Count) bytes=$copiedBytes from=$Library removed=$($removed -join ';')"
Add-Content -LiteralPath (Join-Path $Destination 'backup.log') -Value $line -Encoding utf8
$line
