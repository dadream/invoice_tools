[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Email,

    [Parameter(Mandatory = $true)]
    [string]$AuthorizationPhrase,

    [string]$EvidencePath = "artifacts/real-qq-readonly-validation.txt"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$expectedAuthorization = "授权执行真实 QQ 邮箱只读验证：邮箱 879***187@qq.com，范围 [2026-06-01, 2026-07-01)，仅 IMAP 读取，并核对测试前后 FLAGS 不变。"
$expectedMask = "879***187@qq.com"
$since = "2026-06-01"
$before = "2026-07-01"
$repoRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$artifactsRoot = [System.IO.Path]::GetFullPath((Join-Path $repoRoot "artifacts"))
$envPath = Join-Path $repoRoot ".env.local"
$cargo = "C:\Users\slzheng6\.cargo\bin\cargo.exe"

function Get-MaskedEmail {
    param([Parameter(Mandatory = $true)][string]$Value)

    $parts = $Value.Split('@')
    if ($parts.Count -ne 2 -or $parts[0].Length -lt 6 -or [string]::IsNullOrWhiteSpace($parts[1])) {
        throw "测试邮箱格式无效"
    }
    $local = $parts[0]
    return "{0}***{1}@{2}" -f $local.Substring(0, 3), $local.Substring($local.Length - 3), $parts[1]
}

if ($AuthorizationPhrase -cne $expectedAuthorization) {
    throw "缺少本次运行所需的精确只读授权语句"
}
if ((Get-MaskedEmail -Value $Email) -cne $expectedMask) {
    throw "测试邮箱与本次只读授权不匹配"
}
if (-not (Test-Path -LiteralPath $envPath -PathType Leaf)) {
    throw ".env.local 不存在"
}
if (-not (Test-Path -LiteralPath $cargo -PathType Leaf)) {
    throw "未找到受控 Cargo 可执行文件"
}

$resolvedEvidence = if ([System.IO.Path]::IsPathRooted($EvidencePath)) {
    [System.IO.Path]::GetFullPath($EvidencePath)
} else {
    [System.IO.Path]::GetFullPath((Join-Path $repoRoot $EvidencePath))
}
$artifactPrefix = $artifactsRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
if (-not $resolvedEvidence.StartsWith($artifactPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "脱敏证据只能写入仓库 artifacts 目录"
}
if ((Test-Path -LiteralPath $artifactsRoot) -and
    ((Get-Item -LiteralPath $artifactsRoot -Force).Attributes -band [System.IO.FileAttributes]::ReparsePoint)) {
    throw "artifacts 目录不能是重解析点"
}

# 编译阶段不加载秘密，避免授权码进入构建进程或编译日志。
& $cargo build -p invoice-collect --locked
if ($LASTEXITCODE -ne 0) {
    throw "invoice-collect 构建失败"
}
$binary = Join-Path $repoRoot "target\debug\invoice-collect.exe"
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "未找到 invoice-collect 只读验证程序"
}

$matches = @(
    Get-Content -LiteralPath $envPath |
        Where-Object { $_ -match '^\s*INVOICE_IMAP_PASSWORD\s*=' }
)
if ($matches.Count -ne 1) {
    throw ".env.local 必须且只能定义一次 INVOICE_IMAP_PASSWORD"
}
$secret = ($matches[0] -split '=', 2)[1].Trim()
if (($secret.StartsWith('"') -and $secret.EndsWith('"')) -or
    ($secret.StartsWith("'") -and $secret.EndsWith("'"))) {
    $secret = $secret.Substring(1, $secret.Length - 2)
}
if ([string]::IsNullOrWhiteSpace($secret) -or $secret.Length -ne 16) {
    throw "QQ 邮箱授权码缺失或格式无效"
}

$combinedOutput = ""
$exitCode = -1
$env:INVOICE_IMAP_PASSWORD = $secret
try {
    $combinedOutput = (& $binary verify $Email $since $before 2>&1 | Out-String)
    $exitCode = $LASTEXITCODE
} finally {
    Remove-Item Env:INVOICE_IMAP_PASSWORD -ErrorAction SilentlyContinue
}

try {
    if ($combinedOutput.Contains($secret, [System.StringComparison]::Ordinal)) {
        throw "验证输出包含授权码，已拒绝保存"
    }
    if ($combinedOutput.Contains($Email, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "验证输出包含完整邮箱，已拒绝保存"
    }
    if ($exitCode -ne 0) {
        throw "真实 QQ 邮箱只读验证失败；未生成发布证据。运行输出：`n$combinedOutput"
    }

    $allowedLines = @(
        $combinedOutput -split "`r?`n" |
            Where-Object {
                $_ -match '^(verification|account|range|emails_scanned|emails_with_attachments|attachments_seen|invoice_candidates|duplicates|fetch_failures|parse_failures|mailbox_flags_sha256|message_content_set_sha256|read_only_unchanged)='
            }
    )
    if (($allowedLines -notcontains "read_only_unchanged=true") -or
        ($allowedLines -notcontains "range=[$since, $before)") -or
        ($allowedLines -notcontains "account=$expectedMask")) {
        throw "只读结果缺少范围、账号掩码或邮箱不变证明"
    }

    $evidenceParent = Split-Path -Parent $resolvedEvidence
    New-Item -ItemType Directory -Path $evidenceParent -Force | Out-Null
    [System.IO.File]::WriteAllLines($resolvedEvidence, $allowedLines, [System.Text.UTF8Encoding]::new($false))
    $allowedLines | ForEach-Object { Write-Output $_ }
    Write-Output "evidence=$resolvedEvidence"
} finally {
    $secret = $null
    $matches = $null
    $combinedOutput = $null
}
