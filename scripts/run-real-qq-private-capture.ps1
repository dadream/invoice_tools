[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Email,

    [Parameter(Mandatory = $true)]
    [string]$AuthorizationPhrase,

    [string]$PrivateRoot,

    [string]$EvidencePath = "artifacts/real-qq-private-capture-summary.txt"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$expectedAuthorization = "授权执行真实 QQ 邮箱只读验证：邮箱 879***187@qq.com，范围 [2026-06-01, 2026-07-01)，仅 IMAP 读取，并核对测试前后 FLAGS 不变。"
$expectedMask = "879***187@qq.com"
$since = "2026-06-01"
$before = "2026-07-01"
$repoRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$workspaceRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $repoRoot))
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

function Assert-PlainDirectory {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (Test-Path -LiteralPath $Path) {
        $item = Get-Item -LiteralPath $Path -Force
        if (-not $item.PSIsContainer) {
            throw "路径不是目录: $Path"
        }
        if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
            throw "隔离目录不能是重解析点: $Path"
        }
    }
}

function Get-Stat {
    param(
        [Parameter(Mandatory = $true)][string]$Text,
        [Parameter(Mandatory = $true)][string]$Label
    )

    $match = [regex]::Match($Text, "(?m)^\s*" + [regex]::Escape($Label) + "\s+(\d+)\s*$")
    if (-not $match.Success) {
        throw "采集输出缺少统计字段: $Label"
    }
    return [int]$match.Groups[1].Value
}

function Get-KeyStat {
    param(
        [Parameter(Mandatory = $true)][string]$Text,
        [Parameter(Mandatory = $true)][string]$Key
    )

    $match = [regex]::Match($Text, "(?m)^" + [regex]::Escape($Key) + "=(\d+)\s*$")
    if (-not $match.Success) {
        throw "全量附件捕获输出缺少统计字段: $Key"
    }
    return [int]$match.Groups[1].Value
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

$privateBase = if ([string]::IsNullOrWhiteSpace($PrivateRoot)) {
    [System.IO.Path]::GetFullPath((Join-Path $workspaceRoot ".invoice-tools-private-validation"))
} elseif ([System.IO.Path]::IsPathRooted($PrivateRoot)) {
    [System.IO.Path]::GetFullPath($PrivateRoot)
} else {
    [System.IO.Path]::GetFullPath((Join-Path $workspaceRoot $PrivateRoot))
}
$workspacePrefix = $workspaceRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
$repoPrefix = $repoRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
if (-not $privateBase.StartsWith($workspacePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "真实样本隔离目录必须位于当前工作区内"
}
if ($privateBase.StartsWith($repoPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "真实样本隔离目录不能位于 Git 仓库内"
}
Assert-PlainDirectory -Path $workspaceRoot
Assert-PlainDirectory -Path $privateBase

$resolvedEvidence = if ([System.IO.Path]::IsPathRooted($EvidencePath)) {
    [System.IO.Path]::GetFullPath($EvidencePath)
} else {
    [System.IO.Path]::GetFullPath((Join-Path $repoRoot $EvidencePath))
}
$artifactPrefix = $artifactsRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
if (-not $resolvedEvidence.StartsWith($artifactPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "脱敏证据只能写入仓库 artifacts 目录"
}
Assert-PlainDirectory -Path $artifactsRoot

# 编译阶段不加载秘密，避免授权码进入 Cargo 子进程或构建日志。
& $cargo build -p invoice-collect --locked
if ($LASTEXITCODE -ne 0) {
    throw "invoice-collect 构建失败"
}
$binary = Join-Path $repoRoot "target\debug\invoice-collect.exe"
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "未找到 invoice-collect 采集程序"
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

$runName = "qq-2026-06-{0}-{1}" -f (Get-Date).ToUniversalTime().ToString("yyyyMMdd-HHmmss"), ([Guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Path $privateBase -Force | Out-Null
Assert-PlainDirectory -Path $privateBase
$runRoot = [System.IO.Path]::GetFullPath((Join-Path $privateBase $runName))
New-Item -ItemType Directory -Path $runRoot | Out-Null
Assert-PlainDirectory -Path $runRoot

$auditOutput = ""
$collectOutput = ""
$allOutput = ""
$allRoot = Join-Path $runRoot "all-attachments"
$env:INVOICE_IMAP_PASSWORD = $secret
$env:INVOICE_PRIVATE_CAPTURE_ACK = "authorized-readonly-private-capture-v1"
try {
    Push-Location -LiteralPath $runRoot
    try {
        $auditOutput = (& $binary audit $Email $since $before 2>&1 | Out-String)
        if ($LASTEXITCODE -ne 0) {
            throw "真实 QQ 邮箱私有审计失败"
        }
        $collectOutput = (& $binary collect $Email $since $before 2>&1 | Out-String)
        if ($LASTEXITCODE -ne 0) {
            throw "真实 QQ 邮箱隔离采集失败"
        }
        $allOutput = (& $binary capture-private $Email $since $before $allRoot 2>&1 | Out-String)
        if ($LASTEXITCODE -ne 0) {
            throw "真实 QQ 邮箱全量附件私有捕获失败"
        }
    } finally {
        Pop-Location
    }
} finally {
    Remove-Item Env:INVOICE_IMAP_PASSWORD -ErrorAction SilentlyContinue
    Remove-Item Env:INVOICE_PRIVATE_CAPTURE_ACK -ErrorAction SilentlyContinue
}

try {
    if ($auditOutput.Contains($secret, [System.StringComparison]::Ordinal) -or
        $collectOutput.Contains($secret, [System.StringComparison]::Ordinal) -or
        $allOutput.Contains($secret, [System.StringComparison]::Ordinal)) {
        throw "程序输出包含授权码，已拒绝保存"
    }
    if ($allOutput.Contains($Email, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "全量附件捕获输出包含完整邮箱，已拒绝保存"
    }
    if ($auditOutput -notmatch '(?m)^UID\tDate\tFrom\tSubject\tFilename\t') {
        throw "私有审计输出缺少 TSV 表头"
    }
    if ($auditOutput -notmatch '审计完成') {
        throw "私有审计未完成只读复核"
    }

    $fixturesRoot = Join-Path $runRoot "fixtures"
    $manifestPath = Join-Path $fixturesRoot "manifest.toml"
    $samplesRoot = Join-Path $fixturesRoot "samples"
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "隔离采集未生成私有样本清单"
    }
    Assert-PlainDirectory -Path $fixturesRoot
    Assert-PlainDirectory -Path $samplesRoot
    if ((Get-Content -LiteralPath $manifestPath -Raw).Contains($secret, [System.StringComparison]::Ordinal)) {
        throw "私有样本清单意外包含授权码"
    }

    $sampleFiles = @(Get-ChildItem -LiteralPath $samplesRoot -File)
    if ($sampleFiles.Count -eq 0 -or @($sampleFiles | Where-Object Length -eq 0).Count -ne 0) {
        throw "隔离采集样本为空或含零字节文件"
    }
    $saved = Get-Stat -Text $collectOutput -Label "最终落盘"
    if ($saved -ne $sampleFiles.Count) {
        throw "采集统计与实际文件数量不一致"
    }

    Assert-PlainDirectory -Path $allRoot
    $emailsRoot = Join-Path $allRoot "emails"
    $mimeRoot = Join-Path $allRoot "mime-attachments"
    $expandedRoot = Join-Path $allRoot "expanded-attachments"
    Assert-PlainDirectory -Path $emailsRoot
    Assert-PlainDirectory -Path $mimeRoot
    Assert-PlainDirectory -Path $expandedRoot
    $emailFiles = @(Get-ChildItem -LiteralPath $emailsRoot -File)
    $mimeFiles = @(Get-ChildItem -LiteralPath $mimeRoot -File)
    $expandedFiles = @(Get-ChildItem -LiteralPath $expandedRoot -File)
    if ($emailFiles.Count -ne (Get-KeyStat -Text $allOutput -Key "emails_saved") -or
        $mimeFiles.Count -ne (Get-KeyStat -Text $allOutput -Key "named_mime_attachments") -or
        $expandedFiles.Count -ne (Get-KeyStat -Text $allOutput -Key "expanded_attachments")) {
        throw "全量附件捕获统计与实际文件数量不一致"
    }
    $emptyExpanded = @($expandedFiles | Where-Object Length -eq 0).Count

    [System.IO.File]::WriteAllText(
        (Join-Path $runRoot "attachment-audit.private.tsv"),
        $auditOutput,
        [System.Text.UTF8Encoding]::new($false)
    )
    [System.IO.File]::WriteAllText(
        (Join-Path $runRoot "collect.private.log"),
        $collectOutput,
        [System.Text.UTF8Encoding]::new($false)
    )
    [System.IO.File]::WriteAllText(
        (Join-Path $allRoot "capture.private.log"),
        $allOutput,
        [System.Text.UTF8Encoding]::new($false)
    )

    $summary = @(
        "verification=readonly-private-capture-v1"
        "account=$expectedMask"
        "range=[$since, $before)"
        "emails_scanned=$(Get-Stat -Text $collectOutput -Label '扫描邮件')"
        "emails_with_attachments=$(Get-Stat -Text $collectOutput -Label '其中含附件')"
        "attachments_seen=$(Get-Stat -Text $collectOutput -Label '附件总数')"
        "rejected_by_classifier=$(Get-Stat -Text $collectOutput -Label '判定为非发票')"
        "duplicates=$(Get-Stat -Text $collectOutput -Label '重复丢弃')"
        "fetch_failures=$(Get-Stat -Text $collectOutput -Label '拉取失败')"
        "mime_parse_failures=$(Get-Stat -Text $collectOutput -Label '解析失败')"
        "candidate_files_saved=$saved"
        "empty_files=0"
        "named_mime_attachments=$(Get-KeyStat -Text $allOutput -Key 'named_mime_attachments')"
        "expanded_attachments=$(Get-KeyStat -Text $allOutput -Key 'expanded_attachments')"
        "classifier_positive=$(Get-KeyStat -Text $allOutput -Key 'classifier_positive')"
        "classifier_negative=$(Get-KeyStat -Text $allOutput -Key 'classifier_negative')"
        "all_attachment_duplicates=$(Get-KeyStat -Text $allOutput -Key 'duplicate_attachments')"
        "all_attachment_empty_files=$emptyExpanded"
        "read_only_unchanged=true"
    )
    $evidenceParent = Split-Path -Parent $resolvedEvidence
    New-Item -ItemType Directory -Path $evidenceParent -Force | Out-Null
    [System.IO.File]::WriteAllLines($resolvedEvidence, $summary, [System.Text.UTF8Encoding]::new($false))
    $summary | ForEach-Object { Write-Output $_ }
    Write-Output "private_capture_root=$runRoot"
    Write-Output "evidence=$resolvedEvidence"
} finally {
    $secret = $null
    $matches = $null
    $auditOutput = $null
    $collectOutput = $null
    $allOutput = $null
}
