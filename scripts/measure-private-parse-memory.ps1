[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$CaptureRoot,

    [string]$EvidencePath = "artifacts/real-private-105-memory.validation.json",

    [ValidateRange(25, 1000)]
    [int]$SampleIntervalMs = 100
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$artifactsRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot "artifacts"))
$capture = [IO.Path]::GetFullPath($CaptureRoot)
$samplesRoot = Join-Path $capture "fixtures\samples"
$packageRoot = Join-Path $artifactsRoot "InvoiceAssistant-0.1.0-windows-x64-portable-UNSIGNED-INTERNAL-ALPHA"
$application = Join-Path $packageRoot "InvoiceAssistant.exe"
$worker = Join-Path $packageRoot "invoice-ocr-worker.exe"
$cargo = Join-Path ([Environment]::GetFolderPath("UserProfile")) ".cargo\bin\cargo.exe"
$resultName = "parse-results-current-memory-v3.private.json"
$summaryName = "parse-summary-current-memory-v3.private.txt"
$resultPath = Join-Path $capture $resultName
$summaryPath = Join-Path $capture $summaryName
$runId = [Guid]::NewGuid().ToString("N")
$stdoutPath = Join-Path $capture "memory-validation-$runId.private.stdout.txt"
$stderrPath = Join-Path $capture "memory-validation-$runId.private.stderr.txt"
$dataRoot = Join-Path $capture "memory-validation-data"

function Assert-PlainDirectory {
    param([Parameter(Mandatory = $true)][string]$Path)

    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer) {
        throw "Expected a directory"
    }
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Validation directory cannot be a reparse point"
    }
}

function Get-Role {
    param([Parameter(Mandatory = $true)][string]$ProcessName)

    if ($ProcessName -like "invoice-ocr-worker*") { return "ocrWorker" }
    if ($ProcessName -like "invoice_assistant-*" -or $ProcessName -like "invoice-assistant-*") {
        return "applicationTestTarget"
    }
    if ($ProcessName -eq "cargo") { return "cargo" }
    if ($ProcessName -eq "rustc") { return "rustc" }
    return "otherDescendant"
}

function Get-ProcessTree {
    param([Parameter(Mandatory = $true)][int]$RootProcessId)

    $allProcesses = @(Get-Process -ErrorAction SilentlyContinue)
    $ids = [Collections.Generic.HashSet[int]]::new()
    [void]$ids.Add($RootProcessId)
    do {
        $changed = $false
        foreach ($candidate in $allProcesses) {
            if ($ids.Contains($candidate.Id)) { continue }
            try {
                $parent = $candidate.Parent
                if ($null -ne $parent -and $ids.Contains($parent.Id)) {
                    [void]$ids.Add($candidate.Id)
                    $changed = $true
                }
            }
            catch {
                # A short-lived process may exit while the snapshot is inspected.
            }
        }
    } while ($changed)
    return @($allProcesses | Where-Object { $ids.Contains($_.Id) })
}

if (-not (Test-Path -LiteralPath $capture -PathType Container)) {
    throw "Private capture root does not exist"
}
if (-not (Test-Path -LiteralPath $samplesRoot -PathType Container)) {
    throw "Private capture samples directory does not exist"
}
if (-not (Test-Path -LiteralPath $application -PathType Leaf) -or
    -not (Test-Path -LiteralPath $worker -PathType Leaf)) {
    throw "Current portable candidate is incomplete"
}
if (-not (Test-Path -LiteralPath $cargo -PathType Leaf)) {
    throw "Cargo executable was not found"
}

$repoPrefix = $repoRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if ($capture.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Private capture root must remain outside the Git repository"
}
Assert-PlainDirectory -Path $capture
Assert-PlainDirectory -Path $samplesRoot

$sampleFiles = @(Get-ChildItem -LiteralPath $samplesRoot -File)
if ($sampleFiles.Count -ne 105) {
    throw "Expected the frozen 105-file candidate set"
}
if (Test-Path -LiteralPath $resultPath) {
    throw "Refusing to overwrite an existing private result"
}
if (Test-Path -LiteralPath $summaryPath) {
    throw "Refusing to overwrite an existing private summary"
}

$resolvedEvidence = if ([IO.Path]::IsPathRooted($EvidencePath)) {
    [IO.Path]::GetFullPath($EvidencePath)
} else {
    [IO.Path]::GetFullPath((Join-Path $repoRoot $EvidencePath))
}
$artifactPrefix = $artifactsRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if (-not $resolvedEvidence.StartsWith($artifactPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Sanitized evidence must be written under artifacts"
}
if (Test-Path -LiteralPath $resolvedEvidence) {
    throw "Refusing to overwrite existing evidence"
}

& (Join-Path $PSScriptRoot "check-build-storage.ps1") `
    -Mode Preflight `
    -AutoCleanDev `
    -EvidencePath "artifacts/build-storage-private-memory-preflight.validation.json"

# Compile without any private-data environment variables in scope.
& $cargo test -p invoice-assistant --lib --locked --no-run
if ($LASTEXITCODE -ne 0) {
    throw "Private-memory test target build failed"
}

New-Item -ItemType Directory -Path $dataRoot -Force | Out-Null
Assert-PlainDirectory -Path $dataRoot

$savedEnvironment = [ordered]@{
    INVOICE_REAL_CAPTURE_ROOT = [Environment]::GetEnvironmentVariable("INVOICE_REAL_CAPTURE_ROOT", "Process")
    INVOICE_REAL_PARSE_RESULT_FILE = [Environment]::GetEnvironmentVariable("INVOICE_REAL_PARSE_RESULT_FILE", "Process")
    INVOICE_ASSISTANT_OCR_WORKER = [Environment]::GetEnvironmentVariable("INVOICE_ASSISTANT_OCR_WORKER", "Process")
    INVOICE_ASSISTANT_HOME = [Environment]::GetEnvironmentVariable("INVOICE_ASSISTANT_HOME", "Process")
}

$process = $null
$sampleCount = 0
$peakProcessCount = 0
$peakAggregateWorkingSet = 0L
$peakAggregatePrivateBytes = 0L
$rolePeakWorkingSet = @{}
$rolePeakPrivateBytes = @{}
$startedAt = [DateTime]::UtcNow
$stopwatch = [Diagnostics.Stopwatch]::StartNew()
try {
    $env:INVOICE_REAL_CAPTURE_ROOT = $capture
    $env:INVOICE_REAL_PARSE_RESULT_FILE = $resultName
    $env:INVOICE_ASSISTANT_OCR_WORKER = $worker
    $env:INVOICE_ASSISTANT_HOME = $dataRoot

    $process = Start-Process -FilePath $cargo `
        -ArgumentList @(
            "test", "-p", "invoice-assistant", "--lib", "--locked",
            "commands::pipeline::tests::real_private_capture_parses_candidates_without_logging_invoice_fields",
            "--", "--ignored", "--exact", "--nocapture"
        ) `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError $stderrPath `
        -WindowStyle Hidden `
        -PassThru

    while (-not $process.HasExited) {
        $tree = @(Get-ProcessTree -RootProcessId $process.Id)
        $aggregateWorkingSet = 0L
        $aggregatePrivateBytes = 0L
        foreach ($treeProcess in $tree) {
            try {
                $treeProcess.Refresh()
                $workingSet = [long]$treeProcess.WorkingSet64
                $privateBytes = [long]$treeProcess.PrivateMemorySize64
                $aggregateWorkingSet += $workingSet
                $aggregatePrivateBytes += $privateBytes
                $role = Get-Role -ProcessName $treeProcess.ProcessName
                if (-not $rolePeakWorkingSet.ContainsKey($role) -or
                    $workingSet -gt [long]$rolePeakWorkingSet[$role]) {
                    $rolePeakWorkingSet[$role] = $workingSet
                }
                if (-not $rolePeakPrivateBytes.ContainsKey($role) -or
                    $privateBytes -gt [long]$rolePeakPrivateBytes[$role]) {
                    $rolePeakPrivateBytes[$role] = $privateBytes
                }
            }
            catch {
                # The sampled child exited between enumeration and refresh.
            }
        }
        $sampleCount += 1
        $peakProcessCount = [Math]::Max($peakProcessCount, $tree.Count)
        $peakAggregateWorkingSet = [Math]::Max($peakAggregateWorkingSet, $aggregateWorkingSet)
        $peakAggregatePrivateBytes = [Math]::Max($peakAggregatePrivateBytes, $aggregatePrivateBytes)
        Start-Sleep -Milliseconds $SampleIntervalMs
        $process.Refresh()
    }
    $process.WaitForExit()
}
finally {
    $stopwatch.Stop()
    foreach ($entry in $savedEnvironment.GetEnumerator()) {
        if ($null -eq $entry.Value) {
            Remove-Item "Env:$($entry.Key)" -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($entry.Key, [string]$entry.Value, "Process")
        }
    }
}

if ($null -eq $process -or $process.ExitCode -ne 0) {
    throw "Private-memory validation failed; diagnostics remain only in the private capture root"
}
if (-not (Test-Path -LiteralPath $resultPath -PathType Leaf) -or
    -not (Test-Path -LiteralPath $summaryPath -PathType Leaf)) {
    throw "Private-memory validation did not produce the expected private outputs"
}

$stdout = Get-Content -LiteralPath $stdoutPath -Raw
$stderr = Get-Content -LiteralPath $stderrPath -Raw
$combinedLog = $stdout + "`n" + $stderr
$summary = Get-Content -LiteralPath $summaryPath -Raw
$expectedSummaryLines = @(
    "candidate_files=105",
    "parse_success=84",
    "parse_failed=21",
    "private_fields_logged=false"
)
foreach ($expectedLine in $expectedSummaryLines) {
    if (-not (($summary -split "`r?`n") -contains $expectedLine)) {
        throw "Private-memory validation summary did not match the frozen baseline"
    }
}

$privateRootMatches = if ($combinedLog.Contains($capture, [StringComparison]::OrdinalIgnoreCase)) { 1 } else { 0 }
$privateFilenameMatches = @(
    $sampleFiles | Where-Object {
        $combinedLog.Contains($_.Name, [StringComparison]::OrdinalIgnoreCase)
    }
).Count
$fullQqEmailMatches = [regex]::Matches($combinedLog, '(?i)\b\d{6,}@qq\.com\b').Count
$longNumericTokenMatches = [regex]::Matches($combinedLog, '(?<!\d)\d{18,24}(?!\d)').Count
$privacyPassed = $privateRootMatches -eq 0 -and
    $privateFilenameMatches -eq 0 -and
    $fullQqEmailMatches -eq 0 -and
    $longNumericTokenMatches -eq 0

$computer = $null
$processor = $null
$os = $null
try {
    $computer = Get-CimInstance Win32_ComputerSystem -ErrorAction Stop
    $processor = Get-CimInstance Win32_Processor -ErrorAction Stop | Select-Object -First 1
    $os = Get-CimInstance Win32_OperatingSystem -ErrorAction Stop
}
catch {
    # Enterprise endpoint policy may deny WMI/CIM. Memory sampling does not depend on it.
}
$physicalMemory = if ($null -ne $computer) { [long]$computer.TotalPhysicalMemory } else { $null }
$peakPercent = if ($null -ne $physicalMemory -and $physicalMemory -gt 0) {
    [Math]::Round(100.0 * $peakAggregateWorkingSet / $physicalMemory, 2)
} else {
    $null
}

$evidence = [ordered]@{
    schemaVersion = 1
    verification = "authorized-real-private-105-memory-v1"
    verifiedAtUtc = [DateTime]::UtcNow.ToString("o")
    scope = [ordered]@{
        source = "authorized frozen 105-file private candidate set outside Git"
        mailboxAccessed = $false
        networkRequests = 0
        secretLoaded = $false
        execution = "current source production parser through the invoice-assistant library test target; current portable release OCR worker"
        limitation = "does not replace packaged UI memory, minimum-configuration or second-physical-machine evidence"
    }
    candidate = [ordered]@{
        applicationSha256 = (Get-FileHash -LiteralPath $application -Algorithm SHA256).Hash
        ocrWorkerSha256 = (Get-FileHash -LiteralPath $worker -Algorithm SHA256).Hash
        signed = $false
    }
    workload = [ordered]@{
        files = 105
        structuredResults = 84
        parseFailures = 21
        elapsedMs = [long]$stopwatch.ElapsedMilliseconds
        privateResultSha256 = (Get-FileHash -LiteralPath $resultPath -Algorithm SHA256).Hash
        privateResultBytes = (Get-Item -LiteralPath $resultPath).Length
        privateResultStoredOutsideGit = $true
    }
    sampling = [ordered]@{
        intervalMs = $SampleIntervalMs
        samples = $sampleCount
        maximumConcurrentProcessCount = $peakProcessCount
        peakAggregateWorkingSetBytes = $peakAggregateWorkingSet
        peakAggregatePrivateBytes = $peakAggregatePrivateBytes
        peakWorkingSetByRoleBytes = [ordered]@{
            applicationTestTarget = [long]($rolePeakWorkingSet["applicationTestTarget"] ?? 0)
            ocrWorker = [long]($rolePeakWorkingSet["ocrWorker"] ?? 0)
            cargo = [long]($rolePeakWorkingSet["cargo"] ?? 0)
            rustc = [long]($rolePeakWorkingSet["rustc"] ?? 0)
            otherDescendant = [long]($rolePeakWorkingSet["otherDescendant"] ?? 0)
        }
        peakPrivateBytesByRole = [ordered]@{
            applicationTestTarget = [long]($rolePeakPrivateBytes["applicationTestTarget"] ?? 0)
            ocrWorker = [long]($rolePeakPrivateBytes["ocrWorker"] ?? 0)
            cargo = [long]($rolePeakPrivateBytes["cargo"] ?? 0)
            rustc = [long]($rolePeakPrivateBytes["rustc"] ?? 0)
            otherDescendant = [long]($rolePeakPrivateBytes["otherDescendant"] ?? 0)
        }
    }
    environment = [ordered]@{
        osCaption = if ($null -ne $os) { $os.Caption } else { [Runtime.InteropServices.RuntimeInformation]::OSDescription }
        osVersion = if ($null -ne $os) { $os.Version } else { [Environment]::OSVersion.VersionString }
        cpu = if ($null -ne $processor) { $processor.Name.Trim() } else { [string]$env:PROCESSOR_IDENTIFIER }
        logicalProcessors = if ($null -ne $computer) { [int]$computer.NumberOfLogicalProcessors } else { [Environment]::ProcessorCount }
        physicalMemoryBytes = $physicalMemory
        peakAggregateWorkingSetPercentOfPhysicalMemory = $peakPercent
    }
    privacyLogScan = [ordered]@{
        privateRootMatches = $privateRootMatches
        privateFilenameMatches = $privateFilenameMatches
        fullQqEmailMatches = $fullQqEmailMatches
        longNumericMatches = $longNumericTokenMatches
        privateFieldsLogged = $false
        passed = $privacyPassed
        stdoutSha256 = (Get-FileHash -LiteralPath $stdoutPath -Algorithm SHA256).Hash
        stderrSha256 = (Get-FileHash -LiteralPath $stderrPath -Algorithm SHA256).Hash
    }
    result = if ($privacyPassed) { "passed_on_reference_machine" } else { "failed_privacy_log_scan" }
}

$json = $evidence | ConvertTo-Json -Depth 8
[IO.File]::WriteAllText($resolvedEvidence, $json + "`n", [Text.UTF8Encoding]::new($false))

& (Join-Path $PSScriptRoot "check-build-storage.ps1") `
    -Mode Postflight `
    -AutoCleanDev `
    -EvidencePath "artifacts/build-storage-private-memory-postflight.validation.json"

if (-not $privacyPassed) {
    throw "Private-memory validation log scan failed; only aggregate counts were written to evidence"
}

Write-Output "verification=authorized-real-private-105-memory-v1"
Write-Output "files=105"
Write-Output "parse_success=84"
Write-Output "parse_failed=21"
Write-Output "peak_aggregate_working_set_bytes=$peakAggregateWorkingSet"
Write-Output "peak_ocr_worker_working_set_bytes=$([long]($rolePeakWorkingSet['ocrWorker'] ?? 0))"
Write-Output "privacy_log_scan_passed=true"
Write-Output "evidence=$resolvedEvidence"
