use anyhow::{bail, Context};
use invoice_parse::{manifest::Manifest, xml};
use std::collections::HashMap;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("dump-tags") => {
            let path = args.get(2).context("用法: invoice-parse dump-tags <file.xml>")?;
            dump_tags(PathBuf::from(path))
        }
        Some("parse-one") => {
            let path = args.get(2).context("用法: invoice-parse parse-one <file.xml>")?;
            parse_one(PathBuf::from(path))
        }
        Some("dump-ofd") => {
            let path = args.get(2).context("用法: invoice-parse dump-ofd <file.ofd>")?;
            dump_ofd(PathBuf::from(path))
        }
        Some("explore-xml") => explore_xml(),
        Some(other) => bail!("未知子命令: {other}"),
        None => {
            eprintln!("用法:");
            eprintln!("  invoice-parse explore-xml");
            eprintln!("  invoice-parse dump-tags <file.xml>");
            eprintln!("  invoice-parse parse-one <file.xml>");
            eprintln!("  invoice-parse dump-ofd <file.ofd>");
            Ok(())
        }
    }
}

fn dump_tags(path: PathBuf) -> anyhow::Result<()> {
    let bytes = std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;
    let leaves = xml::collect_leaf_elements(&bytes)?;

    println!("{} 个叶子元素：\n", leaves.len());
    for leaf in &leaves {
        let indent = "  ".repeat(leaf.depth);
        println!("{indent}{:<28} = {}", leaf.tag, leaf.text);
    }
    Ok(())
}

fn parse_one(path: PathBuf) -> anyhow::Result<()> {
    let manifest = Manifest::load(PathBuf::from("fixtures/manifest.toml").as_path())?;

    // Find the sample entry for this path
    let file_name = path.file_name()
        .and_then(|n| n.to_str())
        .context("无效的文件名")?;

    let sample = manifest
        .samples
        .iter()
        .find(|s| s.path.ends_with(file_name))
        .context("未在 manifest.toml 中找到该样本")?;

    let hints = sample.xml_tag_hints.as_ref()
        .context("该样本没有配置 xml_tag_hints")?;

    let bytes = std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;

    let invoice = xml::parse_invoice_xml(&bytes, &path, hints, sample.ticket_type)?;

    println!("解析成功！\n");
    println!("发票号码: {}", invoice.invoice_number);
    println!("开票日期: {}", invoice.issue_date);
    println!("价税合计: {}", invoice.total_amount);
    if let Some(tax) = invoice.tax_amount {
        println!("税额: {}", tax);
    }
    if let Some(rate) = invoice.tax_rate {
        println!("税率: {}", rate);
    }
    if let Some(buyer) = &invoice.buyer_name {
        println!("购买方: {}", buyer);
    }
    if let Some(seller) = &invoice.seller_name {
        println!("销售方: {}", seller);
    }
    println!("票据类型: {:?}", invoice.ticket_type);
    println!("解析级别: {:?}", invoice.parse_level);
    println!("置信度: {}", invoice.confidence);

    Ok(())
}

fn explore_xml() -> anyhow::Result<()> {
    let manifest = Manifest::load(PathBuf::from("fixtures/manifest.toml").as_path())?;

    let xml_samples: Vec<_> = manifest
        .samples
        .iter()
        .filter(|s| s.format == "xml")
        .collect();

    println!("发现 {} 个 XML 样本\n", xml_samples.len());

    let mut tag_counts: HashMap<String, usize> = HashMap::new();

    for sample in &xml_samples {
        let path = PathBuf::from("fixtures").join(&sample.path);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("警告: 读取 {} 失败: {}", path.display(), e);
                continue;
            }
        };

        let leaves = match xml::collect_leaf_elements(&bytes) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("警告: 解析 {} 失败: {}", path.display(), e);
                continue;
            }
        };

        for leaf in leaves {
            *tag_counts.entry(leaf.tag).or_insert(0) += 1;
        }
    }

    let mut tag_freq: Vec<_> = tag_counts.into_iter().collect();
    tag_freq.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    println!("标签频次统计：");
    for (tag, count) in tag_freq {
        println!("{}: {}", tag, count);
    }

    Ok(())
}

fn dump_ofd(path: PathBuf) -> anyhow::Result<()> {
    let bytes = std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;

    println!("容器条目：");
    for name in invoice_parse::ofd::list_entries(&bytes)? {
        println!("  {name}");
    }

    match invoice_parse::ofd::extract_invoice_xml(&bytes, &path) {
        Ok(xml) => {
            println!("\n内嵌发票 XML 的叶子元素：");
            for leaf in invoice_parse::xml::collect_leaf_elements(&xml)? {
                println!("  {:<28} = {}", leaf.tag, leaf.text);
            }
        }
        Err(e) => println!("\n未能提取内嵌 XML: {e}"),
    }
    Ok(())
}
