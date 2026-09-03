[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$')]
    [string]$Version
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$utf8NoBom = [System.Text.UTF8Encoding]::new($false)

function Replace-ExactlyOnce {
    param(
        [Parameter(Mandatory = $true)][string]$Text,
        [Parameter(Mandatory = $true)][string]$Pattern,
        [Parameter(Mandatory = $true)][scriptblock]$Replacement,
        [Parameter(Mandatory = $true)][string]$Label
    )

    $regex = [regex]::new($Pattern)
    $matches = $regex.Matches($Text)
    if ($matches.Count -ne 1) {
        throw "$Label must occur exactly once; found $($matches.Count)"
    }
    return $regex.Replace(
        $Text,
        [System.Text.RegularExpressions.MatchEvaluator]{ param($match) & $Replacement $match },
        1
    )
}

$paths = [ordered]@{
    tauri = Join-Path $projectRoot "src-tauri\tauri.conf.json"
    cargo = Join-Path $projectRoot "src-tauri\Cargo.toml"
    ui = Join-Path $projectRoot "ui\package.json"
    cargoLock = Join-Path $projectRoot "Cargo.lock"
}
$original = [ordered]@{}
foreach ($entry in $paths.GetEnumerator()) {
    $original[$entry.Key] = Get-Content -LiteralPath $entry.Value -Raw -Encoding UTF8
}

$updated = [ordered]@{}
$updated.tauri = Replace-ExactlyOnce `
    -Text $original.tauri `
    -Pattern '(?m)("version"\s*:\s*")[^"]+("\s*,)' `
    -Replacement { param($match) $match.Groups[1].Value + $Version + $match.Groups[2].Value } `
    -Label "Tauri version"
$updated.cargo = Replace-ExactlyOnce `
    -Text $original.cargo `
    -Pattern '(?ms)(^\[package\]\s*$.*?^version\s*=\s*")[^"]+("\s*$)' `
    -Replacement { param($match) $match.Groups[1].Value + $Version + $match.Groups[2].Value } `
    -Label "src-tauri Cargo package version"
$updated.ui = Replace-ExactlyOnce `
    -Text $original.ui `
    -Pattern '(?m)("version"\s*:\s*")[^"]+("\s*,)' `
    -Replacement { param($match) $match.Groups[1].Value + $Version + $match.Groups[2].Value } `
    -Label "UI package version"
$updated.cargoLock = Replace-ExactlyOnce `
    -Text $original.cargoLock `
    -Pattern '(?ms)(^\[\[package\]\]\s*$\s*^name\s*=\s*"invoice-assistant"\s*$\s*^version\s*=\s*")[^"]+("\s*$)' `
    -Replacement { param($match) $match.Groups[1].Value + $Version + $match.Groups[2].Value } `
    -Label "Cargo.lock invoice-assistant version"

try {
    foreach ($entry in $paths.GetEnumerator()) {
        [IO.File]::WriteAllText($entry.Value, [string]$updated[$entry.Key], $utf8NoBom)
    }
    & (Join-Path $PSScriptRoot "assert-release-version.ps1") -Tag "v$Version" | Out-Host
}
catch {
    foreach ($entry in $paths.GetEnumerator()) {
        [IO.File]::WriteAllText($entry.Value, [string]$original[$entry.Key], $utf8NoBom)
    }
    throw
}

Write-Output "Application version set to $Version"
