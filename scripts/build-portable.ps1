[CmdletBinding()]
param(
    [switch]$SkipVerify,
    [switch]$SkipBuild,
    [ValidatePattern('^$|^[A-Za-z0-9][A-Za-z0-9.-]{0,63}$')]
    [string]$PackageTag = ""
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$configPath = Join-Path $projectRoot "src-tauri\tauri.conf.json"
$config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
$version = [string]$config.version
$artifactRoot = Join-Path $projectRoot "artifacts"
$packageBase = "InvoiceAssistant-$version-windows-x64-portable-UNSIGNED-INTERNAL-ALPHA"
if (-not [string]::IsNullOrWhiteSpace($PackageTag)) {
    $packageBase = "$packageBase-$PackageTag"
}
$stage = Join-Path $artifactRoot $packageBase
$zipPath = Join-Path $artifactRoot ($packageBase + ".zip")
$zipHashPath = $zipPath + ".sha256"
$exeSource = Join-Path $projectRoot "target\release\invoice-assistant.exe"
$workerSource = Join-Path $projectRoot "target\release\invoice-ocr-worker.exe"
$cargoExe = Join-Path ([Environment]::GetFolderPath("UserProfile")) ".cargo\bin\cargo.exe"
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$productDisplayName = -join @(0x53D1, 0x7968, 0x62A5, 0x9500, 0x52A9, 0x624B | ForEach-Object { [char]$_ })

function Get-ContainedRelativePath {
    param(
        [Parameter(Mandatory = $true)][string]$BasePath,
        [Parameter(Mandatory = $true)][string]$ChildPath
    )

    $base = [IO.Path]::GetFullPath($BasePath).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $child = [IO.Path]::GetFullPath($ChildPath)
    if (-not $child.StartsWith($base, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Package file escapes staging directory: $child"
    }
    return $child.Substring($base.Length)
}

function Write-JsonUtf8NoBom {
    param(
        [Parameter(Mandatory = $true)]$Value,
        [Parameter(Mandatory = $true)][int]$Depth,
        [Parameter(Mandatory = $true)][string]$Path
    )

    $json = $Value | ConvertTo-Json -Depth $Depth
    [IO.File]::WriteAllText($Path, $json + "`n", $script:Utf8NoBom)
}

& (Join-Path $PSScriptRoot "check-build-storage.ps1") `
    -Mode Preflight `
    -AutoCleanDev `
    -EvidencePath "artifacts/build-storage-portable-preflight.validation.json"

$updateManifestUrl = [string]$env:INVOICE_UPDATE_MANIFEST_URL
$updateManifestConfigured = -not [string]::IsNullOrWhiteSpace($updateManifestUrl)
if ($updateManifestConfigured) {
    try {
        $updateManifestUri = [Uri]$updateManifestUrl
    }
    catch {
        throw "INVOICE_UPDATE_MANIFEST_URL is not a valid absolute HTTPS URL"
    }
    if (-not $updateManifestUri.IsAbsoluteUri -or
        $updateManifestUri.Scheme -ne "https" -or
        [string]::IsNullOrWhiteSpace($updateManifestUri.Host) -or
        -not [string]::IsNullOrEmpty($updateManifestUri.UserInfo) -or
        -not [string]::IsNullOrEmpty($updateManifestUri.Fragment)) {
        throw "INVOICE_UPDATE_MANIFEST_URL must be an absolute HTTPS URL without credentials or fragment"
    }
    $updateManifestUrl = $updateManifestUri.AbsoluteUri
}

$concurSendFlag = [string]$env:INVOICE_ENABLE_CONCUR_SEND
if (-not [string]::IsNullOrWhiteSpace($concurSendFlag) -and $concurSendFlag -ne "0") {
    throw "INVOICE_ENABLE_CONCUR_SEND must remain disabled for unsigned internal Alpha artifacts"
}
$concurSendEnabled = $false

foreach ($path in @($stage, $zipPath, $zipHashPath)) {
    if (Test-Path -LiteralPath $path) {
        throw "Refusing to overwrite existing artifact: $path"
    }
}

if (-not $SkipVerify) {
    & (Join-Path $PSScriptRoot "verify-windows.ps1")
}

if (-not $SkipBuild) {
    $cargoBin = Join-Path ([Environment]::GetFolderPath("UserProfile")) ".cargo\bin"
    $originalProcessPath = $env:Path
    try {
        $env:Path = "$cargoBin;$originalProcessPath"
        & (Join-Path $projectRoot "ui\node_modules\.bin\tauri.cmd") build --no-bundle
        if ($LASTEXITCODE -ne 0) {
            throw "Tauri release build failed with exit code $LASTEXITCODE"
        }
        & $cargoExe build --release --locked -p invoice-parse --bin invoice-ocr-worker
        if ($LASTEXITCODE -ne 0) {
            throw "OCR worker release build failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        $env:Path = $originalProcessPath
    }
}

foreach ($requiredExe in @($exeSource, $workerSource)) {
    if (-not (Test-Path -LiteralPath $requiredExe -PathType Leaf)) {
        throw "Release EXE does not exist: $requiredExe"
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $requiredExe
    if ($signature.Status -eq "Valid") {
        throw "This script is for explicitly unsigned internal Alpha artifacts only"
    }
}

& (Join-Path $PSScriptRoot "test-dll-search-hardening.ps1") -ExecutablePath @($exeSource, $workerSource) | Out-Host

New-Item -ItemType Directory -Path $stage -Force | Out-Null
Copy-Item -LiteralPath $exeSource -Destination (Join-Path $stage "InvoiceAssistant.exe")
Copy-Item -LiteralPath $workerSource -Destination (Join-Path $stage "invoice-ocr-worker.exe")
$documents = @{
    "docs\release\PORTABLE-README-FIRST.txt" = "README-FIRST.txt"
    "docs\release\PRIVACY-DRAFT.md" = "PRIVACY-DRAFT.md"
    "docs\release\USER-AGREEMENT-DRAFT.md" = "USER-AGREEMENT-DRAFT.md"
    "docs\release\RELEASE-NOTES-0.1.0.md" = "RELEASE-NOTES.md"
    "docs\release\IT-REVIEW.md" = "IT-REVIEW.md"
    "docs\release\controlled-cleanup.md" = "CONTROLLED-CLEANUP.md"
    "docs\release\windows-validation-2026-08-19.md" = "VALIDATION-2026-08-19.md"
    "docs\release\data-version-and-migration-audit-2026-08-20.md" = "DATA-VERSION-AND-MIGRATION-AUDIT-2026-08-20.md"
    "docs\release\ocr-and-scanned-pdf-validation-2026-08-20.md" = "OCR-VALIDATION-2026-08-20.md"
    "docs\release\ocr-performance-2026-08-20.md" = "OCR-PERFORMANCE-2026-08-20.md"
    "docs\release\open-defects.md" = "OPEN-DEFECTS.md"
    "docs\security\private-fixture-remediation-2026-08-20.md" = "PRIVATE-FIXTURE-REMEDIATION-2026-08-20.md"
    "docs\testing\fixture-inventory.md" = "FIXTURE-INVENTORY.md"
    "docs\release\version-manifest-schema.md" = "VERSION-MANIFEST-SCHEMA.md"
    "docs\release\concur-receipt-email-design-and-validation.md" = "CONCUR-RECEIPT-EMAIL-DESIGN-AND-VALIDATION.md"
}
foreach ($entry in $documents.GetEnumerator()) {
    Copy-Item -LiteralPath (Join-Path $projectRoot $entry.Key) -Destination (Join-Path $stage $entry.Value)
}

$cargoJson = & $cargoExe metadata --locked --format-version 1
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed" }
$cargoMetadata = $cargoJson | ConvertFrom-Json
$cargoPackages = @(
    $cargoMetadata.packages |
        Where-Object { $null -ne $_.source } |
        Sort-Object name, version |
        ForEach-Object {
            [pscustomobject]@{ type="library"; ecosystem="cargo"; name=$_.name; version=$_.version; license=$_.license }
        }
)
$npmLockRaw = Get-Content -LiteralPath (Join-Path $projectRoot "ui\package-lock.json") -Raw
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
        Where-Object { $_.Name -like "node_modules/*" } |
        Sort-Object Name |
        ForEach-Object {
            [pscustomobject]@{ type="library"; ecosystem="npm"; name=$_.Name.Substring(13); version=$_.Value.version; license=$_.Value.license }
        }
)
$fontLock = Get-Content -LiteralPath (Join-Path $projectRoot "third_party\fonts\fonts.lock.json") -Raw | ConvertFrom-Json
$fontPackages = @(
    $fontLock.fonts | ForEach-Object {
        [pscustomobject]@{ type="file"; ecosystem="font"; name=$_.family; version=$_.version; license=$_.license }
    }
)
foreach ($font in $fontLock.fonts) {
    Copy-Item -LiteralPath (Join-Path $projectRoot $font.licenseFile) -Destination (Join-Path $stage $font.licenseOutput)
}
$ocrLock = Get-Content -LiteralPath (Join-Path $projectRoot "third_party\ocr\ocr.lock.json") -Raw | ConvertFrom-Json
$ocrAssetSource = Join-Path $projectRoot "src-tauri\assets\ocr"
Copy-Item -LiteralPath $ocrAssetSource -Destination (Join-Path $stage "ocr") -Recurse
foreach ($license in $ocrLock.licenses) {
    $outputPath = [IO.Path]::GetFullPath((Join-Path $stage $license.licenseOutput))
    if (-not $outputPath.StartsWith($stage + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Unsafe OCR license output path: $($license.licenseOutput)"
    }
    New-Item -ItemType Directory -Path (Split-Path -Parent $outputPath) -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $projectRoot $license.path) -Destination $outputPath
}
Copy-Item -LiteralPath (Join-Path $projectRoot "third_party\ocr\ocr.lock.json") -Destination (Join-Path $stage "LICENSES\OCR\ocr.lock.json")
$ocrPackages = @(
    [pscustomobject]@{ type="file"; ecosystem="ocr-runtime"; name=$ocrLock.runtime.name; version=$ocrLock.runtime.version; license=$ocrLock.runtime.license }
    [pscustomobject]@{ type="file"; ecosystem="ocr-model"; name=$ocrLock.models.name; version=$ocrLock.models.version; license=$ocrLock.models.license }
)
$components = @($cargoPackages) + @($npmPackages) + @($fontPackages) + @($ocrPackages)

$noticeLines = [System.Collections.Generic.List[string]]::new()
$noticeLines.Add("Third-party dependency inventory for Invoice Assistant $version")
$noticeLines.Add("Generated from Cargo.lock, ui/package-lock.json, third_party/fonts/fonts.lock.json, and third_party/ocr/ocr.lock.json. Review source licenses before public release.")
$noticeLines.Add("")
foreach ($component in $components) {
    $noticeLines.Add("$($component.ecosystem) $($component.name) $($component.version) - $($component.license)")
}
foreach ($font in $fontLock.fonts) {
    $noticeLines.Add("font-source $($font.family) - $($font.repository)@$($font.commit) - license file $($font.licenseOutput)")
}
$noticeLines.Add("ocr-runtime-source $($ocrLock.runtime.name) - $($ocrLock.runtime.packageUrl) - package SHA-256 $($ocrLock.runtime.packageSha256)")
foreach ($model in $ocrLock.models.files) {
    $noticeLines.Add("ocr-model-source $([IO.Path]::GetFileName($model.path)) - $($model.sourceUrl) - SHA-256 $($model.sha256)")
}
[System.IO.File]::WriteAllLines((Join-Path $stage "THIRD-PARTY-NOTICES.txt"), $noticeLines)

$sbomComponents = @(
    $components | ForEach-Object {
        [ordered]@{
            type = $_.type
            group = $_.ecosystem
            name = $_.name
            version = $_.version
            licenses = @([ordered]@{ expression = $_.license })
            properties = @(
                [ordered]@{ name = "invoice-assistant:ecosystem"; value = $_.ecosystem }
            )
        }
    }
)
$sbom = [ordered]@{
    bomFormat = "CycloneDX"
    specVersion = "1.5"
    serialNumber = "urn:uuid:$([guid]::NewGuid())"
    version = 1
    metadata = [ordered]@{
        timestamp = [DateTime]::UtcNow.ToString("o")
        component = [ordered]@{ type="application"; name="InvoiceAssistant"; version=$version }
    }
    components = $sbomComponents
}
Write-JsonUtf8NoBom -Value $sbom -Depth 8 -Path (Join-Path $stage "SBOM.cdx.json")

$versionMetadata = [ordered]@{
    schemaVersion = 1
    product = "com.dadream.invoiceassistant"
    version = $version
    channel = "internal-alpha"
    signed = $false
    manualUpdateCheckOnly = $true
    automaticDownload = $false
    concurSendEnabled = $concurSendEnabled
    concurSendManualConfirmationRequired = $true
    concurSendRealTestApproved = $false
    dllSearchHardened = $true
    dependentLoadFlags = "0x0800"
    ocrRuntimeDirectoryPolicy = "VerifiedAddDllDirectory"
    updateManifestConfigured = $updateManifestConfigured
    updateManifestUrl = $(
        if ($updateManifestConfigured) { $updateManifestUrl } else { $null }
    )
}
Write-JsonUtf8NoBom -Value $versionMetadata -Depth 4 -Path (Join-Path $stage "version.json")

$manifestFiles = @(
    Get-ChildItem -LiteralPath $stage -Recurse -File |
        Sort-Object FullName |
        ForEach-Object {
            [ordered]@{
                path = (Get-ContainedRelativePath -BasePath $stage -ChildPath $_.FullName).Replace('\', '/')
                bytes = $_.Length
                sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
            }
        }
)
$manifest = [ordered]@{
    formatVersion = 1
    product = $productDisplayName
    version = $version
    platform = "windows-x64-portable"
    channel = "internal-alpha"
    signed = $false
    generatedAtUtc = [DateTime]::UtcNow.ToString("o")
    files = $manifestFiles
}
Write-JsonUtf8NoBom -Value $manifest -Depth 6 -Path (Join-Path $stage "manifest.json")

$forbidden = @(Get-ChildItem -LiteralPath $stage -Recurse -File | Where-Object {
    $_.Name -match '(?i)(^\.env|\.eml$|\.db$|\.pfx$|\.p12$|\.pem$|\.key$|\.log$)'
})
if ($forbidden.Count -gt 0) {
    throw "Forbidden files detected in portable staging directory"
}

$checksums = Get-ChildItem -LiteralPath $stage -Recurse -File | Sort-Object FullName | ForEach-Object {
    $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
    $relativePath = (Get-ContainedRelativePath -BasePath $stage -ChildPath $_.FullName).Replace('\', '/')
    "$hash  $relativePath"
}
[System.IO.File]::WriteAllLines((Join-Path $stage "SHA256SUMS.txt"), $checksums)

Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zipPath -CompressionLevel Optimal
$zipHash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash
[System.IO.File]::WriteAllText($zipHashPath, "$zipHash  $([System.IO.Path]::GetFileName($zipPath))`r`n")

& (Join-Path $PSScriptRoot "check-build-storage.ps1") `
    -Mode Postflight `
    -AutoCleanDev `
    -EvidencePath "artifacts/build-storage-portable-postflight.validation.json"

Write-Output "Portable internal Alpha created:"
Write-Output $zipPath
Write-Output "SHA256=$zipHash"
