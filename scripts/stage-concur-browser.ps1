[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$Destination)
$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$target = if ([IO.Path]::IsPathRooted($Destination)) { [IO.Path]::GetFullPath($Destination) } else { [IO.Path]::GetFullPath((Join-Path $projectRoot $Destination)) }
$allowed = @('target','artifacts') | Where-Object { $target.StartsWith((Join-Path $projectRoot $_) + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) }
if (-not $allowed -or [IO.Path]::GetFileName($target) -ne 'concur-browser') { throw 'Concur runtime may only be staged inside target/artifacts in a concur-browser directory' }
$source = Join-Path $projectRoot 'sidecars/concur-browser'
$node = (Get-Command node -ErrorAction Stop).Source
$expected = (Get-Content (Join-Path $projectRoot '.nvmrc') -Raw).Trim().TrimStart('v')
$actual = (& $node --version).Trim().TrimStart('v')
if ($actual -ne $expected) { throw "Use the pinned Node runtime $expected (found $actual)" }
if (-not (Test-Path -LiteralPath (Join-Path $source 'node_modules/playwright-core/package.json'))) { throw 'Missing locked browser dependency. Run npm ci --prefix sidecars/concur-browser' }
$package = Get-Content (Join-Path $source 'node_modules/playwright-core/package.json') -Raw | ConvertFrom-Json
$declared = Get-Content (Join-Path $source 'package.json') -Raw | ConvertFrom-Json
if ($package.version -ne $declared.dependencies.'playwright-core') { throw 'Playwright version differs from locked runtime' }
New-Item -ItemType Directory -Path $target -Force | Out-Null
Copy-Item -LiteralPath $node -Destination (Join-Path $target 'node.exe') -Force
foreach ($file in @('package.json','worker.mjs','adapter.mjs')) { Copy-Item -LiteralPath (Join-Path $source $file) -Destination (Join-Path $target $file) -Force }
Copy-Item -LiteralPath (Join-Path $source 'samples') -Destination $target -Recurse -Force
$dependencySource = Join-Path $source 'node_modules/playwright-core'
$dependencyTarget = Join-Path $target 'node_modules/playwright-core'
New-Item -ItemType Directory -Path $dependencyTarget -Force | Out-Null
# Only the installed runtime is distributed, never tests, node_modules caches or local browser profiles.
Get-ChildItem -LiteralPath $dependencySource -File -Recurse | Where-Object { $_.Name -ne 'README.md' -and $_.Name -notlike '*.d.ts' } | ForEach-Object {
    $relative = $_.FullName.Substring($dependencySource.Length).TrimStart('\','/')
    $output = Join-Path $dependencyTarget $relative
    New-Item -ItemType Directory -Path (Split-Path -Parent $output) -Force | Out-Null
    Copy-Item -LiteralPath $_.FullName -Destination $output -Force
}
$license = & $node --print 'process.release.name'
if ($LASTEXITCODE -ne 0 -or $license -ne 'node') { throw 'Invalid Node runtime' }
$licenseSource = Join-Path (Split-Path -Parent $node) 'LICENSE'
if (-not (Test-Path -LiteralPath $licenseSource)) { throw 'Node license file is missing next to node.exe; do not distribute without notices' }
Copy-Item -LiteralPath $licenseSource -Destination (Join-Path $target 'NODE-LICENSE.txt') -Force
$sample = Get-Content (Join-Path $target 'samples/sample.json') -Raw -Encoding UTF8 | ConvertFrom-Json
foreach ($expense in $sample.expenses) { foreach ($doc in $expense.documents) {
    $path = Join-Path $target "samples/$($doc.path)"
    if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $doc.sha256) { throw 'Synthetic Concur sample integrity failed' }
} }
$bytes = (Get-ChildItem -LiteralPath $target -File -Recurse | Measure-Object Length -Sum).Sum
if ($bytes -gt 256MB) { throw 'Concur runtime exceeds its 256 MiB distribution budget' }
Write-Host "Concur runtime and 6 synthetic PDFs staged: $target ($([Math]::Round($bytes/1MB,1)) MiB)"
