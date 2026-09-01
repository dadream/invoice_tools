[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$failures = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)

Push-Location $projectRoot
try {
    $trackedSensitive = @(& git -c core.quotepath=false ls-files -- ".env" ".env.local" "*.pfx" "*.p12" "*.pem" "*.key")
    if ($LASTEXITCODE -ne 0) {
        throw "git ls-files failed"
    }
    foreach ($path in $trackedSensitive) {
        if (-not [string]::IsNullOrWhiteSpace($path)) {
            [void]$failures.Add($path)
        }
    }

    & git check-ignore --quiet --no-index ".env.local"
    if ($LASTEXITCODE -ne 0) {
        throw ".env.local is not protected by .gitignore"
    }

    $paths = @(& git -c core.quotepath=false ls-files --cached --others --exclude-standard)
    if ($LASTEXITCODE -ne 0) {
        throw "git ls-files failed"
    }

    $textExtensions = [System.Collections.Generic.HashSet[string]]::new(
        [string[]]@(
            ".rs", ".toml", ".json", ".md", ".txt", ".ps1", ".ts", ".js", ".svelte",
            ".css", ".html", ".xml", ".yml", ".yaml", ".lock", ".gitignore"
        ),
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $patterns = @(
        [regex]::new("-----BEGIN " + "(?:RSA |EC |OPENSSH )?PRIVATE KEY-----"),
        [regex]::new("g" + "hp_[A-Za-z0-9]{30,}"),
        [regex]::new("github_pat_" + "[A-Za-z0-9_]{40,}"),
        [regex]::new("A" + "KIA[0-9A-Z]{16}"),
        [regex]::new("sk_live_" + "[A-Za-z0-9]{20,}"),
        [regex]::new("xox[baprs]-" + "[A-Za-z0-9-]{20,}")
    )
    $envAssignment = [regex]::new(
        '(?im)^\s*(?:export\s+)?[A-Z][A-Z0-9_]*(?:PASSWORD|TOKEN|SECRET|API_KEY|AUTH_CODE)[A-Z0-9_]*\s*=\s*["'']?([^\s"'']{8,})'
    )
    $placeholder = [regex]::new('(?i)^(?:x+|test|fake|dummy|example|placeholder|replace-me|your-.+|<.+>|\$\{.+\})$')

    foreach ($relativePath in $paths) {
        if ([string]::IsNullOrWhiteSpace($relativePath)) {
            continue
        }
        $fullPath = Join-Path $projectRoot $relativePath
        if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
            continue
        }
        $item = Get-Item -LiteralPath $fullPath
        if ($item.Length -gt 5MB -or -not $textExtensions.Contains($item.Extension)) {
            continue
        }

        $content = [System.IO.File]::ReadAllText($fullPath)
        $matched = $false
        foreach ($pattern in $patterns) {
            if ($pattern.IsMatch($content)) {
                $matched = $true
                break
            }
        }
        if (-not $matched) {
            foreach ($match in $envAssignment.Matches($content)) {
                if (-not $placeholder.IsMatch($match.Groups[1].Value)) {
                    $matched = $true
                    break
                }
            }
        }
        if ($matched) {
            [void]$failures.Add($relativePath)
        }
    }
}
finally {
    Pop-Location
}

if ($failures.Count -gt 0) {
    [Console]::Error.WriteLine("Secret scan failed in the following files (matched values are intentionally hidden):")
    foreach ($failure in @($failures | Sort-Object)) {
        [Console]::Error.WriteLine(" - $failure")
    }
    exit 1
}

Write-Output "Secret scan passed; no tracked or unignored candidate files matched."
