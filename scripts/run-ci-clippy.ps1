[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

$logPath = Join-Path ([System.IO.Path]::GetTempPath()) "invoice-assistant-clippy.log"
if (Test-Path -LiteralPath $logPath) {
    Remove-Item -LiteralPath $logPath -Force
}

Write-Output "Running workspace Clippy with structured diagnostics..."
& cargo clippy --workspace --all-targets --locked --message-format=json -- -D warnings 2>&1 |
    Set-Content -LiteralPath $logPath -Encoding utf8
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
$lintCodes = @(
    [regex]::Matches($logText, "clippy::([a-z0-9_]+)") |
        ForEach-Object { $_.Groups[1].Value } |
        Sort-Object -Unique
) -join ","
$failureClasses = @(
    if ($logText -match "(?i)failed to run custom build command") { "build-script" }
    if ($logText -match "(?i)linking with .* failed") { "linker" }
    if ($logText -match "(?i)could not compile") { "compiler" }
    if ($logText -match "(?i)failed to download") { "download" }
) -join ","

function Get-DiagnosticClass {
    param([string]$DiagnosticMessage)

    switch -Regex ($DiagnosticMessage) {
        "(?i)(couldn't read|could not read|no such file|cannot find the (file|path))" { return "missing-input" }
        "(?i)environment variable .* not defined" { return "missing-environment" }
        "(?i)access is denied" { return "permission" }
        "(?i)unresolved import" { return "unresolved-import" }
        "(?i)cannot find .* in this scope" { return "missing-symbol" }
        "(?i)no method named" { return "missing-method" }
        "(?i)mismatched types" { return "type-mismatch" }
        "(?i)trait bound .* is not satisfied" { return "trait-bound" }
        "(?i)deprecated" { return "deprecated" }
        "(?i)unused" { return "unused" }
        "(?i)proc macro panicked" { return "proc-macro" }
        "(?i)linking with .* failed" { return "linker" }
        "(?i)failed to run custom build command" { return "build-script" }
        default { return "other" }
    }
}

$structuredDiagnostics = @(
    foreach ($line in Get-Content -LiteralPath $logPath) {
        try {
            $event = $line | ConvertFrom-Json -ErrorAction Stop
        }
        catch {
            continue
        }
        if ($event.reason -ne "compiler-message" -or $event.message.level -notin @("error", "warning")) {
            continue
        }
        $targetName = if ($event.target.name) { $event.target.name } else { "unknown-target" }
        $diagnosticCode = if ($event.message.code.code) { $event.message.code.code } else { "no-code" }
        $diagnosticClass = Get-DiagnosticClass -DiagnosticMessage $event.message.message
        "$targetName`:$($event.message.level)`:$diagnosticCode`:$diagnosticClass"
    }
) | Sort-Object -Unique
$structuredSummary = $structuredDiagnostics -join ","

$message = "cargo clippy exited with code $exitCode; workspace drive free: $freeGiB GiB; diskFull=$diskFull; memoryFailure=$memoryFailure; compilerDiagnostics=$diagnosticCount; rustErrorCodes=$rustErrorCodes; lintCodes=$lintCodes; failureClasses=$failureClasses; structured=$structuredSummary"
$message = $message.Replace("%", "%25").Replace("`r", "%0D").Replace("`n", "%0A")

# Surface only classified, non-sensitive failure metrics through the Checks
# annotations API. Never copy compiler output, paths, or source into it.
Write-Output "::error title=Rust Clippy failed::$message"
exit $exitCode
