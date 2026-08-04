use anyhow::{bail, Context};
use invoice_parse::xml;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("dump-tags") => {
            let path = args.get(2).context("用法: invoice-parse dump-tags <file.xml>")?;
            dump_tags(PathBuf::from(path))
        }
        Some(other) => bail!("未知子命令: {other}"),
        None => {
            eprintln!("用法: invoice-parse dump-tags <file.xml>");
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
