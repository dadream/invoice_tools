[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$projectRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot)).TrimEnd(
    [IO.Path]::DirectorySeparatorChar
)
$runId = [Guid]::NewGuid().ToString('N')
$candidateName = "secret-scan-selftest-$runId.txt"
$candidatePath = [IO.Path]::GetFullPath((Join-Path $projectRoot $candidateName))
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd(
    [IO.Path]::DirectorySeparatorChar
)
$stdoutPath = Join-Path $tempRoot "invoice-secret-scan-$runId.out"
$stderrPath = Join-Path $tempRoot "invoice-secret-scan-$runId.err"
$process = $null

if ([IO.Path]::GetDirectoryName($candidatePath) -ne $projectRoot) {
    throw "Secret scan self-test path escaped the project root"
}

try {
    $variableName = 'INVOICE_' + 'AUTH_TOKEN'
    $simulatedValue = 'drill-' + $runId
    [IO.File]::WriteAllText(
        $candidatePath,
        $variableName + '=' + $simulatedValue + [Environment]::NewLine,
        [Text.UTF8Encoding]::new($false)
    )

    $shellPath = (Get-Process -Id $PID).Path
    $process = Start-Process -FilePath $shellPath `
        -ArgumentList @('-NoProfile', '-File', (Join-Path $PSScriptRoot 'scan-secrets.ps1')) `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError $stderrPath `
        -WindowStyle Hidden `
        -PassThru `
        -Wait
    if ($process.ExitCode -eq 0) {
        throw "Secret scan self-test expected the simulated leak to be rejected"
    }
    $combined = ''
    foreach ($output in @($stdoutPath, $stderrPath)) {
        if (Test-Path -LiteralPath $output -PathType Leaf) {
            $combined += [IO.File]::ReadAllText($output)
        }
    }
    if (-not $combined.Contains($candidateName)) {
        throw "Secret scan self-test failed without identifying the candidate file"
    }
}
finally {
    foreach ($path in @($candidatePath, $stdoutPath, $stderrPath)) {
        if (Test-Path -LiteralPath $path) {
            Remove-Item -LiteralPath $path -Force
        }
    }
}

Write-Output "Secret scan self-test passed; simulated value was rejected and removed."
