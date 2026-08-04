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
        Some("explore-xml") => explore_xml(),
        Some(other) => bail!("未知子命令: {other}"),
        None => {
            eprintln!("用法:");
            eprintln!("  invoice-parse explore-xml");
            eprintln!("  invoice-parse dump-tags <file.xml>");
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
