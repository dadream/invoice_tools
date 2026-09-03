[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$tempRoot = [IO.Path]::GetFullPath(
    (Join-Path ([IO.Path]::GetTempPath()) ("invoice-release-version-test-" + [guid]::NewGuid().ToString("N")))
)
$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
if (-not $tempRoot.StartsWith($tempBase, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Unsafe release-version self-test path: $tempRoot"
}

try {
    New-Item -ItemType Directory -Path `
        (Join-Path $tempRoot "scripts"), `
        (Join-Path $tempRoot "src-tauri"), `
        (Join-Path $tempRoot "ui") | Out-Null
    Copy-Item -LiteralPath `
        (Join-Path $PSScriptRoot "set-version.ps1"), `
        (Join-Path $PSScriptRoot "assert-release-version.ps1") `
        -Destination (Join-Path $tempRoot "scripts")
    Copy-Item -LiteralPath `
        (Join-Path $projectRoot "src-tauri\tauri.conf.json"), `
        (Join-Path $projectRoot "src-tauri\Cargo.toml") `
        -Destination (Join-Path $tempRoot "src-tauri")
    Copy-Item -LiteralPath (Join-Path $projectRoot "ui\package.json") `
        -Destination (Join-Path $tempRoot "ui")
    Copy-Item -LiteralPath (Join-Path $projectRoot "Cargo.lock") -Destination $tempRoot

    & (Join-Path $tempRoot "scripts\set-version.ps1") -Version "0.2.0" | Out-Null
    $result = & (Join-Path $tempRoot "scripts\assert-release-version.ps1") -Tag "v0.2.0" |
        ConvertFrom-Json
    if ($result.version -ne "0.2.0" -or $result.tag -ne "v0.2.0") {
        throw "Release-version self-test returned unexpected metadata"
    }

    $mismatchRejected = $false
    try {
        & (Join-Path $tempRoot "scripts\assert-release-version.ps1") -Tag "v0.2.1" *> $null
    }
    catch {
        $mismatchRejected = $true
    }
    if (-not $mismatchRejected) {
        throw "Release-version self-test did not reject a mismatched tag"
    }
}
finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}

Write-Output "Release version self-test passed"
