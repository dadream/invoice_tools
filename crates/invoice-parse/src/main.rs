use anyhow::{bail, Context};
use invoice_parse::{manifest::{Manifest, TagHints}, model::TicketType, pdf, xml};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
        Some("dump-pdf") => {
            let path = args.get(2).context("用法: invoice-parse dump-pdf <file.pdf>")?;
            let bytes = std::fs::read(&path)?;
            println!("有文本层: {}", invoice_parse::pdf::has_text_layer(&bytes));
            println!("--- 文本层内容 ---");
            println!("{}", invoice_parse::pdf::extract_text(&bytes, Path::new(path))?);
            Ok(())
        }
        Some("dump-pdf-boxes") => {
            let path = args.get(2).context("用法: invoice-parse dump-pdf-boxes <file.pdf>")?;
            dump_pdf_boxes(PathBuf::from(path))
        }
        Some("verify") => {
            let path = args.get(2).context("用法: invoice-parse verify <file.ofd|file.xml>")?;
            let bytes = std::fs::read(path)?;
            let path_obj = Path::new(path);

            if path.ends_with(".xml") {
                // XML 验签
                println!("验签 XML 文件...");
                let status = invoice_parse::verify::verify_xml_signature(&bytes, path_obj)?;
                println!("验签结果: {:?}", status);
            } else {
                // OFD 验签
                match invoice_parse::verify::locate_signature(&bytes)? {
                    None => println!("容器内未找到签章文件"),
                    Some(sig) => {
                        println!("签章文件: {} （{} 字节）", sig.entry_name, sig.raw.len());
                        println!("前 32 字节: {:02x?}", &sig.raw[..sig.raw.len().min(32)]);
                    }
                }
                println!(
                    "验签结果: {:?}",
                    invoice_parse::verify::verify_ofd_signature(&bytes, path_obj)?
                );
            }
            Ok(())
        }
        Some("verify-all") => verify_all(),
        Some("explore-xml") => explore_xml(),
        Some(other) => bail!("未知子命令: {other}"),
        None => {
            eprintln!("用法:");
            eprintln!("  invoice-parse explore-xml");
            eprintln!("  invoice-parse dump-tags <file.xml>");
            eprintln!("  invoice-parse parse-one <file.xml>");
            eprintln!("  invoice-parse dump-ofd <file.ofd>");
            eprintln!("  invoice-parse dump-pdf <file.pdf>");
            eprintln!("  invoice-parse dump-pdf-boxes <file.pdf>");
            eprintln!("  invoice-parse verify <file.ofd>");
            eprintln!("  invoice-parse verify-all");
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

    let bytes = std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;

    // Determine format and parse accordingly
    let invoice = match sample.format.as_str() {
        "xml-rail" | "xml-flight" | "xml-vat" => {
            let hints = sample.xml_tag_hints.as_ref()
                .context("该样本没有配置 xml_tag_hints")?;
            xml::parse_invoice_xml(&bytes, &path, hints, sample.ticket_type.unwrap_or(TicketType::Other))?
        }
        "ofd" => {
            let empty_hints = TagHints {
                invoice_number: vec![],
                issue_date: vec![],
                total_amount: vec![],
                tax_amount: vec![],
                tax_rate: vec![],
                buyer_name: vec![],
                seller_name: vec![],
            };
            let hints = sample.xml_tag_hints.as_ref().unwrap_or(&empty_hints);
            invoice_parse::ofd::parse_invoice_ofd(&bytes, &path, hints, sample.ticket_type.unwrap_or(TicketType::Other))?
        }
        "pdf-rail" | "pdf-flight" => {
            // For PDFs, create empty hints since PDF parsing doesn't use XML tag hints
            let empty_hints = TagHints {
                invoice_number: vec![],
                issue_date: vec![],
                total_amount: vec![],
                tax_amount: vec![],
                tax_rate: vec![],
                buyer_name: vec![],
                seller_name: vec![],
            };
            let hints = sample.xml_tag_hints.as_ref().unwrap_or(&empty_hints);
            pdf::parse_invoice_pdf(&bytes, &path, hints, sample.ticket_type.unwrap_or(TicketType::Other))?
        }
        "pdf-vat" => {
            // L1 坐标路径优先（支持表格版式字段提取），失败降级 flat-text
            invoice_parse::pdf_text::parse_vat_invoice_from_boxes(&bytes, &path)
                .or_else(|_| {
                    let text = invoice_parse::pdf::extract_text(&bytes, &path)
                        .map_err(|e| anyhow::anyhow!("PDF 文本提取失败: {}", e))?;
                    invoice_parse::pdf::parse_vat_invoice_text(&text, &path)
                        .map_err(anyhow::Error::from)
                })?
        }
        other => anyhow::bail!("不支持的格式: {}", other),
    };

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

fn dump_pdf_boxes(path: PathBuf) -> anyhow::Result<()> {
    let bytes = std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;

    println!("提取文本框（带坐标）：");
    let boxes = invoice_parse::pdf_text::extract_text_boxes(&bytes, &path)?;
    println!("共 {} 个文本框（原始）\n", boxes.len());

    for (i, b) in boxes.iter().enumerate().take(20) {
        println!(
            "{:3}: ({:6.1}, {:6.1}) {}x{} conf={:.2} \"{}\"",
            i,
            b.x,
            b.y,
            b.width as i32,
            b.height as i32,
            b.confidence,
            b.text.chars().take(30).collect::<String>()
        );
    }

    if boxes.len() > 20 {
        println!("... 还有 {} 个文本框", boxes.len() - 20);
    }

    // Show merged boxes
    let merged = invoice_parse::ocr::merge_line_fragments(boxes, 12.0);
    println!("\n合并后 {} 个文本框：\n", merged.len());

    // 合并后的框全部打印：诊断购销方区块和税额列必须看到完整版式
    for (i, b) in merged.iter().enumerate() {
        println!(
            "{:3}: ({:6.1}, {:6.1}) {}x{} conf={:.2} \"{}\"",
            i,
            b.x,
            b.y,
            b.width as i32,
            b.height as i32,
            b.confidence,
            b.text.chars().take(50).collect::<String>()
        );
    }

    // Try parsing as VAT invoice
    println!("\n尝试解析为增值税发票：");
    match invoice_parse::pdf_text::parse_vat_invoice_from_boxes(&bytes, &path) {
        Ok(invoice) => {
            println!("发票号码: {}", invoice.invoice_number);
            println!("开票日期: {}", invoice.issue_date);
            println!("价税合计: {}", invoice.total_amount);
            if let Some(tax) = invoice.tax_amount {
                println!("税额: {}", tax);
            }
            if let Some(buyer) = invoice.buyer_name {
                println!("购买方: {}", buyer);
            }
            if let Some(seller) = invoice.seller_name {
                println!("销售方: {}", seller);
            }
            println!("解析等级: {:?}", invoice.parse_level);
            println!("置信度: {:.2}", invoice.confidence);
        }
        Err(e) => println!("解析失败: {}", e),
    }

    Ok(())
}

fn verify_all() -> anyhow::Result<()> {
    use invoice_parse::manifest::{Manifest, TagHints};
    use invoice_parse::model::ParsedInvoice;
    use invoice_parse::report::{render_markdown, OutcomeKind, SampleOutcome};

    let manifest = Manifest::load(Path::new("fixtures/manifest.toml"))?;
    let mut outcomes = Vec::new();

    for sample in &manifest.samples {
        let full_path = PathBuf::from("fixtures").join(&sample.path);

        // 人工确认非发票的样本（邮件横幅、下载按钮、广告图）直接跳过，
        // 不进入通过率的分子和分母。
        if sample.is_invoice == Some(false) {
            outcomes.push(SampleOutcome {
                path: sample.path.display().to_string(),
                format: sample.format.clone(),
                result: OutcomeKind::Skipped {
                    reason: sample
                        .not_invoice_reason
                        .clone()
                        .unwrap_or_else(|| "人工确认非发票".to_string()),
                },
            });
            continue;
        }

        let hints = sample.xml_tag_hints.clone().unwrap_or(TagHints {
            invoice_number: vec![],
            issue_date: vec![],
            total_amount: vec![],
            tax_amount: vec![],
            tax_rate: vec![],
            buyer_name: vec![],
            seller_name: vec![],
        });

        let parsed: anyhow::Result<ParsedInvoice> = std::panic::catch_unwind(|| {
            match sample.format.as_str() {
                "xml" | "xml-vat" | "xml-rail" | "xml-flight" => std::fs::read(&full_path)
                    .map_err(anyhow::Error::from)
                    .and_then(|b| {
                        invoice_parse::xml::parse_invoice_xml(
                            &b,
                            &full_path,
                            &hints,
                            sample.ticket_type.unwrap_or(TicketType::Other),
                        )
                        .map_err(Into::into)
                    }),
                "ofd" => std::fs::read(&full_path)
                    .map_err(anyhow::Error::from)
                    .and_then(|b| {
                        invoice_parse::ofd::parse_invoice_ofd(
                            &b,
                            &full_path,
                            &hints,
                            sample.ticket_type.unwrap_or(TicketType::Other),
                        )
                        .map_err(Into::into)
                    }),
                "pdf-rail" => parse_pdf_with(&full_path, invoice_parse::pdf::parse_rail_itinerary),
                "pdf-flight" => parse_pdf_with(&full_path, invoice_parse::pdf::parse_flight_itinerary),
                "pdf-vat" => {
                    // L1 坐标路径优先，失败降级 flat-text
                    let bytes = std::fs::read(&full_path).map_err(anyhow::Error::from)?;
                    invoice_parse::pdf_text::parse_vat_invoice_from_boxes(&bytes, &full_path)
                        .map_err(anyhow::Error::from)
                        .or_else(|_| {
                            let text = invoice_parse::pdf::extract_text(&bytes, &full_path)
                                .map_err(|e| anyhow::anyhow!("PDF 文本提取失败: {}", e))?;
                            invoice_parse::pdf::parse_vat_invoice_text(&text, &full_path)
                                .map_err(anyhow::Error::from)
                        })
                }
                "image" => {
                    // L2 OCR via Python sidecar
                    let boxes = invoice_parse::ocr::recognize_via_sidecar(&full_path)
                        .map_err(anyhow::Error::from)?;
                    invoice_parse::ocr::locate_vat_fields(
                        &boxes,
                        &full_path,
                        invoice_parse::model::ParseLevel::L2,
                    )
                    .map_err(anyhow::Error::from)
                }
                other => Err(anyhow::anyhow!("未知格式: {other}")),
            }
        })
        .unwrap_or_else(|panic_err| {
            let panic_msg = if let Some(s) = panic_err.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_err.downcast_ref::<String>() {
                s.clone()
            } else {
                "解析器 panic".to_string()
            };
            Err(anyhow::anyhow!("解析器崩溃: {}", panic_msg))
        });

        let result = match parsed {
            Ok(invoice) => {
                let comparisons = sample.compare(&invoice);
                let failures: Vec<_> = comparisons
                    .into_iter()
                    .filter(|c| c.status == invoice_parse::manifest::FieldStatus::Mismatch)
                    .collect();
                if failures.is_empty() {
                    OutcomeKind::FullMatch
                } else {
                    OutcomeKind::PartialMatch { failures }
                }
            }
            Err(e) => OutcomeKind::ParseFailed {
                error: e.to_string(),
            },
        };

        outcomes.push(SampleOutcome {
            path: sample.path.display().to_string(),
            format: sample.format.clone(),
            result,
        });
    }

    let md = render_markdown(&outcomes);
    std::fs::create_dir_all("docs")?;
    std::fs::write("docs/spike-report.md", &md)?;
    println!("{md}");
    println!("报告已写入 docs/spike-report.md");
    Ok(())
}

fn parse_pdf_with(
    path: &Path,
    parser: fn(&str, &Path) -> Result<invoice_parse::model::ParsedInvoice, invoice_parse::model::ParseError>,
) -> anyhow::Result<invoice_parse::model::ParsedInvoice> {
    let bytes = std::fs::read(path)?;
    let text = match invoice_parse::pdf::extract_text(&bytes, path) {
        Ok(t) => t,
        Err(e) => return Err(anyhow::anyhow!("PDF 文本提取失败: {}", e)),
    };
    parser(&text, path).map_err(Into::into)
}
