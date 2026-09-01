[CmdletBinding()]
param(
    [ValidateRange(5, 60)]
    [int]$DurationSeconds = 15,

    [ValidateRange(25, 1000)]
    [int]$SampleIntervalMs = 100,

    [string]$EvidencePath = "artifacts/packaged-idle-memory-v2.validation.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$artifactsRoot = Join-Path $repoRoot "artifacts"
$packageRoot = Join-Path $artifactsRoot "InvoiceAssistant-0.1.0-windows-x64-portable-UNSIGNED-INTERNAL-ALPHA"
$application = Join-Path $packageRoot "InvoiceAssistant.exe"
$workspaceRoot = [IO.Path]::GetFullPath((Split-Path -Parent $repoRoot))
$privateRoot = Join-Path $workspaceRoot ".invoice-tools-private-validation"
$runId = [Guid]::NewGuid().ToString("N")
$dataRoot = Join-Path $privateRoot "packaged-idle-memory-$runId"
$resolvedEvidence = if ([IO.Path]::IsPathRooted($EvidencePath)) {
    [IO.Path]::GetFullPath($EvidencePath)
} else {
    [IO.Path]::GetFullPath((Join-Path $repoRoot $EvidencePath))
}

if (-not (Test-Path -LiteralPath $application -PathType Leaf)) {
    throw "Current portable application is missing"
}
$artifactPrefix = [IO.Path]::GetFullPath($artifactsRoot).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if (-not $resolvedEvidence.StartsWith($artifactPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Evidence must be written under artifacts"
}
if (Test-Path -LiteralPath $resolvedEvidence) {
    throw "Refusing to overwrite existing evidence"
}
New-Item -ItemType Directory -Path $dataRoot -Force | Out-Null

function Get-ProcessTree {
    param([Parameter(Mandatory = $true)][int]$RootProcessId)

    # Windows PowerShell 5.1 的 System.Diagnostics.Process 没有稳定的 Parent
    # 属性；使用 Win32_Process 的 ParentProcessId 构造树，再读取实时内存。
    $processRelations = @()
    $processRelations = @(Get-CimInstance -ClassName Win32_Process -ErrorAction Stop)
    $ids = [Collections.Generic.HashSet[int]]::new()
    [void]$ids.Add($RootProcessId)
    do {
        $changed = $false
        foreach ($candidate in $processRelations) {
            $candidateId = [int]$candidate.ProcessId
            if ($ids.Contains($candidateId)) { continue }
            if ($ids.Contains([int]$candidate.ParentProcessId)) {
                [void]$ids.Add($candidateId)
                $changed = $true
            }
        }
    } while ($changed)
    return @(Get-Process -Id @($ids) -ErrorAction SilentlyContinue)
}

$savedDataRoot = [Environment]::GetEnvironmentVariable("INVOICE_ASSISTANT_HOME", "Process")
$process = $null
$tree = @()
$childIds = @()
$samples = 0
$peakProcessCount = 0
$peakAggregateWorkingSet = 0L
$peakAggregatePrivateBytes = 0L
$peakApplicationWorkingSet = 0L
$peakWebViewWorkingSet = 0L
$peakWebViewProcessCount = 0
$programFilesBefore = @(
    Get-ChildItem -LiteralPath $packageRoot -Recurse -File |
        ForEach-Object { $_.FullName.Substring($packageRoot.Length).TrimStart('\') }
)
$startedAt = [DateTime]::UtcNow
try {
    $env:INVOICE_ASSISTANT_HOME = $dataRoot
    $process = Start-Process -FilePath $application -WorkingDirectory $packageRoot -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds($DurationSeconds)
    while ([DateTime]::UtcNow -lt $deadline -and -not $process.HasExited) {
        $tree = @(Get-ProcessTree -RootProcessId $process.Id)
        $aggregateWorkingSet = 0L
        $aggregatePrivateBytes = 0L
        $webViewProcessCount = 0
        foreach ($treeProcess in $tree) {
            try {
                $treeProcess.Refresh()
                $workingSet = [long]$treeProcess.WorkingSet64
                $aggregateWorkingSet += $workingSet
                $aggregatePrivateBytes += [long]$treeProcess.PrivateMemorySize64
                if ($treeProcess.Id -eq $process.Id) {
                    $peakApplicationWorkingSet = [Math]::Max($peakApplicationWorkingSet, $workingSet)
                }
                if ($treeProcess.ProcessName -like "msedgewebview2*") {
                    $webViewProcessCount += 1
                    $peakWebViewWorkingSet = [Math]::Max($peakWebViewWorkingSet, $workingSet)
                }
            }
            catch {
                # Process exited between snapshot and refresh.
            }
        }
        $samples += 1
        $peakProcessCount = [Math]::Max($peakProcessCount, $tree.Count)
        $peakWebViewProcessCount = [Math]::Max($peakWebViewProcessCount, $webViewProcessCount)
        $peakAggregateWorkingSet = [Math]::Max($peakAggregateWorkingSet, $aggregateWorkingSet)
        $peakAggregatePrivateBytes = [Math]::Max($peakAggregatePrivateBytes, $aggregatePrivateBytes)
        Start-Sleep -Milliseconds $SampleIntervalMs
        $process.Refresh()
    }
}
finally {
    if ($null -ne $process -and -not $process.HasExited) {
        # Windows PowerShell 5.1 运行在 .NET Framework 上，没有 Process.Kill(bool)
        # 重载。先关闭本次应用树的 WebView2 子进程，再关闭主进程，避免遗留后台进程。
        # 使用最后一次成功采样的树清理子进程；即使枚举失败也会继续关闭主进程。
        $childIds = @($tree | Where-Object { $_.Id -ne $process.Id } | ForEach-Object { $_.Id })
        if ($childIds.Count -gt 0) {
            Stop-Process -Id $childIds -Force -ErrorAction SilentlyContinue
        }
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        $process.WaitForExit()
    }
    if ($null -eq $savedDataRoot) {
        Remove-Item Env:INVOICE_ASSISTANT_HOME -ErrorAction SilentlyContinue
    } else {
        [Environment]::SetEnvironmentVariable("INVOICE_ASSISTANT_HOME", $savedDataRoot, "Process")
    }
}

if ($null -eq $process) {
    throw "Packaged application was not started"
}
$programFilesAfter = @(
    Get-ChildItem -LiteralPath $packageRoot -Recurse -File |
        ForEach-Object { $_.FullName.Substring($packageRoot.Length).TrimStart('\') }
)
$dataFiles = @(
    Get-ChildItem -LiteralPath $dataRoot -Recurse -File -ErrorAction SilentlyContinue |
        ForEach-Object { $_.FullName.Substring($dataRoot.Length).TrimStart('\') }
)
$logFiles = @(Get-ChildItem -LiteralPath (Join-Path $dataRoot "logs") -File -ErrorAction SilentlyContinue)
$privateTokens = 0
$logErrorMatches = 0
foreach ($logFile in $logFiles) {
    $log = [string](Get-Content -LiteralPath $logFile.FullName -Raw)
    $privateTokens += [regex]::Matches($log, '(?i)\b\d{6,}@qq\.com\b|(?<!\d)\d{18,24}(?!\d)').Count
    $logErrorMatches += [regex]::Matches($log, '(?im)\bERROR\b|failed to create webview').Count
}
$programDirectoryUnchanged = (@($programFilesBefore | Sort-Object) -join "`n") -eq
    (@($programFilesAfter | Sort-Object) -join "`n")

$evidence = [ordered]@{
    schemaVersion = 2
    verification = "packaged-idle-memory-v2"
    verifiedAtUtc = [DateTime]::UtcNow.ToString("o")
    candidate = [ordered]@{
        applicationSha256 = (Get-FileHash -LiteralPath $application -Algorithm SHA256).Hash
        packageSha256 = (Get-FileHash -LiteralPath (Join-Path $artifactsRoot "InvoiceAssistant-0.1.0-windows-x64-portable-UNSIGNED-INTERNAL-ALPHA.zip") -Algorithm SHA256).Hash
        signed = $false
    }
    scope = [ordered]@{
        actualPackagedExecutable = $true
        firstRunIdleShell = $true
        mailboxAccessed = $false
        secretLoaded = $false
        invoiceFilesProcessed = 0
        limitation = "idle UI shell only; not a batch-processing or minimum-configuration result"
    }
    sampling = [ordered]@{
        requestedDurationSeconds = $DurationSeconds
        intervalMs = $SampleIntervalMs
        samples = $samples
        maximumConcurrentProcessCount = $peakProcessCount
        maximumConcurrentWebViewProcessCount = $peakWebViewProcessCount
        peakAggregateWorkingSetBytes = $peakAggregateWorkingSet
        peakAggregatePrivateBytes = $peakAggregatePrivateBytes
        peakApplicationWorkingSetBytes = $peakApplicationWorkingSet
        peakSingleWebViewProcessWorkingSetBytes = $peakWebViewWorkingSet
    }
    isolation = [ordered]@{
        programDirectoryUnchanged = $programDirectoryUnchanged
        dataFileCount = $dataFiles.Count
        logPrivatePatternMatches = $privateTokens
        logErrorMatches = $logErrorMatches
        processExitedAfterMeasurement = $process.HasExited
    }
    result = if ($programDirectoryUnchanged -and
        $privateTokens -eq 0 -and
        $logErrorMatches -eq 0 -and
        $peakWebViewProcessCount -ge 3 -and
        $samples -gt 0) {
        "passed_reference_machine_idle_baseline"
    } else {
        "failed"
    }
}
$json = $evidence | ConvertTo-Json -Depth 7
[IO.File]::WriteAllText($resolvedEvidence, $json + "`n", [Text.UTF8Encoding]::new($false))
if ($evidence.result -eq "failed") {
    throw "Packaged idle-memory validation failed"
}

Write-Output "verification=packaged-idle-memory-v2"
Write-Output "peak_aggregate_working_set_bytes=$peakAggregateWorkingSet"
Write-Output "peak_application_working_set_bytes=$peakApplicationWorkingSet"
Write-Output "peak_single_webview_process_working_set_bytes=$peakWebViewWorkingSet"
Write-Output "maximum_concurrent_webview_processes=$peakWebViewProcessCount"
Write-Output "program_directory_unchanged=true"
Write-Output "private_token_matches=0"
Write-Output "evidence=$resolvedEvidence"
