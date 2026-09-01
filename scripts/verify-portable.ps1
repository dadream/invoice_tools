[CmdletBinding()]
param(
    [string]$ZipPath,
    [ValidateSet("NotSigned", "Valid")]
    [string]$ExpectedSignatureStatus = "NotSigned",
    [switch]$SkipLaunch,
    [string]$EvidencePath
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot

function Get-ContainedRelativePath {
    param(
        [Parameter(Mandatory = $true)][string]$BasePath,
        [Parameter(Mandatory = $true)][string]$ChildPath
    )

    $base = [IO.Path]::GetFullPath($BasePath).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $child = [IO.Path]::GetFullPath($ChildPath)
    if (-not $child.StartsWith($base, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Extracted file escapes verification directory: $child"
    }
    return $child.Substring($base.Length)
}

if ([string]::IsNullOrWhiteSpace($ZipPath)) {
    $config = Get-Content -LiteralPath (Join-Path $projectRoot "src-tauri\tauri.conf.json") -Raw -Encoding UTF8 |
        ConvertFrom-Json
    $base = "InvoiceAssistant-$($config.version)-windows-x64-portable-UNSIGNED-INTERNAL-ALPHA"
    $ZipPath = Join-Path $projectRoot "artifacts\$base.zip"
}
$zip = [System.IO.Path]::GetFullPath($ZipPath)
$sidecar = "$zip.sha256"
if (-not (Test-Path -LiteralPath $zip -PathType Leaf)) {
    throw "Portable ZIP does not exist: $zip"
}
if (-not (Test-Path -LiteralPath $sidecar -PathType Leaf)) {
    throw "Portable ZIP sidecar does not exist: $sidecar"
}

$expectedZipHash = ((Get-Content -LiteralPath $sidecar -Raw).Trim() -split "\s+")[0]
$actualZipHash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash
if ($expectedZipHash -ne $actualZipHash) {
    throw "Portable ZIP SHA-256 does not match its sidecar"
}

$tempBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd(
    [System.IO.Path]::DirectorySeparatorChar
)
$runId = [Guid]::NewGuid().ToString("N")
$invoiceAssistantText = -join @(0x53D1, 0x7968, 0x52A9, 0x624B | ForEach-Object { [char]$_ })
$verificationText = -join @(0x9A8C, 0x8BC1 | ForEach-Object { [char]$_ })
$expectedWindowTitle = -join @(0x53D1, 0x7968, 0x62A5, 0x9500, 0x52A9, 0x624B | ForEach-Object { [char]$_ })
$extractRoot = Join-Path $tempBase "$invoiceAssistantText $verificationText $runId"
$dataRoot = Join-Path $tempBase "InvoiceAssistant-Data-$runId"
$process = $null

function Assert-TemporaryChild {
    param([Parameter(Mandatory = $true)][string]$Path)
    $full = [System.IO.Path]::GetFullPath($Path)
    $prefix = $tempBase + [System.IO.Path]::DirectorySeparatorChar
    if (-not $full.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Unsafe temporary path: $full"
    }
}

function Resolve-PortableRelativePath {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$RelativePath
    )
    if ([string]::IsNullOrWhiteSpace($RelativePath) -or [IO.Path]::IsPathRooted($RelativePath)) {
        throw "Portable package contains an unsafe path: $RelativePath"
    }
    $rootFull = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar)
    $full = [IO.Path]::GetFullPath((Join-Path $rootFull $RelativePath.Replace('/', '\')))
    if (-not $full.StartsWith($rootFull + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Portable package path escapes extraction root: $RelativePath"
    }
    return $full
}
Assert-TemporaryChild -Path $extractRoot
Assert-TemporaryChild -Path $dataRoot
New-Item -ItemType Directory -Path $extractRoot, $dataRoot | Out-Null

try {
    Expand-Archive -LiteralPath $zip -DestinationPath $extractRoot
    $files = @(Get-ChildItem -LiteralPath $extractRoot -Recurse -File)
    $manifestPath = Join-Path $extractRoot "manifest.json"
    $versionPath = Join-Path $extractRoot "version.json"
    $checksumsPath = Join-Path $extractRoot "SHA256SUMS.txt"
    $exe = Join-Path $extractRoot "InvoiceAssistant.exe"
    $workerExe = Join-Path $extractRoot "invoice-ocr-worker.exe"
    foreach ($required in @($manifestPath, $versionPath, $checksumsPath, $exe, $workerExe)) {
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
            throw "Portable package is missing required file: $required"
        }
    }

    $dllHardening = @(& (Join-Path $PSScriptRoot "test-dll-search-hardening.ps1") -ExecutablePath @($exe, $workerExe))

    foreach ($relative in @(
        "ocr/onnxruntime.dll",
        "ocr/onnxruntime_providers_shared.dll",
        "ocr/models/ch_PP-OCRv5_det_mobile.onnx",
        "ocr/models/ch_ppocr_mobile_v2.0_cls_mobile.onnx",
        "ocr/models/ch_PP-OCRv5_rec_mobile.onnx"
    )) {
        $required = Resolve-PortableRelativePath -Root $extractRoot -RelativePath $relative
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
            throw "Portable package is missing OCR file: $relative"
        }
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($manifest.product -ne $expectedWindowTitle -or $manifest.platform -ne "windows-x64-portable") {
        throw "Portable manifest identity is invalid"
    }
    $versionMetadata = Get-Content -LiteralPath $versionPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($versionMetadata.schemaVersion -ne 1 -or
        $versionMetadata.product -ne "com.dadream.invoiceassistant" -or
        $versionMetadata.version -ne $manifest.version -or
        $versionMetadata.channel -ne $manifest.channel -or
        [bool]$versionMetadata.signed -ne [bool]$manifest.signed -or
        -not [bool]$versionMetadata.manualUpdateCheckOnly -or
        [bool]$versionMetadata.automaticDownload -or
        [bool]$versionMetadata.concurSendEnabled -or
        -not [bool]$versionMetadata.concurSendManualConfirmationRequired -or
        [bool]$versionMetadata.concurSendRealTestApproved -or
        -not [bool]$versionMetadata.dllSearchHardened -or
        $versionMetadata.dependentLoadFlags -ne "0x0800" -or
        $versionMetadata.ocrRuntimeDirectoryPolicy -ne "VerifiedAddDllDirectory") {
        throw "Portable version metadata is invalid or inconsistent with manifest.json"
    }
    if ([bool]$versionMetadata.updateManifestConfigured) {
        try {
            $updateManifestUri = [Uri][string]$versionMetadata.updateManifestUrl
        }
        catch {
            throw "Portable update manifest URL is invalid"
        }
        if (-not $updateManifestUri.IsAbsoluteUri -or
            $updateManifestUri.Scheme -ne "https" -or
            [string]::IsNullOrWhiteSpace($updateManifestUri.Host) -or
            -not [string]::IsNullOrEmpty($updateManifestUri.UserInfo) -or
            -not [string]::IsNullOrEmpty($updateManifestUri.Fragment)) {
            throw "Portable update manifest URL violates the HTTPS policy"
        }
    }
    elseif (-not [string]::IsNullOrWhiteSpace([string]$versionMetadata.updateManifestUrl)) {
        throw "Unconfigured portable metadata must not contain an update manifest URL"
    }
    $manifestPaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in @($manifest.files)) {
        if (-not $manifestPaths.Add([string]$entry.path)) {
            throw "Portable manifest contains a duplicate path: $($entry.path)"
        }
        $path = Resolve-PortableRelativePath -Root $extractRoot -RelativePath $entry.path
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Portable manifest file is missing: $($entry.path)"
        }
        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $entry.sha256) {
            throw "Portable manifest hash mismatch: $($entry.path)"
        }
    }
    $payloadPaths = @(
        $files |
            ForEach-Object { (Get-ContainedRelativePath -BasePath $extractRoot -ChildPath $_.FullName).Replace('\', '/') } |
            Where-Object { $_ -notin @("manifest.json", "SHA256SUMS.txt") }
    )
    if ($manifestPaths.Count -ne $payloadPaths.Count -or @($payloadPaths | Where-Object { -not $manifestPaths.Contains($_) }).Count -ne 0) {
        throw "Portable manifest does not cover every payload file exactly once"
    }

    $checksumLines = @(
        Get-Content -LiteralPath $checksumsPath -Encoding UTF8 |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    )
    $checksumPaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($line in $checksumLines) {
        $parts = $line -split "\s+", 2
        if ($parts.Count -ne 2 -or -not $checksumPaths.Add($parts[1])) {
            throw "SHA256SUMS contains an invalid entry"
        }
        $path = Resolve-PortableRelativePath -Root $extractRoot -RelativePath $parts[1]
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "SHA256SUMS file is missing: $($parts[1])"
        }
        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $parts[0]) {
            throw "SHA256SUMS mismatch: $($parts[1])"
        }
    }
    $checksumPayloadPaths = @(
        $files |
            ForEach-Object { (Get-ContainedRelativePath -BasePath $extractRoot -ChildPath $_.FullName).Replace('\', '/') } |
            Where-Object { $_ -ne "SHA256SUMS.txt" }
    )
    if ($checksumPaths.Count -ne $checksumPayloadPaths.Count -or @($checksumPayloadPaths | Where-Object { -not $checksumPaths.Contains($_) }).Count -ne 0) {
        throw "SHA256SUMS does not cover every package file exactly once"
    }

    $forbidden = @(
        Get-ChildItem -LiteralPath $extractRoot -Recurse -File |
            Where-Object {
                $_.Name -match "(?i)(^\.env|\.eml$|\.db$|\.pfx$|\.p12$|\.pem$|\.key$|\.log$)"
            }
    )
    if ($forbidden.Count -ne 0) {
        throw "Forbidden files were found in the portable package"
    }

    $authenticodeStatuses = [ordered]@{}
    foreach ($pe in @($exe, $workerExe)) {
        $signature = Get-AuthenticodeSignature -LiteralPath $pe
        if ([string]$signature.Status -ne $ExpectedSignatureStatus) {
            throw "Unexpected Authenticode status for $([IO.Path]::GetFileName($pe)): $($signature.Status)"
        }
        $authenticodeStatuses[[IO.Path]::GetFileName($pe)] = [string]$signature.Status
    }

    $windowTitle = $null
    $programDirectoryUnchanged = $null
    $dataFiles = @()
    $ocrWorkerEvidence = $null
    if (-not $SkipLaunch) {
        $before = @{}
        Get-ChildItem -LiteralPath $extractRoot -Recurse -File | ForEach-Object {
            $relative = (Get-ContainedRelativePath -BasePath $extractRoot -ChildPath $_.FullName).Replace('\', '/')
            $before[$relative] = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
        }
        $ocrWorkerEvidence = & (Join-Path $PSScriptRoot "verify-ocr-worker.ps1") `
            -WorkerPath $workerExe `
            -AssetDir (Join-Path $extractRoot "ocr")
        $launchInfo = New-Object Diagnostics.ProcessStartInfo
        $launchInfo.FileName = $exe
        $launchInfo.WorkingDirectory = $extractRoot
        $launchInfo.UseShellExecute = $false
        $launchInfo.EnvironmentVariables["INVOICE_ASSISTANT_HOME"] = $dataRoot
        $process = [Diagnostics.Process]::Start($launchInfo)
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        while ([DateTime]::UtcNow -lt $deadline) {
            Start-Sleep -Milliseconds 500
            $process.Refresh()
            if ($process.HasExited) {
                throw "Portable EXE exited early with code $($process.ExitCode)"
            }
            if (-not [string]::IsNullOrWhiteSpace($process.MainWindowTitle)) {
                $windowTitle = $process.MainWindowTitle
                break
            }
        }
        if ($windowTitle -ne $expectedWindowTitle) {
            throw "Portable window title was missing or unexpected: $windowTitle"
        }
        Stop-Process -Id $process.Id -Force
        $process.WaitForExit()
        $process = $null

        $after = @{}
        Get-ChildItem -LiteralPath $extractRoot -Recurse -File | ForEach-Object {
            $relative = (Get-ContainedRelativePath -BasePath $extractRoot -ChildPath $_.FullName).Replace('\', '/')
            $after[$relative] = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
        }
        if ($before.Count -ne $after.Count) {
            throw "Portable program directory file count changed after launch"
        }
        foreach ($name in $before.Keys) {
            if ($before[$name] -ne $after[$name]) {
                throw "Portable program directory changed after launch: $name"
            }
        }
        $programDirectoryUnchanged = $true
        $dataFiles = @(
            Get-ChildItem -LiteralPath $dataRoot -Recurse -File |
                ForEach-Object { $_.FullName.Substring($dataRoot.Length + 1) }
        )
        foreach ($requiredDataFile in @("accounts.db", "ledger.db")) {
            if ($dataFiles -notcontains $requiredDataFile) {
                throw "Portable launch did not create expected data file: $requiredDataFile"
            }
        }
    }

    $evidence = [ordered]@{
        verifiedAtUtc = [DateTime]::UtcNow.ToString("o")
        zipPath = $zip
        zipBytes = (Get-Item -LiteralPath $zip).Length
        zipSha256 = $actualZipHash
        zipEntries = $files.Count
        manifestEntries = @($manifest.files).Count
        checksumEntries = $checksumLines.Count
        forbiddenFiles = $forbidden.Count
        authenticodeStatus = [string]$authenticodeStatuses["InvoiceAssistant.exe"]
        authenticodeStatuses = $authenticodeStatuses
        updateManifestConfigured = [bool]$versionMetadata.updateManifestConfigured
        updateManifestUrl = $versionMetadata.updateManifestUrl
        manualUpdateCheckOnly = [bool]$versionMetadata.manualUpdateCheckOnly
        concurSendEnabled = [bool]$versionMetadata.concurSendEnabled
        concurSendManualConfirmationRequired = [bool]$versionMetadata.concurSendManualConfirmationRequired
        concurSendRealTestApproved = [bool]$versionMetadata.concurSendRealTestApproved
        dllSearchHardened = [bool]$versionMetadata.dllSearchHardened
        dependentLoadFlags = [string]$versionMetadata.dependentLoadFlags
        dllHardening = $dllHardening
        ocrWorker = $ocrWorkerEvidence
        launched = -not $SkipLaunch
        windowTitle = $windowTitle
        programDirectoryUnchanged = $programDirectoryUnchanged
        dataFiles = $dataFiles
        pathScenario = "temporary path with Chinese characters and spaces"
    }
    $json = $evidence | ConvertTo-Json -Depth 5
    if (-not [string]::IsNullOrWhiteSpace($EvidencePath)) {
        $evidenceFullPath = [System.IO.Path]::GetFullPath($EvidencePath)
        $evidenceParent = Split-Path -Parent $evidenceFullPath
        if (-not (Test-Path -LiteralPath $evidenceParent -PathType Container)) {
            throw "Evidence output directory does not exist: $evidenceParent"
        }
        [System.IO.File]::WriteAllText(
            $evidenceFullPath,
            $json + [Environment]::NewLine,
            [System.Text.UTF8Encoding]::new($false)
        )
    }
    Write-Output $json
}
finally {
    if ($null -ne $process) {
        try {
            $process.Refresh()
            if (-not $process.HasExited) {
                Stop-Process -Id $process.Id -Force
            }
        }
        catch {
            Write-Warning "Unable to stop portable smoke process: $($_.Exception.Message)"
        }
    }
    foreach ($candidate in @($extractRoot, $dataRoot)) {
        Assert-TemporaryChild -Path $candidate
        if (Test-Path -LiteralPath $candidate) {
            Remove-Item -LiteralPath $candidate -Recurse -Force
        }
    }
}
