[CmdletBinding()]
param(
    [ValidateRange(1, 100)]
    [int]$KeepNewest = 3,
    [switch]$Apply
)

$ErrorActionPreference = "Stop"
$projectRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot)).TrimEnd('\', '/')
$archiveRoot = [IO.Path]::GetFullPath((Join-Path $projectRoot "artifacts\archive")).TrimEnd('\', '/')
$archivePrefix = $archiveRoot + [IO.Path]::DirectorySeparatorChar

if (-not (Test-Path -LiteralPath $archiveRoot -PathType Container)) {
    Write-Output "No artifacts/archive directory exists."
    exit 0
}

$archiveRootItem = Get-Item -LiteralPath $archiveRoot -Force
if (($archiveRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "Refusing to prune a reparse-point archive root: $archiveRoot"
}

$archives = @(
    Get-ChildItem -LiteralPath $archiveRoot -Directory -Force |
        Sort-Object LastWriteTimeUtc -Descending
)
$candidates = @($archives | Select-Object -Skip $KeepNewest)
[int64]$candidateBytes = 0

foreach ($candidate in $candidates) {
    $resolved = [IO.Path]::GetFullPath($candidate.FullName)
    if (-not $resolved.StartsWith($archivePrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Archive candidate escapes the archive root: $resolved"
    }
    if (($candidate.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing to prune a reparse-point archive: $resolved"
    }
    foreach ($nested in Get-ChildItem -LiteralPath $resolved -Recurse -Force) {
        if (($nested.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refusing to prune an archive containing a reparse point: $($nested.FullName)"
        }
        if (-not $nested.PSIsContainer) {
            $candidateBytes += $nested.Length
        }
    }
}

Write-Output "Archive retention preview: keep newest $KeepNewest of $($archives.Count); candidates=$($candidates.Count); bytes=$candidateBytes"
foreach ($candidate in $candidates) {
    Write-Output ("{0}`t{1:o}" -f $candidate.FullName, $candidate.LastWriteTimeUtc)
}

if (-not $Apply) {
    Write-Output "Preview only. Re-run with -Apply after reviewing every path."
    exit 0
}

foreach ($candidate in $candidates) {
    Remove-Item -LiteralPath $candidate.FullName -Recurse -Force
}
Write-Output "Pruned $($candidates.Count) historical build archive directories; current artifacts and newest $KeepNewest archives were preserved."
