[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$uiLockPath = Join-Path $projectRoot "ui\package-lock.json"
$fontLockPath = Join-Path $projectRoot "third_party\fonts\fonts.lock.json"
$ocrLockPath = Join-Path $projectRoot "third_party\ocr\ocr.lock.json"
$blockedPattern = '(?i)(AGPL|SSPL|GPL-3)'

function Get-NormalizedTextSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)

    $bytes = [IO.File]::ReadAllBytes($Path)
    $utf8 = New-Object System.Text.UTF8Encoding($false, $true)
    $text = $utf8.GetString($bytes).Replace("`r`n", "`n").Replace("`r", "`n")
    $normalizedBytes = $utf8.GetBytes($text)
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($sha256.ComputeHash($normalizedBytes))).Replace('-', '')
    }
    finally {
        $sha256.Dispose()
    }
}

$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
if ($null -ne $cargoCommand) {
    $cargoExe = $cargoCommand.Source
}
else {
    $cargoExe = Join-Path ([Environment]::GetFolderPath("UserProfile")) ".cargo\bin\cargo.exe"
}
if (-not (Test-Path -LiteralPath $cargoExe -PathType Leaf)) {
    throw "cargo was not found"
}

if (-not (Test-Path -LiteralPath $fontLockPath -PathType Leaf)) {
    throw "font asset lock was not found"
}
$fontLock = Get-Content -LiteralPath $fontLockPath -Raw | ConvertFrom-Json
if ($fontLock.formatVersion -ne 1 -or @($fontLock.fonts).Count -eq 0) {
    throw "font asset lock is invalid"
}
$fontAssetCount = 0
foreach ($font in $fontLock.fonts) {
    if ([string]::IsNullOrWhiteSpace($font.family) -or
        [string]::IsNullOrWhiteSpace($font.version) -or
        [string]::IsNullOrWhiteSpace($font.license) -or
        $font.license -match $blockedPattern) {
        throw "font license metadata is missing or blocked: $($font.family)"
    }
    $licensePath = [IO.Path]::GetFullPath((Join-Path $projectRoot $font.licenseFile))
    if (-not $licensePath.StartsWith($projectRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or
        -not (Test-Path -LiteralPath $licensePath -PathType Leaf)) {
        throw "font license path is unsafe or missing: $($font.licenseFile)"
    }
    if ((Get-NormalizedTextSha256 -Path $licensePath) -ne $font.licenseSha256 -or
        (Get-Content -LiteralPath $licensePath -Raw) -notmatch 'SIL OPEN FONT LICENSE') {
        throw "font license verification failed: $($font.family)"
    }
    foreach ($asset in $font.files) {
        $fontAssetCount++
        $assetPath = [IO.Path]::GetFullPath((Join-Path $projectRoot $asset.path))
        if (-not $assetPath.StartsWith($projectRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or
            -not (Test-Path -LiteralPath $assetPath -PathType Leaf)) {
            throw "font asset path is unsafe or missing: $($asset.path)"
        }
        if ((Get-FileHash -LiteralPath $assetPath -Algorithm SHA256).Hash -ne $asset.sha256) {
            throw "font asset hash mismatch: $($asset.path)"
        }
    }
}

if (-not (Test-Path -LiteralPath $ocrLockPath -PathType Leaf)) {
    throw "OCR asset lock was not found"
}
$ocrLock = Get-Content -LiteralPath $ocrLockPath -Raw | ConvertFrom-Json
if ($ocrLock.formatVersion -ne 1 -or
    @($ocrLock.runtime.files).Count -eq 0 -or
    @($ocrLock.models.files).Count -eq 0 -or
    @($ocrLock.licenses).Count -eq 0) {
    throw "OCR asset lock is invalid"
}
foreach ($component in @($ocrLock.runtime, $ocrLock.models)) {
    if ([string]::IsNullOrWhiteSpace($component.name) -or
        [string]::IsNullOrWhiteSpace($component.version) -or
        [string]::IsNullOrWhiteSpace($component.license) -or
        $component.license -match $blockedPattern) {
        throw "OCR component license metadata is missing or blocked: $($component.name)"
    }
}
$ocrAssets = @($ocrLock.runtime.files) + @($ocrLock.models.files) + @($ocrLock.licenses)
foreach ($asset in $ocrAssets) {
    $assetPath = [IO.Path]::GetFullPath((Join-Path $projectRoot $asset.path))
    if (-not $assetPath.StartsWith($projectRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or
        -not (Test-Path -LiteralPath $assetPath -PathType Leaf)) {
        throw "OCR asset path is unsafe or missing: $($asset.path)"
    }
    if ($null -ne $asset.bytes -and (Get-Item -LiteralPath $assetPath).Length -ne [long]$asset.bytes) {
        throw "OCR asset size mismatch: $($asset.path)"
    }
    if ((Get-FileHash -LiteralPath $assetPath -Algorithm SHA256).Hash -ne $asset.sha256) {
        throw "OCR asset hash mismatch: $($asset.path)"
    }
}
Push-Location $projectRoot
try {
    $cargoJson = & $cargoExe metadata --locked --format-version 1
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed"
    }
    $metadata = $cargoJson | ConvertFrom-Json
    $cargoPackages = @($metadata.packages | Where-Object { $null -ne $_.source })
    $cargoMissing = @(
        $cargoPackages | Where-Object {
            [string]::IsNullOrWhiteSpace($_.license) -and [string]::IsNullOrWhiteSpace($_.license_file)
        }
    )
    $cargoBlocked = @($cargoPackages | Where-Object { $_.license -match $blockedPattern })

    # package-lock v3 uses an empty-string property for the workspace root.
    # Windows PowerShell 5.1 cannot deserialize that key, while PowerShell 7
    # requires -AsHashtable (which 5.1 does not provide). Rename only that
    # structural root key before parsing so the same release gate works in both.
    $npmLockRaw = Get-Content -LiteralPath $uiLockPath -Raw
    $normalizedNpmLockRaw = [regex]::Replace(
        $npmLockRaw,
        '("packages"\s*:\s*\{\s*)""(\s*:)',
        '$1"__workspace_root__"$2',
        1
    )
    if ($normalizedNpmLockRaw -eq $npmLockRaw) {
        throw "package-lock workspace root entry was not found"
    }
    $npmLock = $normalizedNpmLockRaw | ConvertFrom-Json
    $npmPackages = @(
        $npmLock.packages.PSObject.Properties |
            Where-Object { $_.Name -ne "" -and $_.Name -like "node_modules/*" }
    )
    $npmMissing = @(
        $npmPackages | Where-Object {
            $null -eq $_.Value.PSObject.Properties["license"] -or
            [string]::IsNullOrWhiteSpace([string]$_.Value.license)
        }
    )
    $npmBlocked = @($npmPackages | Where-Object { [string]$_.Value.license -match $blockedPattern })
}
finally {
    Pop-Location
}

if ($cargoMissing.Count -gt 0 -or $npmMissing.Count -gt 0) {
    Write-Error "License metadata is missing: Cargo=$($cargoMissing.Count), npm=$($npmMissing.Count)."
    exit 1
}
if ($cargoBlocked.Count -gt 0 -or $npmBlocked.Count -gt 0) {
    Write-Error "Blocked copyleft license detected: Cargo=$($cargoBlocked.Count), npm=$($npmBlocked.Count)."
    exit 1
}

Write-Output "License scan passed: Cargo=$($cargoPackages.Count), npm=$($npmPackages.Count), fonts=$(@($fontLock.fonts).Count), font-assets=$fontAssetCount, OCR-assets=$($ocrAssets.Count)."
