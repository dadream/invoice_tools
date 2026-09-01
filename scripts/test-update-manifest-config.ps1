[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$runId = [Guid]::NewGuid().ToString('N')
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd(
    [IO.Path]::DirectorySeparatorChar
)
$stdoutPath = Join-Path $tempRoot "invoice-update-config-$runId.out"
$stderrPath = Join-Path $tempRoot "invoice-update-config-$runId.err"
$previousValue = [Environment]::GetEnvironmentVariable(
    'INVOICE_UPDATE_MANIFEST_URL',
    [EnvironmentVariableTarget]::Process
)

try {
    [Environment]::SetEnvironmentVariable(
        'INVOICE_UPDATE_MANIFEST_URL',
        'http://updates.example.invalid/version.json',
        [EnvironmentVariableTarget]::Process
    )
    $shellPath = (Get-Process -Id $PID).Path
    $process = Start-Process -FilePath $shellPath `
        -ArgumentList @(
            '-NoProfile',
            '-File',
            (Join-Path $PSScriptRoot 'build-portable.ps1'),
            '-SkipVerify',
            '-SkipBuild'
        ) `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError $stderrPath `
        -WindowStyle Hidden `
        -PassThru `
        -Wait
    if ($process.ExitCode -eq 0) {
        throw "Update manifest config self-test expected insecure HTTP to be rejected"
    }

    $combined = ''
    foreach ($output in @($stdoutPath, $stderrPath)) {
        if (Test-Path -LiteralPath $output -PathType Leaf) {
            $combined += [IO.File]::ReadAllText($output)
        }
    }
    if (-not $combined.Contains('must be an absolute HTTPS URL')) {
        throw "Update manifest config self-test did not report the HTTPS policy"
    }
}
finally {
    [Environment]::SetEnvironmentVariable(
        'INVOICE_UPDATE_MANIFEST_URL',
        $previousValue,
        [EnvironmentVariableTarget]::Process
    )
    foreach ($path in @($stdoutPath, $stderrPath)) {
        if (Test-Path -LiteralPath $path) {
            Remove-Item -LiteralPath $path -Force
        }
    }
}

Write-Output "Update manifest config self-test passed; insecure HTTP was rejected."
