[CmdletBinding()]
param(
    [ValidateRange(2, 20)]
    [int]$ImageIterations = 5,
    [ValidateRange(2, 20)]
    [int]$PdfIterations = 3,
    [string]$OutputPath
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $projectRoot "artifacts\ocr-performance-2026-08-20.json"
}
$OutputPath = [IO.Path]::GetFullPath($OutputPath)
$outputParent = Split-Path -Parent $OutputPath
New-Item -ItemType Directory -Path $outputParent -Force | Out-Null

$cargoExe = Join-Path ([Environment]::GetFolderPath("UserProfile")) ".cargo\bin\cargo.exe"
& $cargoExe build --release --locked -p invoice-parse --example ocr_benchmark
if ($LASTEXITCODE -ne 0) { throw "OCR benchmark build failed with exit code $LASTEXITCODE" }
& $cargoExe build --release --locked -p invoice-parse --bin invoice-ocr-worker
if ($LASTEXITCODE -ne 0) { throw "OCR worker build failed with exit code $LASTEXITCODE" }

$benchmarkExe = Join-Path $projectRoot "target\release\examples\ocr_benchmark.exe"
$workerExe = Join-Path $projectRoot "target\release\invoice-ocr-worker.exe"
if (-not (Test-Path -LiteralPath $benchmarkExe -PathType Leaf)) {
    throw "OCR benchmark executable was not generated"
}

function Invoke-OcrBenchmark {
    param(
        [Parameter(Mandatory = $true)][ValidateSet("image", "pdf")][string]$Mode,
        [Parameter(Mandatory = $true)][int]$Iterations
    )
    $runId = [Guid]::NewGuid().ToString("N")
    $stdout = Join-Path ([IO.Path]::GetTempPath()) "invoice-ocr-benchmark-$runId.json"
    $stderr = Join-Path ([IO.Path]::GetTempPath()) "invoice-ocr-benchmark-$runId.err"
    $process = $null
    try {
        $process = Start-Process -FilePath $benchmarkExe `
            -ArgumentList @("--mode", $Mode, "--iterations", $Iterations.ToString()) `
            -RedirectStandardOutput $stdout `
            -RedirectStandardError $stderr `
            -WindowStyle Hidden `
            -PassThru
        $peakWorkingSet = 0L
        while (-not $process.HasExited) {
            $process.Refresh()
            $peakWorkingSet = [Math]::Max($peakWorkingSet, [long]$process.PeakWorkingSet64)
            Start-Sleep -Milliseconds 50
        }
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) {
            $message = if (Test-Path -LiteralPath $stderr) { Get-Content -LiteralPath $stderr -Raw } else { "" }
            throw "OCR benchmark $Mode failed with exit code $($process.ExitCode): $message"
        }
        $payload = Get-Content -LiteralPath $stdout -Raw | ConvertFrom-Json
        $payload | Add-Member -NotePropertyName peakWorkingSetBytes -NotePropertyValue $peakWorkingSet
        return $payload
    }
    finally {
        if ($null -ne $process -and -not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
        Remove-Item -LiteralPath $stdout, $stderr -Force -ErrorAction SilentlyContinue
    }
}

function Measure-ProductionWorker {
    param(
        [Parameter(Mandatory = $true)][string]$Mode,
        [Parameter(Mandatory = $true)][string]$InputPath,
        [Parameter(Mandatory = $true)][int]$Iterations
    )
    $rows = @(
        1..$Iterations | ForEach-Object {
            $run = & (Join-Path $PSScriptRoot "verify-ocr-worker.ps1") -WorkerPath $workerExe -AssetDir (Join-Path $projectRoot "src-tauri\assets\ocr") -InputPath $InputPath
            @($run.results)[0]
        }
    )
    $durations = @($rows | ForEach-Object { [long]$_.elapsedMs })
    $sorted = @($durations | Sort-Object)
    $mean = ($durations | Measure-Object -Average).Average
    $p50Index = [Math]::Max(0, [Math]::Ceiling($sorted.Count * 0.50) - 1)
    $p95Index = [Math]::Max(0, [Math]::Ceiling($sorted.Count * 0.95) - 1)
    [ordered]@{
        mode = $Mode
        processIsolation = "one worker process per file"
        iterations = $Iterations
        allMs = $durations
        meanMs = [double]$mean
        p50Ms = $sorted[$p50Index]
        p95Ms = $sorted[$p95Index]
        maxMs = ($durations | Measure-Object -Maximum).Maximum
        maxPeakWorkingSetBytes = ($rows | Measure-Object -Property peakWorkingSetBytes -Maximum).Maximum
        estimated50Seconds = [double]$mean * 50.0 / 1000.0
        target50Seconds = 300
        estimated50WithinTarget = [double]$mean * 50.0 -le 300000.0
    }
}

$imageResult = Invoke-OcrBenchmark -Mode image -Iterations $ImageIterations
$pdfResult = Invoke-OcrBenchmark -Mode pdf -Iterations $PdfIterations
$imageFixture = Join-Path $projectRoot "fixtures\synthetic\ocr-vat-invoice.png"
$pdfFixture = Join-Path $projectRoot "fixtures\synthetic\ocr-vat-invoice-scanned.pdf"
$productionImage = Measure-ProductionWorker -Mode "image" -InputPath $imageFixture -Iterations $ImageIterations
$productionPdf = Measure-ProductionWorker -Mode "pdf" -InputPath $pdfFixture -Iterations $PdfIterations
$computer = Get-CimInstance Win32_ComputerSystem
$processor = Get-CimInstance Win32_Processor | Select-Object -First 1
$os = Get-CimInstance Win32_OperatingSystem
$report = [ordered]@{
    schemaVersion = 1
    generatedAtUtc = [DateTime]::UtcNow.ToString("o")
    candidate = "InvoiceAssistant-0.1.0-windows-x64-portable-UNSIGNED-INTERNAL-ALPHA"
    environment = [ordered]@{
        osCaption = $os.Caption
        osVersion = $os.Version
        cpu = $processor.Name.Trim()
        logicalProcessors = [int]$computer.NumberOfLogicalProcessors
        physicalMemoryBytes = [long]$computer.TotalPhysicalMemory
    }
    fixtures = [ordered]@{
        imageSha256 = (Get-FileHash -LiteralPath $imageFixture -Algorithm SHA256).Hash
        scannedPdfSha256 = (Get-FileHash -LiteralPath $pdfFixture -Algorithm SHA256).Hash
    }
    sourceHashes = [ordered]@{
        ocrRs = (Get-FileHash -LiteralPath (Join-Path $projectRoot "crates\invoice-parse\src\ocr.rs") -Algorithm SHA256).Hash
        pdfOcrRs = (Get-FileHash -LiteralPath (Join-Path $projectRoot "crates\invoice-parse\src\pdf_ocr.rs") -Algorithm SHA256).Hash
        benchmarkRs = (Get-FileHash -LiteralPath (Join-Path $projectRoot "crates\invoice-parse\examples\ocr_benchmark.rs") -Algorithm SHA256).Hash
        workerProtocolRs = (Get-FileHash -LiteralPath (Join-Path $projectRoot "crates\invoice-parse\src\ocr_worker_protocol.rs") -Algorithm SHA256).Hash
        workerBinRs = (Get-FileHash -LiteralPath (Join-Path $projectRoot "crates\invoice-parse\src\bin\invoice-ocr-worker.rs") -Algorithm SHA256).Hash
        appWorkerRs = (Get-FileHash -LiteralPath (Join-Path $projectRoot "src-tauri\src\ocr_worker.rs") -Algorithm SHA256).Hash
        verifyWorkerPs1 = (Get-FileHash -LiteralPath (Join-Path $projectRoot "scripts\verify-ocr-worker.ps1") -Algorithm SHA256).Hash
        cargoLock = (Get-FileHash -LiteralPath (Join-Path $projectRoot "Cargo.lock") -Algorithm SHA256).Hash
    }
    image = $imageResult
    scannedPdf = $pdfResult
    productionWorker = [ordered]@{
        timeoutSeconds = 45
        maxConcurrency = 1
        image = $productionImage
        scannedPdf = $productionPdf
    }
    interpretation = [ordered]@{
        scope = "synthetic single-invoice cold/warm baseline"
        releaseGateClosed = $false
        reason = "Real/private samples, 50-file mixed end-to-end batches, and minimum-spec physical machines remain required."
    }
}
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath -Encoding utf8
Get-Content -LiteralPath $OutputPath -Raw
