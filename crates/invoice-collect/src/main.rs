use anyhow::{bail, Context};
use invoice_collect::classify::classify_attachment;
use invoice_collect::config::{DateRange, ImapConfig};
use invoice_collect::dedupe::Deduper;
use invoice_collect::extract::{extract_email, extract_zip_if_needed};
use invoice_collect::imap_client::Session;
use invoice_collect::manifest_gen::{render, ManifestEntry};
use invoice_collect::store::save_sample;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

const DEFAULT_SINCE: &str = "2026-06-01";
const DEFAULT_BEFORE: &str = "2026-07-01";

const USAGE: &str = "用法:
  invoice-collect probe   <邮箱地址> [起始日期 结束日期]
  invoice-collect collect <邮箱地址> [起始日期 结束日期]
  invoice-collect audit   <邮箱地址> [起始日期 结束日期]
  invoice-collect verify  <邮箱地址> [起始日期 结束日期]
  invoice-collect capture-private <邮箱地址> <起始日期> <结束日期> <绝对隔离目录>

日期格式 YYYY-MM-DD，默认 2026-06-01 至 2026-07-01（半开区间）。
密码从环境变量 INVOICE_IMAP_PASSWORD 读取。QQ 邮箱需填 16 位授权码。";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("probe") => probe(&parse_target(&args)?),
        Some("collect") => collect(&parse_target(&args)?),
        Some("audit") => audit(&parse_target(&args)?),
        Some("verify") => verify_read_only(&parse_target(&args)?),
        Some("capture-private") => {
            let output_root = args
                .get(5)
                .with_context(|| format!("缺少私有隔离目录参数\n\n{USAGE}"))?;
            capture_private(&parse_target(&args)?, Path::new(output_root))
        }
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
        range.since,
        range.before,
        uids.len()
    );

    if uids.is_empty() {
        println!("\n该范围内没有邮件。需要调整日期范围或更换样本来源。");
        return Ok(());
    }

    println!(
        "\n{:<8} {:<18} {:<32} 附件  主题",
        "UID", "收件时间", "发件人"
    );
    for s in session.fetch_summaries(&uids)? {
        let flag = if s.has_attachments { "有" } else { "无" };
        let subject: String = s.subject.chars().take(40).collect();
        println!(
            "{:<8} {:<18} {:<32} {:<4}  {}",
            s.uid, s.internal_date, s.from, flag, subject
        );
    }
    session.verify_read_only_unchanged("INBOX")?;
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
    session.verify_read_only_unchanged("INBOX")?;

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
                        invoice_collect::classify::MatchReason::SupportedDocumentContent => {
                            "supported_document_content"
                        }
                    };
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\tYES\t{}",
                        uid,
                        date,
                        from,
                        subject,
                        filename,
                        content_type,
                        byte_len,
                        platform,
                        format,
                        reason,
                        reason
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
    session.verify_read_only_unchanged("INBOX")?;

    eprintln!("\n审计完成");
    Ok(())
}

/// Escape tabs and newlines in TSV fields
fn verify_read_only(target: &Target) -> anyhow::Result<()> {
    let cfg = ImapConfig::from_env(&target.username)?;
    let mut session = Session::connect(&cfg).context("建立只读 IMAP 会话失败")?;
    let uids = session.search_range("INBOX", &target.range)?;
    let mut stats = Stats::default();
    let mut invoice_candidates = 0usize;
    let mut content_set = Sha256::new();
    let mut deduper = Deduper::new();

    for uid in &uids {
        stats.emails_scanned += 1;
        let raw = match session.fetch_raw(*uid) {
            Ok(value) => value,
            Err(_) => {
                stats.fetch_failures += 1;
                continue;
            }
        };
        content_set.update(Sha256::digest(&raw));
        let mut email = match extract_email(&raw) {
            Ok(value) => value,
            Err(_) => {
                stats.parse_failures += 1;
                continue;
            }
        };
        if !email.attachments.is_empty() {
            stats.emails_with_attachments += 1;
        }
        let mut expanded = Vec::new();
        for attachment in &email.attachments {
            expanded.extend(extract_zip_if_needed(attachment));
        }
        email.attachments = expanded;
        for attachment in &email.attachments {
            stats.attachments_seen += 1;
            if classify_attachment(&email, attachment).is_none() {
                stats.not_invoice += 1;
                continue;
            }
            if !deduper.is_new(email.message_id.as_deref(), &attachment.data) {
                stats.duplicates += 1;
                continue;
            }
            invoice_candidates += 1;
        }
    }

    let mailbox_fingerprint = session.verify_read_only_unchanged("INBOX")?;
    println!("verification=readonly-imap-v1");
    println!("account={}", mask_email(&target.username));
    println!("range=[{}, {})", target.range.since, target.range.before);
    println!("emails_scanned={}", stats.emails_scanned);
    println!("emails_with_attachments={}", stats.emails_with_attachments);
    println!("attachments_seen={}", stats.attachments_seen);
    println!("invoice_candidates={invoice_candidates}");
    println!("duplicates={}", stats.duplicates);
    println!("fetch_failures={}", stats.fetch_failures);
    println!("parse_failures={}", stats.parse_failures);
    println!("mailbox_flags_sha256={mailbox_fingerprint}");
    println!("message_content_set_sha256={:x}", content_set.finalize());
    println!("read_only_unchanged=true");
    Ok(())
}

fn capture_private(target: &Target, output_root: &Path) -> anyhow::Result<()> {
    const ACK: &str = "authorized-readonly-private-capture-v1";
    if std::env::var("INVOICE_PRIVATE_CAPTURE_ACK").as_deref() != Ok(ACK) {
        bail!("缺少私有只读捕获确认变量");
    }
    if !output_root.is_absolute() {
        bail!("私有隔离目录必须是绝对路径");
    }
    if output_root.exists() {
        bail!("私有隔离目录已存在，拒绝覆盖");
    }
    let output_parent = output_root
        .parent()
        .context("私有隔离目录缺少父目录")?
        .canonicalize()
        .context("私有隔离目录父路径不存在或不可访问")?;
    let requested_root =
        output_parent.join(output_root.file_name().context("私有隔离目录缺少目录名")?);
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("无法定位 Git 仓库根目录")?
        .canonicalize()
        .context("Git 仓库根目录不可访问")?;
    if requested_root.starts_with(&repo_root) {
        bail!("真实邮件和附件不得写入 Git 仓库");
    }

    let emails_root = requested_root.join("emails");
    let mime_root = requested_root.join("mime-attachments");
    let expanded_root = requested_root.join("expanded-attachments");
    std::fs::create_dir_all(&emails_root)?;
    std::fs::create_dir_all(&mime_root)?;
    std::fs::create_dir_all(&expanded_root)?;

    let cfg = ImapConfig::from_env(&target.username)?;
    let mut session = Session::connect(&cfg).context("建立私有只读 IMAP 会话失败")?;
    let uids = session.search_range("INBOX", &target.range)?;
    let mut email_rows = vec![
        "email_file\tuid\tfrom\tsubject\tnamed_mime_attachments\texpanded_attachments\tpredicted_invoice_email"
            .to_string(),
    ];
    let mut attachment_rows = vec![
        "layer\tfile\temail_file\tuid\toriginal_filename\tcontent_type\tbyte_len\tsha256\tpredicted_invoice\tpredicted_format\tpredicted_platform\tmatch_reason\tduplicate_of"
            .to_string(),
    ];
    let mut seen_hashes = HashMap::<String, String>::new();
    let mut fetch_failures = 0usize;
    let mut mime_parse_failures = 0usize;
    let mut emails_saved = 0usize;
    let mut named_mime_attachments = 0usize;
    let mut expanded_attachments = 0usize;
    let mut classifier_positive = 0usize;
    let mut classifier_negative = 0usize;
    let mut duplicate_attachments = 0usize;

    for (email_index, uid) in uids.iter().enumerate() {
        let raw = match session.fetch_raw(*uid) {
            Ok(value) => value,
            Err(_) => {
                fetch_failures += 1;
                continue;
            }
        };
        let email_file = format!("email-{:03}-uid-{uid}.eml", email_index + 1);
        std::fs::write(emails_root.join(&email_file), &raw)?;
        emails_saved += 1;

        let email = match extract_email(&raw) {
            Ok(value) => value,
            Err(_) => {
                mime_parse_failures += 1;
                continue;
            }
        };
        let mut email_expanded = 0usize;
        let mut email_predicted = false;
        for (mime_index, attachment) in email.attachments.iter().enumerate() {
            named_mime_attachments += 1;
            let hash = invoice_collect::dedupe::sha256_hex(&attachment.data);
            let extension = private_extension(&attachment.filename, &attachment.content_type);
            let file = format!(
                "email-{:03}-mime-{:03}-{}.{}",
                email_index + 1,
                mime_index + 1,
                &hash[..8],
                extension
            );
            std::fs::write(mime_root.join(&file), &attachment.data)?;
            attachment_rows.push(render_private_attachment_row(
                "mime",
                &file,
                &email_file,
                *uid,
                attachment,
                &hash,
                None,
                None,
            ));

            for (expanded_index, item) in extract_zip_if_needed(attachment).iter().enumerate() {
                expanded_attachments += 1;
                email_expanded += 1;
                let hash = invoice_collect::dedupe::sha256_hex(&item.data);
                let extension = private_extension(&item.filename, &item.content_type);
                let file = format!(
                    "email-{:03}-expanded-{:03}-{:03}-{}.{}",
                    email_index + 1,
                    mime_index + 1,
                    expanded_index + 1,
                    &hash[..8],
                    extension
                );
                std::fs::write(expanded_root.join(&file), &item.data)?;
                let duplicate_of = seen_hashes.get(&hash).cloned();
                if duplicate_of.is_some() {
                    duplicate_attachments += 1;
                } else {
                    seen_hashes.insert(hash.clone(), file.clone());
                }
                let classification = classify_attachment(&email, item);
                if classification.is_some() {
                    classifier_positive += 1;
                    email_predicted = true;
                } else {
                    classifier_negative += 1;
                }
                attachment_rows.push(render_private_attachment_row(
                    "expanded",
                    &file,
                    &email_file,
                    *uid,
                    item,
                    &hash,
                    classification.as_ref(),
                    duplicate_of.as_deref(),
                ));
            }
        }
        email_rows.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            email_file,
            uid,
            escape_tsv(&email.from),
            escape_tsv(&email.subject),
            email.attachments.len(),
            email_expanded,
            email_predicted
        ));
    }

    session.verify_read_only_unchanged("INBOX")?;
    std::fs::write(
        requested_root.join("emails.private.tsv"),
        email_rows.join("\n"),
    )?;
    std::fs::write(
        requested_root.join("attachments.private.tsv"),
        attachment_rows.join("\n"),
    )?;
    println!("verification=readonly-private-all-attachments-v1");
    println!("account={}", mask_email(&target.username));
    println!("range=[{}, {})", target.range.since, target.range.before);
    println!("emails_scanned={}", uids.len());
    println!("emails_saved={emails_saved}");
    println!("named_mime_attachments={named_mime_attachments}");
    println!("expanded_attachments={expanded_attachments}");
    println!("classifier_positive={classifier_positive}");
    println!("classifier_negative={classifier_negative}");
    println!("duplicate_attachments={duplicate_attachments}");
    println!("fetch_failures={fetch_failures}");
    println!("mime_parse_failures={mime_parse_failures}");
    println!("read_only_unchanged=true");
    Ok(())
}

fn private_extension(filename: &str, content_type: &str) -> String {
    let candidate = Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 10
                && value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        });
    candidate.unwrap_or_else(|| match content_type.to_ascii_lowercase().as_str() {
        "application/pdf" => "pdf".to_string(),
        "application/ofd" => "ofd".to_string(),
        "application/xml" | "text/xml" => "xml".to_string(),
        "image/jpeg" => "jpg".to_string(),
        "image/png" => "png".to_string(),
        "application/zip" => "zip".to_string(),
        _ => "bin".to_string(),
    })
}

#[allow(clippy::too_many_arguments)]
fn render_private_attachment_row(
    layer: &str,
    file: &str,
    email_file: &str,
    uid: u32,
    attachment: &invoice_collect::extract::RawAttachment,
    hash: &str,
    classification: Option<&invoice_collect::classify::Classification>,
    duplicate_of: Option<&str>,
) -> String {
    let (predicted, format, platform, reason) = match classification {
        Some(value) => (
            true,
            value.format.as_manifest_str(),
            value.platform.as_str(),
            match value.reason {
                invoice_collect::classify::MatchReason::SenderWhitelist => "sender_whitelist",
                invoice_collect::classify::MatchReason::AttachmentFeature => "attachment_feature",
                invoice_collect::classify::MatchReason::SupportedDocumentContent => {
                    "supported_document_content"
                }
            },
        ),
        None => (false, "", "", ""),
    };
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        layer,
        file,
        email_file,
        uid,
        escape_tsv(&attachment.filename),
        escape_tsv(&attachment.content_type),
        attachment.data.len(),
        hash,
        predicted,
        format,
        platform,
        reason,
        duplicate_of.unwrap_or("")
    )
}

fn mask_email(email: &str) -> String {
    let Some((local, domain)) = email.split_once('@') else {
        return "***".to_string();
    };
    let chars = local.chars().collect::<Vec<_>>();
    if chars.len() < 6 {
        let visible = chars.first().copied().unwrap_or('*');
        return format!("{visible}***@{domain}");
    }

    let prefix = chars[..3].iter().collect::<String>();
    let suffix = chars[chars.len() - 3..].iter().collect::<String>();
    format!("{prefix}***{suffix}@{domain}")
}

fn escape_tsv(s: &str) -> String {
    s.replace(['\t', '\n'], " ").replace('\r', "")
}

#[cfg(test)]
mod tests {
    use super::{mask_email, private_extension};

    #[test]
    fn masks_long_account_with_authorized_prefix_and_suffix() {
        assert_eq!(
            mask_email("123456789@example.invalid"),
            "123***789@example.invalid"
        );
    }

    #[test]
    fn masks_short_and_invalid_accounts_conservatively() {
        assert_eq!(mask_email("abc@qq.com"), "a***@qq.com");
        assert_eq!(mask_email("invalid"), "***");
    }

    #[test]
    fn private_capture_extensions_are_safe_and_bounded() {
        assert_eq!(
            private_extension("invoice.PDF", "application/octet-stream"),
            "pdf"
        );
        assert_eq!(private_extension("invoice", "application/ofd"), "ofd");
        assert_eq!(
            private_extension("invoice.bad-ext", "application/pdf"),
            "pdf"
        );
        assert_eq!(
            private_extension("invoice.verylongextension", "application/pdf"),
            "pdf"
        );
    }
}
