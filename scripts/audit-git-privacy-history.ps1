[CmdletBinding()]
param(
    [ValidateSet("not_attempted", "unreachable", "matched", "different")]
    [string]$RemoteLiveStatus = "not_attempted",

    [string]$RemoteLiveHash,

    [string]$EvidencePath = "artifacts/git-privacy-history-audit.validation.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$artifactsRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot "artifacts"))
$resolvedEvidence = if ([IO.Path]::IsPathRooted($EvidencePath)) {
    [IO.Path]::GetFullPath($EvidencePath)
} else {
    [IO.Path]::GetFullPath((Join-Path $repoRoot $EvidencePath))
}
$artifactPrefix = $artifactsRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if (-not $resolvedEvidence.StartsWith($artifactPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Evidence must be written under artifacts"
}

function Invoke-Git {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)

    $output = @(& git @Arguments 2>&1)
    if ($LASTEXITCODE -ne 0) {
        throw "Git read-only audit command failed"
    }
    return $output
}

function Get-LegacyTreeSummary {
    param([Parameter(Mandatory = $true)][string]$Treeish)

    $lines = @(Invoke-Git -Arguments @("ls-tree", "-r", "-l", $Treeish, "--", "fixtures/test-images"))
    $entries = @(
        foreach ($line in $lines) {
            if ($line -match '^\d+\s+blob\s+([0-9a-f]{40})\s+(\d+)\t(.+)$') {
                [pscustomobject]@{
                    blob = $Matches[1]
                    bytes = [long]$Matches[2]
                    path = $Matches[3]
                }
            }
        }
    )
    return [ordered]@{
        files = $entries.Count
        bytes = [long](($entries | Measure-Object -Property bytes -Sum).Sum ?? 0)
        uniqueBlobs = @($entries.blob | Sort-Object -Unique).Count
    }
}

Push-Location -LiteralPath $repoRoot
try {
    $topLevel = [IO.Path]::GetFullPath([string](@(Invoke-Git -Arguments @("rev-parse", "--show-toplevel"))[0]))
    if ($topLevel -ne $repoRoot) {
        throw "Audit was not started at the expected repository root"
    }

    $head = [string](@(Invoke-Git -Arguments @("rev-parse", "HEAD"))[0])
    $branch = [string](@(Invoke-Git -Arguments @("branch", "--show-current"))[0])
    $remoteUrls = @(Invoke-Git -Arguments @("remote", "get-url", "--all", "origin"))
    $originMain = $null
    & git show-ref --verify --quiet refs/remotes/origin/main
    if ($LASTEXITCODE -eq 0) {
        $originMain = [string](@(Invoke-Git -Arguments @("rev-parse", "refs/remotes/origin/main"))[0])
    }

    $headLegacy = Get-LegacyTreeSummary -Treeish "HEAD"
    $originLegacy = if ($null -ne $originMain) {
        Get-LegacyTreeSummary -Treeish "refs/remotes/origin/main"
    } else {
        $null
    }

    $trackedLegacy = @(Invoke-Git -Arguments @("ls-files", "--", "fixtures/test-images"))
    $legacyStatus = @(Invoke-Git -Arguments @("status", "--porcelain=v1", "--", "fixtures/test-images"))
    $workingTreeDeleted = @($legacyStatus | Where-Object { $_ -match '^\s*D\s' }).Count
    $historyCommits = @(
        Invoke-Git -Arguments @(
            "log", "--format=%H", "--all", "--",
            "fixtures/test-images", "fixtures/manifest.toml"
        ) | Sort-Object -Unique
    )
    $historyObjects = @(
        Invoke-Git -Arguments @(
            "rev-list", "--objects", "--all", "--",
            "fixtures/test-images", "fixtures/manifest.toml"
        ) | Sort-Object -Unique
    )

    $commits = @(Invoke-Git -Arguments @("rev-list", "--all"))
    $passwordAssignmentPathHits = 0
    $fullQqEmailPathHits = 0
    foreach ($commit in $commits) {
        $passwordHits = @(& git grep -I -l -E 'INVOICE_IMAP_PASSWORD[[:space:]]*=[[:space:]]*[^$[:space:]]' $commit -- . 2>$null)
        if ($LASTEXITCODE -eq 0) { $passwordAssignmentPathHits += $passwordHits.Count }
        elseif ($LASTEXITCODE -ne 1) { throw "Historical password heuristic scan failed" }

        $emailHits = @(& git grep -I -l -E '[0-9]{6,}@qq\.com' $commit -- . 2>$null)
        if ($LASTEXITCODE -eq 0) { $fullQqEmailPathHits += $emailHits.Count }
        elseif ($LASTEXITCODE -ne 1) { throw "Historical email heuristic scan failed" }
    }

    $quarantine = Join-Path $repoRoot ".private-fixtures-quarantine"
    $quarantineFiles = @()
    if (Test-Path -LiteralPath $quarantine -PathType Container) {
        $quarantineItem = Get-Item -LiteralPath $quarantine -Force
        if ($quarantineItem.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "Private fixture quarantine cannot be a reparse point"
        }
        $quarantineFiles = @(Get-ChildItem -LiteralPath $quarantine -Recurse -File -Force)
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zipFiles = @(Get-ChildItem -LiteralPath $artifactsRoot -Recurse -File -Filter "*.zip")
    $forbiddenZipEntries = 0
    foreach ($zipFile in $zipFiles) {
        $archive = [IO.Compression.ZipFile]::OpenRead($zipFile.FullName)
        try {
            $forbiddenZipEntries += @(
                $archive.Entries | Where-Object {
                    $_.FullName -match '(?i)(^|/)(fixtures/test-images|fixtures/samples)(/|$)' -or
                    $_.Name -match '(?i)(^\.env|\.eml$|\.db$|\.pfx$|\.p12$|\.pem$|\.key$|\.log$)'
                }
            ).Count
        }
        finally {
            $archive.Dispose()
        }
    }

    $packFiles = @(Get-ChildItem -LiteralPath (Join-Path $repoRoot ".git\objects\pack") -File -ErrorAction SilentlyContinue)
    $manifestHeadBlob = [string](@(Invoke-Git -Arguments @("rev-parse", "HEAD:fixtures/manifest.toml"))[0])
    $worktreeManifestHash = (Get-FileHash -LiteralPath (Join-Path $repoRoot "fixtures\manifest.toml") -Algorithm SHA256).Hash

    $remoteTrackingMatchesHead = $null -ne $originMain -and $originMain -eq $head
    $remoteLiveMatchesTracking = if ($RemoteLiveStatus -eq "matched") {
        -not [string]::IsNullOrWhiteSpace($RemoteLiveHash) -and $RemoteLiveHash -eq $originMain
    } elseif ($RemoteLiveStatus -eq "different") {
        $false
    } else {
        $null
    }
    $currentWorktreeGatePassed = $trackedLegacy.Count -eq 20 -and
        $workingTreeDeleted -eq 20 -and
        -not (Test-Path -LiteralPath (Join-Path $repoRoot "fixtures\test-images"))
    $p0CanClose = $false

    $evidence = [ordered]@{
        schemaVersion = 1
        verification = "git-privacy-history-audit-v1"
        auditedAtUtc = [DateTime]::UtcNow.ToString("o")
        repository = [ordered]@{
            branch = $branch
            head = $head
            originUrls = $remoteUrls
            originMainTrackingHash = $originMain
            remoteTrackingMatchesHead = $remoteTrackingMatchesHead
            remoteLiveStatus = $RemoteLiveStatus
            remoteLiveHash = if ([string]::IsNullOrWhiteSpace($RemoteLiveHash)) { $null } else { $RemoteLiveHash }
            remoteLiveMatchesTracking = $remoteLiveMatchesTracking
        }
        legacyFixtureExposure = [ordered]@{
            head = $headLegacy
            originMainTracking = $originLegacy
            trackedLegacyPaths = $trackedLegacy.Count
            workingTreeDeletedPaths = $workingTreeDeleted
            legacyDirectoryAbsentFromWorkingTree = -not (Test-Path -LiteralPath (Join-Path $repoRoot "fixtures\test-images"))
            relevantReachableCommits = $historyCommits.Count
            relevantReachableObjects = $historyObjects.Count
            headManifestBlob = $manifestHeadBlob
            replacementWorktreeManifestSha256 = $worktreeManifestHash
        }
        heuristicHistoryScan = [ordered]@{
            commitsScanned = $commits.Count
            passwordAssignmentPathHits = $passwordAssignmentPathHits
            fullQqEmailPathHits = $fullQqEmailPathHits
            contentValuesPersistedInEvidence = $false
            limitation = "heuristic path-hit scan; not a substitute for a dedicated secret scanner over rewritten history"
        }
        localContainment = [ordered]@{
            currentWorktreeLegacyRemovalPrepared = $currentWorktreeGatePassed
            quarantineFiles = $quarantineFiles.Count
            quarantineBytes = [long](($quarantineFiles | Measure-Object -Property Length -Sum).Sum ?? 0)
            artifactZipFilesScanned = $zipFiles.Count
            artifactForbiddenEntries = $forbiddenZipEntries
            gitPackFiles = $packFiles.Count
            gitPackBytes = [long](($packFiles | Measure-Object -Property Length -Sum).Sum ?? 0)
        }
        conclusion = [ordered]@{
            p0Defect = "PRIV-001"
            p0CanClose = $p0CanClose
            currentHeadContainsLegacyFixtures = $headLegacy.files -gt 0
            originTrackingHeadContainsLegacyFixtures = $null -ne $originLegacy -and $originLegacy.files -gt 0
            removalIsCommitted = $headLegacy.files -eq 0
            controlledHistoryRewriteRequired = $true
            remoteAndCacheCleanupRequired = $true
            cleanCloneValidationRequired = $true
            productOwnerConfirmationRequired = $true
        }
    }
    $json = $evidence | ConvertTo-Json -Depth 8
    [IO.File]::WriteAllText($resolvedEvidence, $json + "`n", [Text.UTF8Encoding]::new($false))

    Write-Output "verification=git-privacy-history-audit-v1"
    Write-Output "head_legacy_files=$($headLegacy.files)"
    Write-Output "origin_tracking_legacy_files=$(if ($null -eq $originLegacy) { 'unknown' } else { $originLegacy.files })"
    Write-Output "working_tree_deleted_paths=$workingTreeDeleted"
    Write-Output "artifact_forbidden_entries=$forbiddenZipEntries"
    Write-Output "remote_live_status=$RemoteLiveStatus"
    Write-Output "p0_can_close=false"
    Write-Output "evidence=$resolvedEvidence"
}
finally {
    Pop-Location
}
