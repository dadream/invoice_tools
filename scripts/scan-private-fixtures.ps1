[CmdletBinding()]
param([switch]$SelfTest)

$ErrorActionPreference = "Stop"
$projectRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot)).TrimEnd(
    [IO.Path]::DirectorySeparatorChar
)
$inventoryPath = Join-Path $projectRoot "fixtures\inventory.json"
$fixtureRoot = Join-Path $projectRoot "fixtures"

function Resolve-ProjectFixturePath {
    param([Parameter(Mandatory = $true)][string]$RelativePath)
    if ([string]::IsNullOrWhiteSpace($RelativePath) -or [IO.Path]::IsPathRooted($RelativePath)) {
        throw "Fixture inventory contains an invalid path"
    }
    $full = [IO.Path]::GetFullPath((Join-Path $projectRoot $RelativePath.Replace('/', '\')))
    if (-not $full.StartsWith(
        $projectRoot + [IO.Path]::DirectorySeparatorChar,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "Fixture inventory path escapes the project root"
    }
    return $full
}

function Test-SyntheticMarker {
    param([Parameter(Mandatory = $true)][string]$Content)
    return [regex]::IsMatch(
        $Content,
        '(?i)synthetic|synthetic-only|example\.invalid|虚构|示例|测试|演示'
    )
}

function Get-NormalizedTextFixture {
    param([Parameter(Mandatory = $true)][string]$Path)

    $utf8 = New-Object System.Text.UTF8Encoding($false, $true)
    $text = $utf8.GetString([IO.File]::ReadAllBytes($Path)).Replace("`r`n", "`n").Replace("`r", "`n")
    $bytes = $utf8.GetBytes($text)
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        return [pscustomobject]@{
            text = $text
            bytes = $bytes.Length
            sha256 = ([BitConverter]::ToString($sha256.ComputeHash($bytes))).Replace('-', '')
        }
    }
    finally {
        $sha256.Dispose()
    }
}

if ($SelfTest) {
    if (-not (Test-SyntheticMarker 'synthetic fixture using example.invalid')) {
        throw "Fixture privacy scanner self-test failed to accept synthetic data"
    }
    $unmarked = ('ordinary invoice content ' + ('1' * 20))
    if (Test-SyntheticMarker $unmarked) {
        throw "Fixture privacy scanner self-test failed to reject unmarked data"
    }
    $selfTestBase = [IO.Path]::GetFullPath((Join-Path $projectRoot ".tmp"))
    $selfTestPrefix = $selfTestBase.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $selfTestRoot = [IO.Path]::GetFullPath((Join-Path $selfTestBase ("fixture-line-ending-test-" + [guid]::NewGuid().ToString("N"))))
    if (-not $selfTestRoot.StartsWith($selfTestPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Unsafe fixture line-ending self-test path"
    }
    try {
        New-Item -ItemType Directory -Path $selfTestRoot | Out-Null
        $lfPath = Join-Path $selfTestRoot "lf.txt"
        $crlfPath = Join-Path $selfTestRoot "crlf.txt"
        [IO.File]::WriteAllText($lfPath, "synthetic`nfixture`n", [Text.UTF8Encoding]::new($false))
        [IO.File]::WriteAllText($crlfPath, "synthetic`r`nfixture`r`n", [Text.UTF8Encoding]::new($false))
        $lf = Get-NormalizedTextFixture -Path $lfPath
        $crlf = Get-NormalizedTextFixture -Path $crlfPath
        if ($lf.bytes -ne $crlf.bytes -or $lf.sha256 -ne $crlf.sha256) {
            throw "Fixture privacy scanner self-test failed to normalize line endings"
        }
    }
    finally {
        if (Test-Path -LiteralPath $selfTestRoot) {
            $resolvedSelfTestRoot = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $selfTestRoot))
            $selfTestItem = Get-Item -LiteralPath $resolvedSelfTestRoot -Force
            if (-not $resolvedSelfTestRoot.StartsWith($selfTestPrefix, [StringComparison]::OrdinalIgnoreCase) -or
                ($selfTestItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Refusing to remove unsafe fixture line-ending self-test directory"
            }
            Remove-Item -LiteralPath $resolvedSelfTestRoot -Recurse -Force
        }
    }
    try {
        Resolve-ProjectFixturePath '..\outside-private-data.bin' | Out-Null
        throw "Fixture privacy scanner self-test failed to reject traversal"
    }
    catch {
        if ($_.Exception.Message -like '*self-test failed*') { throw }
    }
}

foreach ($legacyPath in @(
    (Join-Path $fixtureRoot 'test-images'),
    (Join-Path $fixtureRoot 'samples')
)) {
    if (Test-Path -LiteralPath $legacyPath) {
        throw "Unreviewed legacy/private fixture directory is present"
    }
}

if (-not (Test-Path -LiteralPath $inventoryPath -PathType Leaf)) {
    throw "Fixture privacy inventory is missing"
}
$inventoryItem = Get-Item -LiteralPath $inventoryPath -Force
if (($inventoryItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "Fixture privacy inventory cannot be a reparse point"
}
$inventory = Get-Content -LiteralPath $inventoryPath -Raw | ConvertFrom-Json
if ($inventory.schemaVersion -ne 1 -or $inventory.policy -ne 'synthetic-only') {
    throw "Fixture privacy inventory identity is invalid"
}

$covered = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($entry in @($inventory.fixtures)) {
    if ($entry.classification -ne 'synthetic' -or $entry.containsPrivateData -ne $false) {
        throw "Fixture inventory includes a non-synthetic or private entry"
    }
    $full = Resolve-ProjectFixturePath ([string]$entry.path)
    if (-not $covered.Add($full)) { throw "Fixture inventory contains a duplicate path" }
    if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
        throw "Fixture inventory references a missing file"
    }
    $item = Get-Item -LiteralPath $full -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Fixture file cannot be a reparse point"
    }
    if ($item.Extension -in @('.json', '.toml', '.xml', '.eml', '.txt', '.md')) {
        $normalized = Get-NormalizedTextFixture -Path $full
        if ($normalized.bytes -ne [long]$entry.bytes) { throw "Fixture byte count mismatch: $($entry.path)" }
        if ($normalized.sha256 -ne $entry.sha256) { throw "Fixture hash mismatch: $($entry.path)" }
        $content = $normalized.text
        if (-not (Test-SyntheticMarker $content)) {
            throw "Text fixture lacks an explicit synthetic marker"
        }
    }
    else {
        if ($item.Length -ne [long]$entry.bytes) { throw "Fixture byte count mismatch: $($entry.path)" }
        if ((Get-FileHash -LiteralPath $full -Algorithm SHA256).Hash -ne $entry.sha256) {
            throw "Fixture hash mismatch: $($entry.path)"
        }
    }
}

$inventoryFull = [IO.Path]::GetFullPath($inventoryPath)
$uncovered = @(
    Get-ChildItem -LiteralPath $fixtureRoot -Recurse -File |
        Where-Object { $_.FullName -ne $inventoryFull -and -not $covered.Contains($_.FullName) }
)
if ($uncovered.Count -ne 0) {
    throw "Fixture files exist without privacy inventory coverage"
}

Write-Output "Private fixture scan passed: $($covered.Count) synthetic files are hash-locked."
