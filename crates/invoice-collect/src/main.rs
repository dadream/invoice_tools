use anyhow::{bail, Context};
use invoice_collect::classify::classify_attachment;
use invoice_collect::config::{DateRange, ImapConfig};
use invoice_collect::dedupe::Deduper;
use invoice_collect::extract::{extract_email, extract_zip_if_needed};
use invoice_collect::imap_client::Session;
use invoice_collect::manifest_gen::{render, ManifestEntry};
use invoice_collect::store::save_sample;
use std::path::Path;

const DEFAULT_SINCE: &str = "2026-06-01";
const DEFAULT_BEFORE: &str = "2026-07-01";

const USAGE: &str = "用法:
  invoice-collect probe   <邮箱地址> [起始日期 结束日期]
  invoice-collect collect <邮箱地址> [起始日期 结束日期]
  invoice-collect audit   <邮箱地址> [起始日期 结束日期]

日期格式 YYYY-MM-DD，默认 2026-06-01 至 2026-07-01（半开区间）。
密码从环境变量 INVOICE_IMAP_PASSWORD 读取。QQ 邮箱需填 16 位授权码。";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("probe") => probe(&parse_target(&args)?),
        Some("collect") => collect(&parse_target(&args)?),
        Some("audit") => audit(&parse_target(&args)?),
        Some(other) => bail!("未知子命令: {other}\n\n{USAGE}"),
        None => {
            eprintln!("{USAGE}");
            Ok(())
        }
    }
}

/// 邮箱地址与日期范围都由命令行给出 —— 账号不硬编码进源码。
struct Target {
    username: String,
    range: DateRange,
}

fn parse_target(args: &[String]) -> anyhow::Result<Target> {
    let username = args
        .get(2)
        .with_context(|| format!("缺少邮箱地址参数\n\n{USAGE}"))?
        .clone();

    let since = args.get(3).map(String::as_str).unwrap_or(DEFAULT_SINCE);
    let before = args.get(4).map(String::as_str).unwrap_or(DEFAULT_BEFORE);

    Ok(Target {
        username,
        range: DateRange::parse(since, before)?,
    })
}

fn probe(target: &Target) -> anyhow::Result<()> {
    let cfg = ImapConfig::from_env(&target.username)?;
    let range = target.range.clone();

    println!("连接 {}:{} 账号 {}", cfg.host, cfg.port, cfg.username);
    let mut session = Session::connect(&cfg).context("建立 IMAP 会话失败")?;
    println!("登录成功\n");

    println!("可用文件夹：");
    for name in session.list_folders()? {
        println!("  {name}");
    }

    let uids = session.search_range("INBOX", &range)?;
    println!(
        "\nINBOX 中 {} 至 {} 共 {} 封邮件",
        range.since, range.before, uids.len()
    );

    if uids.is_empty() {
        println!("\n该范围内没有邮件。需要调整日期范围或更换样本来源。");
        return Ok(());
    }

    println!("\n{:<8} {:<18} {:<32} 附件  主题", "UID", "收件时间", "发件人");
    for s in session.fetch_summaries(&uids)? {
        let flag = if s.has_attachments { "有" } else { "无" };
        let subject: String = s.subject.chars().take(40).collect();
        println!(
            "{:<8} {:<18} {:<32} {:<4}  {}",
            s.uid, s.internal_date, s.from, flag, subject
        );
    }
    Ok(())
}

fn collect(target: &Target) -> anyhow::Result<()> {
    let cfg = ImapConfig::from_env(&target.username)?;
    let range = target.range.clone();
    let fixtures_root = Path::new("fixtures");

    let mut session = Session::connect(&cfg)?;
    let uids = session.search_range("INBOX", &range)?;
    println!("范围内 {} 封邮件，开始逐封处理\n", uids.len());

    let mut deduper = Deduper::new();
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let mut stats = Stats::default();

    for uid in &uids {
        stats.emails_scanned += 1;

        let raw = match session.fetch_raw(*uid) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  UID {uid} 拉取失败，跳过: {e}");
                stats.fetch_failures += 1;
                continue;
            }
        };

        let mut email = match extract_email(&raw) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("  UID {uid} MIME 解析失败，跳过: {e}");
                stats.parse_failures += 1;
                continue;
            }
        };

        if email.attachments.is_empty() {
            continue;
        }
        stats.emails_with_attachments += 1;

        // Extract ZIP files if needed
        let mut expanded_attachments = Vec::new();
        for att in &email.attachments {
            expanded_attachments.extend(extract_zip_if_needed(att));
        }
        email.attachments = expanded_attachments;

        for att in &email.attachments {
            stats.attachments_seen += 1;

            let Some(cls) = classify_attachment(&email, att) else {
                stats.not_invoice += 1;
                continue;
            };

            if !deduper.is_new(email.message_id.as_deref(), &att.data) {
                stats.duplicates += 1;
                continue;
            }

            let seq = entries.len() + 1;
            let saved = save_sample(fixtures_root, seq, &cls, att)?;
            println!(
                "  [{:>2}] {:<10} {:<12} {} 字节  ← {}",
                seq,
                cls.format.as_manifest_str(),
                cls.platform,
                saved.byte_len,
                att.filename
            );

            entries.push(ManifestEntry {
                saved,
                format: cls.format.as_manifest_str().to_string(),
                platform: cls.platform.clone(),
                original_filename: att.filename.clone(),
                subject: email.subject.clone(),
            });
        }
    }

    let manifest_path = fixtures_root.join("manifest.toml");
    std::fs::create_dir_all(fixtures_root)?;
    std::fs::write(&manifest_path, render(&entries))?;

    stats.print(entries.len());
    print_format_breakdown(&entries);
    println!("\n清单骨架已写入 {}", manifest_path.display());
    println!("下一步：打开每个样本文件，把期望值填进清单。");

    Ok(())
}

#[derive(Default)]
struct Stats {
    emails_scanned: usize,
    emails_with_attachments: usize,
    attachments_seen: usize,
    not_invoice: usize,
    duplicates: usize,
    fetch_failures: usize,
    parse_failures: usize,
}

impl Stats {
    fn print(&self, saved: usize) {
        println!("\n─── 采集统计 ───");
        println!("扫描邮件          {}", self.emails_scanned);
        println!("其中含附件        {}", self.emails_with_attachments);
        println!("附件总数          {}", self.attachments_seen);
        println!("判定为非发票      {}", self.not_invoice);
        println!("重复丢弃          {}", self.duplicates);
        println!("拉取失败          {}", self.fetch_failures);
        println!("解析失败          {}", self.parse_failures);
        println!("最终落盘          {saved}");
    }
}

fn print_format_breakdown(entries: &[ManifestEntry]) {
    use std::collections::BTreeMap;
    let mut by_format: BTreeMap<&str, usize> = BTreeMap::new();
    for e in entries {
        *by_format.entry(e.format.as_str()).or_insert(0) += 1;
    }

    println!("\n─── 格式分布 vs 解析验证计划的需求 ───");
    let needed: &[(&str, usize)] = &[
        ("xml", 5),
        ("ofd", 5),
        ("pdf-rail", 3),
        ("pdf-flight", 3),
        ("pdf-vat", 3),
        ("image", 10),
    ];
    for (format, need) in needed {
        let got = by_format.get(format).copied().unwrap_or(0);
        let mark = if got >= *need { "✓" } else { "缺" };
        println!("  {mark} {format:<12} {got}/{need}");
    }
}

fn audit(target: &Target) -> anyhow::Result<()> {
    let cfg = ImapConfig::from_env(&target.username)?;
    let range = target.range.clone();

    eprintln!("连接 {}:{} 账号 {}", cfg.host, cfg.port, cfg.username);
    let mut session = Session::connect(&cfg)?;
    let uids = session.search_range("INBOX", &range)?;
    eprintln!("范围内 {} 封邮件，开始审计\n", uids.len());

    // Print TSV header
    println!("UID\tDate\tFrom\tSubject\tFilename\tContentType\tByteLen\tPlatform\tFormat\tReason\tWouldSave\tNotes");

    for uid in &uids {
        let raw = match session.fetch_raw(*uid) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  UID {uid} 拉取失败: {e}");
                // Log error as a TSV row
                println!(
                    "{}\t\t\t\tFETCH_ERROR\t\t\t\t\t\tNO\t{}",
                    uid,
                    escape_tsv(&format!("fetch_failed: {e}"))
                );
                continue;
            }
        };

        let mut email = match extract_email(&raw) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("  UID {uid} MIME 解析失败: {e}");
                // Log parse error as a TSV row
                println!(
                    "{}\t\t\t\tPARSE_ERROR\t\t\t\t\t\tNO\t{}",
                    uid,
                    escape_tsv(&format!("parse_failed: {e}"))
                );
                continue;
            }
        };

        let date = "(from_envelope)";
        let from = escape_tsv(&email.from);
        let subject = escape_tsv(&email.subject);

        if email.attachments.is_empty() {
            println!(
                "{}\t{}\t{}\t{}\tNO_ATTACHMENTS\t\t\t\t\t\tSKIP_NO_ATTACH\t",
                uid, date, from, subject
            );
            continue;
        }

        // Extract ZIP files if needed
        let mut expanded_attachments = Vec::new();
        for att in &email.attachments {
            expanded_attachments.extend(extract_zip_if_needed(att));
        }
        email.attachments = expanded_attachments;

        for att in &email.attachments {
            let filename = escape_tsv(&att.filename);
            let content_type = escape_tsv(&att.content_type);
            let byte_len = att.data.len();

            match classify_attachment(&email, att) {
                Some(cls) => {
                    let platform = escape_tsv(&cls.platform);
                    let format = cls.format.as_manifest_str();
                    let reason = match cls.reason {
                        invoice_collect::classify::MatchReason::SenderWhitelist => {
                            "sender_whitelist_match"
                        }
                        invoice_collect::classify::MatchReason::AttachmentFeature => {
                            "keyword_match"
                        }
                    };
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\tYES\t{}",
                        uid, date, from, subject, filename, content_type, byte_len, platform, format, reason, reason
                    );
                }
                None => {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t\t\t\tNO\trejected_no_invoice_features",
                        uid, date, from, subject, filename, content_type, byte_len
                    );
                }
            }
        }
    }

    eprintln!("\n审计完成");
    Ok(())
}

/// Escape tabs and newlines in TSV fields
fn escape_tsv(s: &str) -> String {
    s.replace('\t', " ")
        .replace('\n', " ")
        .replace('\r', "")
}
