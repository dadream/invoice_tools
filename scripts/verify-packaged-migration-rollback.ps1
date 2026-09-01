[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ApplicationPath,
    [string]$EvidencePath = "artifacts/packaged-migration-rollback.validation.json",
    [switch]$KeepTestData
)

$ErrorActionPreference = "Stop"
$CargoPath = Join-Path $env:USERPROFILE ".cargo/bin/cargo.exe"
$RepositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$ApplicationPath = [IO.Path]::GetFullPath($ApplicationPath)
$EvidencePath = [IO.Path]::GetFullPath((Join-Path $RepositoryRoot $EvidencePath))
$TempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
$TestRoot = Join-Path $TempBase ("InvoiceAssistant packaged migration " + [Guid]::NewGuid().ToString("N"))
$DataRoot = Join-Path $TestRoot "Data"
$LedgerPath = Join-Path $DataRoot "ledger.db"
$SnapshotDirectory = Join-Path $DataRoot "migration-backups"
$ExpectedSchemaVersion = "17"

function Assert-TestPath([string]$Path) {
    $resolved = [IO.Path]::GetFullPath($Path)
    if (-not $resolved.StartsWith($TempBase + '\', [StringComparison]::OrdinalIgnoreCase) -or
        -not $resolved.StartsWith($TestRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing operation outside validated test root: $resolved"
    }
}

function Invoke-Probe([string]$Command, [string]$Path) {
    $output = & $script:ProbePath $Command $Path
    if ($LASTEXITCODE -ne 0) {
        throw "migration probe failed: $Command $Path"
    }
    $result = @{}
    foreach ($line in $output) {
        $parts = $line -split '=', 2
        if ($parts.Count -eq 2) {
            $result[$parts[0]] = $parts[1]
        }
    }
    return $result
}

function Start-And-WaitForMigration([int]$ExpectedSnapshotCount) {
    $process = Start-Process -FilePath $ApplicationPath -Environment @{
        INVOICE_ASSISTANT_HOME = $DataRoot
    } -WindowStyle Hidden -PassThru
    try {
        $deadline = [DateTime]::UtcNow.AddSeconds(20)
        do {
            Start-Sleep -Milliseconds 250
            if ($process.HasExited) {
                throw "packaged application exited before migration completed (exit $($process.ExitCode))"
            }
            $snapshots = @(Get-ChildItem -LiteralPath $SnapshotDirectory -File -Filter '*.db' -ErrorAction SilentlyContinue)
            try {
                $inspection = Invoke-Probe "inspect" $LedgerPath
            }
            catch {
                $inspection = $null
            }
            if ($null -ne $inspection -and
                $inspection.version -eq $ExpectedSchemaVersion -and
                $inspection.integrity -eq "ok" -and
                $snapshots.Count -eq $ExpectedSnapshotCount) {
                return [pscustomobject]@{
                    Process = $process
                    Inspection = $inspection
                    Snapshots = $snapshots
                }
            }
        } while ([DateTime]::UtcNow -lt $deadline)
        throw "packaged migration did not reach schema v$ExpectedSchemaVersion with $ExpectedSnapshotCount snapshot(s)"
    }
    catch {
        if (-not $process.HasExited) {
            Stop-Process -Id $process.Id -Force
            $process.WaitForExit()
        }
        throw
    }
}

function Stop-TestApplication([Diagnostics.Process]$Process) {
    if ($Process.HasExited) {
        return
    }
    [void]$Process.CloseMainWindow()
    if (-not $Process.WaitForExit(3000)) {
        Stop-Process -Id $Process.Id -Force
        $Process.WaitForExit()
    }
}

if (-not (Test-Path -LiteralPath $ApplicationPath -PathType Leaf)) {
    throw "Application does not exist: $ApplicationPath"
}
if (-not (Test-Path -LiteralPath $CargoPath -PathType Leaf)) {
    throw "Cargo does not exist: $CargoPath"
}
Assert-TestPath $DataRoot
[IO.Directory]::CreateDirectory($DataRoot) | Out-Null

Push-Location $RepositoryRoot
try {
    & $CargoPath build -p invoice-store --example migration_probe --locked
    if ($LASTEXITCODE -ne 0) {
        throw "failed to build migration probe"
    }
    $script:ProbePath = Join-Path $RepositoryRoot "target/debug/examples/migration_probe.exe"
    if (-not (Test-Path -LiteralPath $script:ProbePath -PathType Leaf)) {
        throw "migration probe executable is missing"
    }

    $legacy = Invoke-Probe "create-v5" $LedgerPath
    if ($legacy.version -ne "5" -or $legacy.integrity -ne "ok" -or $legacy.concur_tables -ne "0") {
        throw "legacy fixture is not the expected v5 database"
    }
    $legacySha256 = (Get-FileHash -LiteralPath $LedgerPath -Algorithm SHA256).Hash
    $applicationSha256Before = (Get-FileHash -LiteralPath $ApplicationPath -Algorithm SHA256).Hash

    $firstRun = Start-And-WaitForMigration 1
    Stop-TestApplication $firstRun.Process
    $firstInspection = Invoke-Probe "inspect" $LedgerPath
    $firstSnapshot = @($firstRun.Snapshots)[0].FullName
    $snapshotInspection = Invoke-Probe "inspect" $firstSnapshot
    if ($firstInspection.version -ne $ExpectedSchemaVersion -or $firstInspection.concur_tables -ne "2" -or
        $firstInspection.batch_name -ne "packaged-migration-sentinel" -or
        $snapshotInspection.version -ne "5" -or $snapshotInspection.integrity -ne "ok" -or
        $snapshotInspection.batch_name -ne "packaged-migration-sentinel") {
        throw "first packaged migration or its pre-migration snapshot is invalid"
    }

    $migratedLedger = Join-Path $DataRoot "ledger-after-first-launch.db"
    Assert-TestPath $migratedLedger
    Move-Item -LiteralPath $LedgerPath -Destination $migratedLedger
    Copy-Item -LiteralPath $firstSnapshot -Destination $LedgerPath
    $rollbackSha256 = (Get-FileHash -LiteralPath $LedgerPath -Algorithm SHA256).Hash
    $snapshotSha256 = (Get-FileHash -LiteralPath $firstSnapshot -Algorithm SHA256).Hash
    if ($rollbackSha256 -ne $snapshotSha256) {
        throw "manual rollback copy does not match the verified snapshot"
    }

    $secondRun = Start-And-WaitForMigration 2
    Stop-TestApplication $secondRun.Process
    $secondInspection = Invoke-Probe "inspect" $LedgerPath
    $secondSnapshots = @(Get-ChildItem -LiteralPath $SnapshotDirectory -File -Filter '*.db')
    if ($secondInspection.version -ne $ExpectedSchemaVersion -or $secondInspection.integrity -ne "ok" -or
        $secondInspection.batch_name -ne "packaged-migration-sentinel" -or
        $secondSnapshots.Count -ne 2) {
        throw "packaged application did not migrate the manually restored v5 snapshot"
    }

    $applicationSha256After = (Get-FileHash -LiteralPath $ApplicationPath -Algorithm SHA256).Hash
    if ($applicationSha256After -ne $applicationSha256Before) {
        throw "packaged executable changed during migration validation"
    }
    $evidence = [ordered]@{
        verifiedAtUtc = [DateTime]::UtcNow.ToString("o")
        applicationPath = $ApplicationPath
        applicationSha256 = $applicationSha256After
        isolatedDataRoot = $true
        initialSchemaVersion = [int]$legacy.version
        migratedSchemaVersion = [int]$firstInspection.version
        preMigrationSnapshotVersion = [int]$snapshotInspection.version
        snapshotIntegrity = $snapshotInspection.integrity
        rollbackCopyMatchesSnapshot = ($rollbackSha256 -eq $snapshotSha256)
        remigratedSchemaVersion = [int]$secondInspection.version
        remigratedIntegrity = $secondInspection.integrity
        sentinelPreserved = ($secondInspection.batch_name -eq "packaged-migration-sentinel")
        snapshotCountAfterRemigration = $secondSnapshots.Count
        legacySha256 = $legacySha256
        firstSnapshotSha256 = $snapshotSha256
        programFileUnchanged = ($applicationSha256After -eq $applicationSha256Before)
    }
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($EvidencePath)) | Out-Null
    [IO.File]::WriteAllText(
        $EvidencePath,
        ($evidence | ConvertTo-Json -Depth 5) + [Environment]::NewLine,
        [Text.UTF8Encoding]::new($false)
    )
    $evidence | ConvertTo-Json -Depth 5
}
finally {
    Pop-Location
    if (-not $KeepTestData -and (Test-Path -LiteralPath $TestRoot)) {
        Assert-TestPath (Join-Path $TestRoot "cleanup-sentinel")
        Remove-Item -LiteralPath $TestRoot -Recurse -Force
    }
}
