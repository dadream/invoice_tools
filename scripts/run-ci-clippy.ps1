[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

$logPath = Join-Path ([System.IO.Path]::GetTempPath()) "invoice-assistant-clippy.log"
if (Test-Path -LiteralPath $logPath) {
    Remove-Item -LiteralPath $logPath -Force
}

& cargo clippy --workspace --all-targets --locked -- -D warnings 2>&1 |
    Tee-Object -FilePath $logPath
$exitCode = $LASTEXITCODE

if ($exitCode -eq 0) {
    Remove-Item -LiteralPath $logPath -Force -ErrorAction SilentlyContinue
    exit 0
}

$workspaceRoot = [System.IO.Path]::GetPathRoot((Get-Location).Path)
$driveName = $workspaceRoot.TrimEnd("\").TrimEnd(":")
$freeGiB = [Math]::Round((Get-PSDrive -Name $driveName).Free / 1GB, 2)
$logText = Get-Content -LiteralPath $logPath -Raw
$diskFull = $logText -match "(?i)(no space left|not enough space|disk full|os error 112)"
$memoryFailure = $logText -match "(?i)(out of memory|memory allocation.*failed|os error 1455)"
$diagnosticCount = @(
    Select-String -LiteralPath $logPath -Pattern "^(error|warning)(\[[^]]+\])?:" -AllMatches
).Count
$rustErrorCodes = @(
    [regex]::Matches($logText, "error\[(E\d{4})\]") |
        ForEach-Object { $_.Groups[1].Value } |
        Sort-Object -Unique
) -join ","
$message = "cargo clippy exited with code $exitCode; workspace drive free: $freeGiB GiB; diskFull=$diskFull; memoryFailure=$memoryFailure; compilerDiagnostics=$diagnosticCount; rustErrorCodes=$rustErrorCodes"
$message = $message.Replace("%", "%25").Replace("`r", "%0D").Replace("`n", "%0A")

# Surface only classified, non-sensitive failure metrics through the Checks
# annotations API. Never copy compiler output, paths, or source into it.
Write-Output "::error title=Rust Clippy failed::$message"
exit $exitCode
