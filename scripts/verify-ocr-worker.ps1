[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$WorkerPath,
    [Parameter(Mandatory = $true)][string]$AssetDir,
    [string[]]$InputPath,
    [ValidateRange(1, 300)][int]$TimeoutSeconds = 45
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$worker = [IO.Path]::GetFullPath($WorkerPath)
$assets = [IO.Path]::GetFullPath($AssetDir)
$workerItem = Get-Item -LiteralPath $worker -Force
$assetItem = Get-Item -LiteralPath $assets -Force
if (-not $workerItem.PSIsContainer -and ($workerItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) {
    # expected shape
}
else {
    throw "OCR worker is missing, not a file, or is a reparse point"
}
if (-not $assetItem.PSIsContainer -or ($assetItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "OCR asset directory is missing or unsafe"
}

$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([IO.Path]::DirectorySeparatorChar)
$probeRoot = Join-Path $tempBase ("invoice-dll-search-probe-" + [Guid]::NewGuid().ToString("N"))
$probePrefix = $tempBase + [IO.Path]::DirectorySeparatorChar
if (-not $probeRoot.StartsWith($probePrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Unsafe DLL search probe path"
}
New-Item -ItemType Directory -Path $probeRoot | Out-Null
[IO.File]::WriteAllBytes((Join-Path $probeRoot "onnxruntime.dll"), [byte[]](0x4D, 0x5A, 0x00, 0x00))
[IO.File]::WriteAllBytes((Join-Path $probeRoot "onnxruntime_providers_shared.dll"), [byte[]](0x4D, 0x5A, 0x00, 0x00))

function Invoke-WorkerGolden {
    param([Parameter(Mandatory = $true)][string]$InputPath)
    $input = [IO.Path]::GetFullPath($InputPath)
    $psi = [Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $worker
    $psi.WorkingDirectory = $probeRoot
    $psi.UseShellExecute = $false
    $psi.Environment["PATH"] = $probeRoot
    $psi.Environment["ORT_DYLIB_PATH"] = Join-Path $probeRoot "onnxruntime.dll"
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $utf8NoBom = [Text.UTF8Encoding]::new($false)
    $process = [Diagnostics.Process]::Start($psi)
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    try {
        $request = [ordered]@{
            protocolVersion = 2
            inputPath = $input
            assetDir = $assets
            ticketType = "Other"
        } | ConvertTo-Json -Compress
        $requestBytes = $utf8NoBom.GetBytes($request)
        $process.StandardInput.BaseStream.Write($requestBytes, 0, $requestBytes.Length)
        $process.StandardInput.BaseStream.Flush()
        $process.StandardInput.BaseStream.Close()
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        $peakWorkingSet = 0L
        while (-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) {
            $process.Refresh()
            $peakWorkingSet = [Math]::Max($peakWorkingSet, [long]$process.PeakWorkingSet64)
            Start-Sleep -Milliseconds 50
        }
        if (-not $process.HasExited) {
            $process.Kill()
            $process.WaitForExit()
            throw "OCR worker exceeded $TimeoutSeconds seconds and was terminated"
        }
        $process.WaitForExit()
        $stdoutBytes = New-Object IO.MemoryStream
        $stderrBytes = New-Object IO.MemoryStream
        $process.StandardOutput.BaseStream.CopyTo($stdoutBytes)
        $process.StandardError.BaseStream.CopyTo($stderrBytes)
        $stdout = $utf8NoBom.GetString($stdoutBytes.ToArray())
        $stderr = $utf8NoBom.GetString($stderrBytes.ToArray())
        $stdoutBytes.Dispose()
        $stderrBytes.Dispose()
        if ($process.ExitCode -ne 0) {
            throw "OCR worker exited with code $($process.ExitCode): $stderr"
        }
        if ([Text.Encoding]::UTF8.GetByteCount($stdout) -gt 1MB) {
            throw "OCR worker output exceeded 1 MiB"
        }
        $response = $stdout | ConvertFrom-Json
        if ($response.status -ne "success") {
            throw "OCR worker returned failure: $($response.message)"
        }
        $invoice = $response.invoice
        # Keep the script itself ASCII so Windows PowerShell 5.1 does not
        # decode UTF-8-without-BOM source literals through the legacy code page.
        $expectedBuyer = -join @(0x5317, 0x4EAC, 0x793A, 0x4F8B, 0x79D1, 0x6280, 0x6709, 0x9650, 0x516C, 0x53F8 | ForEach-Object { [char]$_ })
        $expectedSeller = -join @(0x4E0A, 0x6D77, 0x6F14, 0x793A, 0x5546, 0x8D38, 0x6709, 0x9650, 0x516C, 0x53F8 | ForEach-Object { [char]$_ })
        if ($invoice.invoice_number -ne "26112000000000000001" `
            -or $invoice.issue_date -ne "2026-06-18" `
            -or [string]$invoice.total_amount -ne "1200.00" `
            -or [string]$invoice.tax_amount -ne "67.92" `
            -or $invoice.buyer_name -ne $expectedBuyer `
            -or $invoice.seller_name -ne $expectedSeller `
            -or $invoice.parse_level -ne "L2" `
            -or [double]$invoice.confidence -lt 0.85) {
            throw "OCR worker output did not match the synthetic golden"
        }
        [pscustomobject]@{
            file = [IO.Path]::GetFileName($input)
            sha256 = (Get-FileHash -LiteralPath $input -Algorithm SHA256).Hash
            elapsedMs = [long]$stopwatch.ElapsedMilliseconds
            peakWorkingSetBytes = $peakWorkingSet
            confidence = [double]$invoice.confidence
        }
    }
    finally {
        $stopwatch.Stop()
        if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
        $process.Dispose()
    }
}

if ($null -eq $InputPath -or $InputPath.Count -eq 0) {
    $InputPath = @(
        (Join-Path $projectRoot "fixtures\synthetic\ocr-vat-invoice.png"),
        (Join-Path $projectRoot "fixtures\synthetic\ocr-vat-invoice-scanned.pdf")
    )
}
try {
    $results = @($InputPath | ForEach-Object { Invoke-WorkerGolden -InputPath $_ })
    [pscustomobject]@{
        protocolVersion = 2
        timeoutSeconds = $TimeoutSeconds
        workerSha256 = (Get-FileHash -LiteralPath $worker -Algorithm SHA256).Hash
        dllSearchProbe = [ordered]@{
            adversarialWorkingDirectory = $true
            adversarialPath = $true
            adversarialOrtDylibPath = $true
            decoyRuntimeIgnored = $true
        }
        results = $results
    }
}
finally {
    if (Test-Path -LiteralPath $probeRoot) {
        $resolvedProbe = [IO.Path]::GetFullPath($probeRoot)
        if (-not $resolvedProbe.StartsWith($probePrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to remove unsafe DLL search probe path"
        }
        Remove-Item -LiteralPath $resolvedProbe -Recurse -Force
    }
}
