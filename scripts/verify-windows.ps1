[CmdletBinding()]
param(
    [switch]$SkipTests,
    [switch]$SkipPrerequisiteChecks,
    [ValidateRange(-1, 1024)]
    [double]$MinimumFreeGiB = -1
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)]
        [string]$FilePath,
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $FilePath $($Arguments -join ' ')"
    }
}

$uiRoot = Join-Path $projectRoot "ui"
if (-not $SkipPrerequisiteChecks) {
    & (Join-Path $PSScriptRoot "check-build-storage.ps1") `
        -Mode Preflight `
        -AutoCleanDev `
        -MinimumFreeGiB $MinimumFreeGiB `
        -EvidencePath "artifacts/build-storage-preflight.validation.json"
    & (Join-Path $PSScriptRoot "test-build-storage-gate.ps1")
    & (Join-Path $PSScriptRoot "scan-secrets.ps1")
    & (Join-Path $PSScriptRoot "test-secret-scan.ps1")
    & (Join-Path $PSScriptRoot "scan-private-fixtures.ps1") -SelfTest
    & (Join-Path $PSScriptRoot "test-update-manifest-config.ps1")
    & (Join-Path $PSScriptRoot "test-concur-send-build-gate.ps1")
    & (Join-Path $PSScriptRoot "check-third-party-licenses.ps1")
    & (Join-Path $PSScriptRoot "assert-release-version.ps1") | Out-Host
    & (Join-Path $PSScriptRoot "test-release-version.ps1")
}

$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
if ($null -ne $cargoCommand) {
    $cargoExe = $cargoCommand.Source
}
else {
    $cargoExe = Join-Path ([Environment]::GetFolderPath("UserProfile")) ".cargo\bin\cargo.exe"
    if (-not (Test-Path -LiteralPath $cargoExe -PathType Leaf)) {
        throw "cargo was not found. Install Rust 1.97.1 with rustup or add cargo to PATH."
    }
}

Push-Location $projectRoot
try {
    Invoke-Checked -FilePath $cargoExe -Arguments @("build", "-p", "invoice-parse", "--bin", "invoice-ocr-worker", "--locked")
    Invoke-Checked -FilePath $cargoExe -Arguments @("fmt", "--all", "--", "--check")
    Invoke-Checked -FilePath $cargoExe -Arguments @("clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings")
    Invoke-Checked -FilePath $cargoExe -Arguments @("check", "--workspace", "--all-targets", "--locked")
    if (-not $SkipTests) {
        Invoke-Checked -FilePath $cargoExe -Arguments @("test", "--workspace", "--all-targets", "--locked")
        Invoke-Checked -FilePath $cargoExe -Arguments @(
            "test",
            "-p", "invoice-parse",
            "offline_ocr_reads_synthetic_vat_invoice",
            "--locked",
            "--",
            "--ignored"
        )
        Invoke-Checked -FilePath $cargoExe -Arguments @(
            "test",
            "-p", "invoice-parse",
            "scanned_pdf_ocr_reads_synthetic_vat_invoice",
            "--locked",
            "--",
            "--ignored"
        )
        Invoke-Checked -FilePath $cargoExe -Arguments @(
            "test",
            "-p", "invoice-assistant",
            "--test", "ocr_worker_process",
            "production_worker_roundtrips_image_and_scanned_pdf",
            "--locked",
            "--",
            "--ignored",
            "--exact"
        )
    }

    Push-Location $uiRoot
    try {
        Invoke-Checked -FilePath "npm" -Arguments @("run", "check")
        if (-not $SkipTests) {
            Invoke-Checked -FilePath "npm" -Arguments @("test")
        }
        Invoke-Checked -FilePath "npm" -Arguments @("run", "build")
    }
    finally {
        Pop-Location
    }
}
finally {
    Pop-Location
}

& (Join-Path $PSScriptRoot "check-build-storage.ps1") `
    -Mode Postflight `
    -AutoCleanDev `
    -MinimumFreeGiB $MinimumFreeGiB `
    -EvidencePath "artifacts/build-storage-postflight.validation.json"
