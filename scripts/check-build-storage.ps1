[CmdletBinding()]
param(
    [ValidateSet("Audit", "Preflight", "Postflight")]
    [string]$Mode = "Audit",
    [string]$RepositoryRoot,
    [switch]$AutoCleanDev,
    [double]$DebugLimitGiB = 20,
    [double]$ReleaseLimitGiB = 12,
    [double]$TargetLimitGiB = 32,
    [double]$ArtifactsLimitGiB = 2,
    [double]$DependencyLimitGiB = 2,
    [double]$TransientLimitGiB = 2,
    [double]$MinimumFreeGiB = -1,
    [string]$EvidencePath
)

$ErrorActionPreference = "Stop"
$GiB = [int64]1073741824

if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
    $RepositoryRoot = Split-Path -Parent $PSScriptRoot
}
$RepositoryRoot = [IO.Path]::GetFullPath($RepositoryRoot).TrimEnd('\', '/')
$repositoryPrefix = $RepositoryRoot + [IO.Path]::DirectorySeparatorChar

if (-not (Test-Path -LiteralPath (Join-Path $RepositoryRoot "Cargo.toml") -PathType Leaf)) {
    throw "Repository root does not contain Cargo.toml: $RepositoryRoot"
}

function Resolve-RepositoryPath {
    param([Parameter(Mandatory = $true)][string]$RelativePath)

    $resolved = [IO.Path]::GetFullPath((Join-Path $RepositoryRoot $RelativePath))
    if (-not $resolved.StartsWith($repositoryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Path escapes repository root: $RelativePath"
    }
    return $resolved
}

function Measure-Tree {
    param([Parameter(Mandatory = $true)][string]$RelativePath)

    $fullPath = Resolve-RepositoryPath $RelativePath
    if (-not (Test-Path -LiteralPath $fullPath -PathType Container)) {
        return [pscustomobject]@{
            path = $RelativePath.Replace('\', '/')
            bytes = [int64]0
            files = [int64]0
            reparsePoints = @()
        }
    }

    $rootItem = Get-Item -LiteralPath $fullPath -Force
    if (($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Generated directory must not be a reparse point: $fullPath"
    }

    [int64]$bytes = 0
    [int64]$files = 0
    $reparsePoints = [Collections.Generic.List[string]]::new()
    $pending = [Collections.Generic.Stack[string]]::new()
    $pending.Push($fullPath)

    while ($pending.Count -gt 0) {
        $current = $pending.Pop()
        foreach ($file in [IO.Directory]::EnumerateFiles($current)) {
            $fileInfo = [IO.FileInfo]::new($file)
            $bytes += $fileInfo.Length
            $files++
        }
        foreach ($directory in [IO.Directory]::EnumerateDirectories($current)) {
            $directoryInfo = [IO.DirectoryInfo]::new($directory)
            if (($directoryInfo.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                $reparsePoints.Add($directory) | Out-Null
            }
            else {
                $pending.Push($directory)
            }
        }
    }

    return [pscustomobject]@{
        path = $RelativePath.Replace('\', '/')
        bytes = $bytes
        files = $files
        reparsePoints = @($reparsePoints)
    }
}

function Get-CargoExecutable {
    $command = Get-Command cargo -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        return $command.Source
    }

    $fallback = Join-Path ([Environment]::GetFolderPath("UserProfile")) ".cargo\bin\cargo.exe"
    if (-not (Test-Path -LiteralPath $fallback -PathType Leaf)) {
        throw "cargo was not found"
    }
    return $fallback
}

function Assert-CleanupIdle {
    $blockingNames = @("cargo", "rustc", "rust-analyzer", "invoice-assistant", "invoice-ocr-worker")
    $blocking = @(
        Get-Process -ErrorAction SilentlyContinue |
            Where-Object { $blockingNames -contains $_.ProcessName }
    )
    if ($blocking.Count -gt 0) {
        $description = ($blocking | ForEach-Object { "$($_.ProcessName):$($_.Id)" }) -join ", "
        throw "Development artifacts exceed the limit, but cleanup is unsafe while these processes run: $description"
    }
}

function Convert-ToGiB {
    param([int64]$Bytes)
    return [Math]::Round($Bytes / $GiB, 2)
}

if ($MinimumFreeGiB -lt 0) {
    $MinimumFreeGiB = switch ($Mode) {
        "Preflight" { 20 }
        "Postflight" { 8 }
        default { 0 }
    }
}

$debugBefore = Measure-Tree "target\debug"
$cleanupApplied = $false
$cleanupPreview = $null

if ($Mode -ne "Audit" -and $AutoCleanDev -and $debugBefore.bytes -gt [int64]($DebugLimitGiB * $GiB)) {
    Assert-CleanupIdle
    $cargoExe = Get-CargoExecutable

    $previewOutput = & $cargoExe clean --manifest-path (Join-Path $RepositoryRoot "Cargo.toml") --profile dev --dry-run 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "cargo clean dry-run failed with exit code $LASTEXITCODE"
    }
    $cleanupPreview = ($previewOutput | Select-Object -Last 2) -join "`n"

    & $cargoExe clean --manifest-path (Join-Path $RepositoryRoot "Cargo.toml") --profile dev
    if ($LASTEXITCODE -ne 0) {
        throw "cargo clean --profile dev failed with exit code $LASTEXITCODE"
    }
    $cleanupApplied = $true
}

$debug = Measure-Tree "target\debug"
$release = Measure-Tree "target\release"
$artifacts = Measure-Tree "artifacts"
$dependencies = Measure-Tree "ui\node_modules"
$temporary = Measure-Tree ".tmp"
$temporaryPdf = Measure-Tree "tmp"
$targetOtherBytes = [int64]0
$targetPath = Resolve-RepositoryPath "target"
if (Test-Path -LiteralPath $targetPath -PathType Container) {
    foreach ($item in Get-ChildItem -LiteralPath $targetPath -Force) {
        if ($item.Name -notin @("debug", "release")) {
            if ($item.PSIsContainer) {
                $relative = "target\$($item.Name)"
                $targetOtherBytes += (Measure-Tree $relative).bytes
            }
            else {
                $targetOtherBytes += $item.Length
            }
        }
    }
}
$targetBytes = $debug.bytes + $release.bytes + $targetOtherBytes
$transientBytes = $temporary.bytes + $temporaryPdf.bytes

$driveRoot = [IO.Path]::GetPathRoot($RepositoryRoot)
$drive = [IO.DriveInfo]::new($driveRoot)
$freeBytes = [int64]$drive.AvailableFreeSpace
$violations = [Collections.Generic.List[string]]::new()

foreach ($tree in @($debug, $release, $artifacts, $dependencies, $temporary, $temporaryPdf)) {
    if ($tree.reparsePoints.Count -gt 0) {
        $violations.Add("Unexpected reparse point below generated tree $($tree.path)") | Out-Null
    }
}
if ($debug.bytes -gt [int64]($DebugLimitGiB * $GiB)) {
    $violations.Add("target/debug is $(Convert-ToGiB $debug.bytes) GiB; limit is $DebugLimitGiB GiB") | Out-Null
}
if ($release.bytes -gt [int64]($ReleaseLimitGiB * $GiB)) {
    $violations.Add("target/release is $(Convert-ToGiB $release.bytes) GiB; limit is $ReleaseLimitGiB GiB") | Out-Null
}
if ($targetBytes -gt [int64]($TargetLimitGiB * $GiB)) {
    $violations.Add("target is $(Convert-ToGiB $targetBytes) GiB; limit is $TargetLimitGiB GiB") | Out-Null
}
if ($artifacts.bytes -gt [int64]($ArtifactsLimitGiB * $GiB)) {
    $violations.Add("artifacts is $(Convert-ToGiB $artifacts.bytes) GiB; limit is $ArtifactsLimitGiB GiB; preview scripts/prune-build-archives.ps1") | Out-Null
}
if ($dependencies.bytes -gt [int64]($DependencyLimitGiB * $GiB)) {
    $violations.Add("ui/node_modules is $(Convert-ToGiB $dependencies.bytes) GiB; limit is $DependencyLimitGiB GiB") | Out-Null
}
if ($transientBytes -gt [int64]($TransientLimitGiB * $GiB)) {
    $violations.Add("combined .tmp/tmp is $(Convert-ToGiB $transientBytes) GiB; limit is $TransientLimitGiB GiB") | Out-Null
}
if ($freeBytes -lt [int64]($MinimumFreeGiB * $GiB)) {
    $violations.Add("drive $driveRoot has $(Convert-ToGiB $freeBytes) GiB free; required reserve is $MinimumFreeGiB GiB") | Out-Null
}

$result = [ordered]@{
    schemaVersion = 1
    measuredAtUtc = [DateTime]::UtcNow.ToString("o")
    mode = $Mode
    repositoryRoot = $RepositoryRoot
    limitsGiB = [ordered]@{
        debug = $DebugLimitGiB
        release = $ReleaseLimitGiB
        target = $TargetLimitGiB
        artifacts = $ArtifactsLimitGiB
        dependencies = $DependencyLimitGiB
        transient = $TransientLimitGiB
        minimumFree = $MinimumFreeGiB
    }
    measurements = [ordered]@{
        debugBeforeBytes = $debugBefore.bytes
        debugBytes = $debug.bytes
        releaseBytes = $release.bytes
        targetBytes = $targetBytes
        artifactsBytes = $artifacts.bytes
        dependencyBytes = $dependencies.bytes
        transientBytes = $transientBytes
        freeBytes = $freeBytes
    }
    cleanup = [ordered]@{
        applied = $cleanupApplied
        previewSummary = $cleanupPreview
        profile = $(if ($cleanupApplied) { "dev" } else { $null })
    }
    violations = @($violations)
    passed = ($violations.Count -eq 0)
}

Write-Output ("Build storage {0}: debug {1}/{2} GiB, release {3}/{4} GiB, target {5}/{6} GiB, artifacts {7}/{8} GiB, free {9} GiB" -f
    $Mode,
    (Convert-ToGiB $debug.bytes), $DebugLimitGiB,
    (Convert-ToGiB $release.bytes), $ReleaseLimitGiB,
    (Convert-ToGiB $targetBytes), $TargetLimitGiB,
    (Convert-ToGiB $artifacts.bytes), $ArtifactsLimitGiB,
    (Convert-ToGiB $freeBytes))
if ($cleanupApplied) {
    Write-Output ("Cargo dev cleanup reclaimed approximately {0} GiB" -f (Convert-ToGiB ($debugBefore.bytes - $debug.bytes)))
}

if (-not [string]::IsNullOrWhiteSpace($EvidencePath)) {
    $resolvedEvidence = if ([IO.Path]::IsPathRooted($EvidencePath)) {
        [IO.Path]::GetFullPath($EvidencePath)
    }
    else {
        [IO.Path]::GetFullPath((Join-Path $RepositoryRoot $EvidencePath))
    }
    if (-not $resolvedEvidence.StartsWith($repositoryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Evidence path must stay inside repository root: $resolvedEvidence"
    }
    $evidenceDirectory = Split-Path -Parent $resolvedEvidence
    New-Item -ItemType Directory -Path $evidenceDirectory -Force | Out-Null
    $result | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $resolvedEvidence -Encoding utf8
    Write-Output "Storage evidence: $resolvedEvidence"
}

if ($Mode -ne "Audit" -and $violations.Count -gt 0) {
    throw ("Build storage gate failed:`n- " + ($violations -join "`n- "))
}
