[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$buildScript = Join-Path $PSScriptRoot "build-portable.ps1"
$originalFlag = $env:INVOICE_ENABLE_CONCUR_SEND
$rejected = $false

try {
    $env:INVOICE_ENABLE_CONCUR_SEND = "1"
    try {
        & $buildScript -SkipVerify -SkipBuild
    }
    catch {
        if ($_.Exception.Message -match "INVOICE_ENABLE_CONCUR_SEND") {
            $rejected = $true
        }
        else {
            throw
        }
    }
}
finally {
    $env:INVOICE_ENABLE_CONCUR_SEND = $originalFlag
}

if (-not $rejected) {
    throw "Unsigned internal Alpha packaging accepted INVOICE_ENABLE_CONCUR_SEND=1"
}

Write-Host "Concur internal-Alpha build gate self-test passed."
