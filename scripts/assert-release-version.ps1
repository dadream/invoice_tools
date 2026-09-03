[CmdletBinding()]
param(
    [ValidatePattern('^$|^v?[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$')]
    [string]$Tag = "",
    [switch]$RequireCleanGit
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$semverPattern = '^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'

function Get-RequiredRegexCapture {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Pattern,
        [Parameter(Mandatory = $true)][string]$Label
    )

    $text = Get-Content -LiteralPath $Path -Raw -Encoding UTF8
    $matches = [regex]::Matches($text, $Pattern)
    if ($matches.Count -ne 1) {
        throw "$Label must occur exactly once in $Path; found $($matches.Count)"
    }
    return [string]$matches[0].Groups[1].Value
}

$tauriPath = Join-Path $projectRoot "src-tauri\tauri.conf.json"
$cargoPath = Join-Path $projectRoot "src-tauri\Cargo.toml"
$uiPath = Join-Path $projectRoot "ui\package.json"
$lockPath = Join-Path $projectRoot "Cargo.lock"

$tauriVersion = [string]((Get-Content -LiteralPath $tauriPath -Raw -Encoding UTF8 | ConvertFrom-Json).version)
$cargoVersion = Get-RequiredRegexCapture `
    -Path $cargoPath `
    -Pattern '(?ms)^\[package\]\s*$.*?^version\s*=\s*"([^"]+)"\s*$' `
    -Label "src-tauri Cargo package version"
$uiVersion = [string]((Get-Content -LiteralPath $uiPath -Raw -Encoding UTF8 | ConvertFrom-Json).version)
$lockVersion = Get-RequiredRegexCapture `
    -Path $lockPath `
    -Pattern '(?ms)^\[\[package\]\]\s*$\s*^name\s*=\s*"invoice-assistant"\s*$\s*^version\s*=\s*"([^"]+)"\s*$' `
    -Label "Cargo.lock invoice-assistant version"

$versions = [ordered]@{
    tauri = $tauriVersion
    cargo = $cargoVersion
    ui = $uiVersion
    cargoLock = $lockVersion
}
foreach ($entry in $versions.GetEnumerator()) {
    if ($entry.Value -notmatch $semverPattern) {
        throw "$($entry.Key) contains an invalid semantic version: $($entry.Value)"
    }
}

$distinctVersions = @($versions.Values | Sort-Object -Unique)
if ($distinctVersions.Count -ne 1) {
    throw "Release versions are inconsistent: $($versions | ConvertTo-Json -Compress)"
}
$version = [string]$distinctVersions[0]

$normalizedTag = $Tag
if ($normalizedTag.StartsWith("refs/tags/", [StringComparison]::OrdinalIgnoreCase)) {
    $normalizedTag = $normalizedTag.Substring(10)
}
if (-not [string]::IsNullOrWhiteSpace($normalizedTag)) {
    if (-not $normalizedTag.StartsWith("v", [StringComparison]::Ordinal)) {
        throw "Release tag must start with v: $normalizedTag"
    }
    $tagVersion = $normalizedTag.Substring(1)
    if ($tagVersion -ne $version) {
        throw "Release tag $normalizedTag does not match application version $version"
    }
}

if ($RequireCleanGit) {
    $gitStatus = @(& git -C $projectRoot status --porcelain=v1 --untracked-files=all)
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to inspect Git worktree"
    }
    if ($gitStatus.Count -ne 0) {
        throw "Release source worktree is not clean"
    }
}

$result = [ordered]@{
    version = $version
    tag = $(if ([string]::IsNullOrWhiteSpace($normalizedTag)) { $null } else { $normalizedTag })
    cleanGitRequired = [bool]$RequireCleanGit
    files = $versions
}
$result | ConvertTo-Json -Depth 4
