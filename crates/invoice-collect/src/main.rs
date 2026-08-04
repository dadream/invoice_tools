use anyhow::{bail, Context};
use invoice_collect::config::{DateRange, ImapConfig};
use invoice_collect::imap_client::Session;

const DEFAULT_SINCE: &str = "2026-06-01";
const DEFAULT_BEFORE: &str = "2026-07-01";

const USAGE: &str = "用法:
  invoice-collect probe   <邮箱地址> [起始日期 结束日期]
  invoice-collect collect <邮箱地址> [起始日期 结束日期]

日期格式 YYYY-MM-DD，默认 2026-06-01 至 2026-07-01（半开区间）。
密码从环境变量 INVOICE_IMAP_PASSWORD 读取。QQ 邮箱需填 16 位授权码。";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("probe") => probe(&parse_target(&args)?),
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
