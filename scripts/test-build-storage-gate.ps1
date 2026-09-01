[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$projectRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot)).TrimEnd('\', '/')
$temporaryRoot = [IO.Path]::GetFullPath((Join-Path $projectRoot ".tmp"))
$temporaryPrefix = $temporaryRoot.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
$testRoot = [IO.Path]::GetFullPath((Join-Path $temporaryRoot ("build-storage-self-test-" + [guid]::NewGuid().ToString("N"))))

if (-not $testRoot.StartsWith($temporaryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Unsafe storage self-test path: $testRoot"
}

try {
    New-Item -ItemType Directory -Path (Join-Path $testRoot "artifacts") -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $testRoot "Cargo.toml"), "[workspace]`r`nresolver = `"2`"`r`n")
    [IO.File]::WriteAllBytes((Join-Path $testRoot "artifacts\synthetic-build.bin"), [byte[]]::new(4096))

    & (Join-Path $PSScriptRoot "check-build-storage.ps1") `
        -Mode Preflight `
        -RepositoryRoot $testRoot `
        -DebugLimitGiB 1 `
        -ReleaseLimitGiB 1 `
        -TargetLimitGiB 2 `
        -ArtifactsLimitGiB 1 `
        -DependencyLimitGiB 1 `
        -TransientLimitGiB 1 `
        -MinimumFreeGiB 0 `
        -EvidencePath "artifacts/pass.json" | Out-Host

    $passEvidence = Get-Content -LiteralPath (Join-Path $testRoot "artifacts\pass.json") -Raw | ConvertFrom-Json
    if (-not $passEvidence.passed -or $passEvidence.measurements.artifactsBytes -lt 4096) {
        throw "Passing storage-gate evidence is incomplete"
    }

    $limitRejected = $false
    try {
        & (Join-Path $PSScriptRoot "check-build-storage.ps1") `
            -Mode Preflight `
            -RepositoryRoot $testRoot `
            -DebugLimitGiB 1 `
            -ReleaseLimitGiB 1 `
            -TargetLimitGiB 2 `
            -ArtifactsLimitGiB 0.000001 `
            -DependencyLimitGiB 1 `
            -TransientLimitGiB 1 `
            -MinimumFreeGiB 0 | Out-Host
    }
    catch {
        if ($_.Exception.Message -notmatch "artifacts") {
            throw
        }
        $limitRejected = $true
    }
    if (-not $limitRejected) {
        throw "Storage gate accepted an artifacts directory above its configured limit"
    }

    $escapeRejected = $false
    try {
        & (Join-Path $PSScriptRoot "check-build-storage.ps1") `
            -Mode Audit `
            -RepositoryRoot $testRoot `
            -MinimumFreeGiB 0 `
            -EvidencePath "..\escaped-storage-evidence.json" | Out-Host
    }
    catch {
        if ($_.Exception.Message -notmatch "inside repository root") {
            throw
        }
        $escapeRejected = $true
    }
    if (-not $escapeRejected) {
        throw "Storage gate accepted an evidence path outside the repository"
    }
    if (Test-Path -LiteralPath (Join-Path $temporaryRoot "escaped-storage-evidence.json")) {
        throw "Storage gate wrote escaped evidence before rejecting the path"
    }

    Write-Output "Build storage gate self-test passed"
}
finally {
    if (Test-Path -LiteralPath $testRoot) {
        $resolvedTestRoot = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $testRoot))
        $testRootItem = Get-Item -LiteralPath $resolvedTestRoot -Force
        if (-not $resolvedTestRoot.StartsWith($temporaryPrefix, [StringComparison]::OrdinalIgnoreCase) -or
            ($testRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refusing to remove unsafe storage self-test directory: $resolvedTestRoot"
        }
        Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force
    }
}
