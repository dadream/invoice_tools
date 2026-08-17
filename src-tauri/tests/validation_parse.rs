//! 批量解析验证测试：遍历所有样本文件，统计解析成功率与字段提取质量。
//!
//! 运行：cargo test validation_parse --release -- --nocapture
//! 输出：reports/parse_report.json

use chrono::NaiveDate;
use invoice_parse::manifest::TagHints;
use invoice_parse::model::{ParseLevel, ParsedInvoice, TicketType};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// 格式统计
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FormatStats {
    total: usize,
    success: usize,
    failed: usize,
    rate: f64,
}

/// 字段提取统计
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FieldStats {
    extracted: usize,
    rate: f64,
}

/// 失败记录
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FailureRecord {
    file: String,
    reason: String,
}

/// 解析成功的发票（简化版，用于后续阶段）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParsedInvoiceRecord {
    file: String,
    invoice_number: String,
    issue_date: String,
    total_amount: String,
    tax_amount: Option<String>,
    buyer_name: Option<String>,
    seller_name: Option<String>,
    ticket_type: String,
    parse_level: String,
    confidence: f32,
}

/// 完整报告
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParseReport {
    total_files: usize,
    success_count: usize,
    success_rate: f64,
    by_format: HashMap<String, FormatStats>,
    field_extraction: HashMap<String, FieldStats>,
    parse_level_distribution: HashMap<String, usize>,
    failures: Vec<FailureRecord>,
    parsed_invoices_json_path: String,
}

/// 内置 tag hints，从 invoice_assistant 模块引用
fn builtin_hints() -> TagHints {
    TagHints {
        invoice_number: vec!["InvoiceNumber".into(), "EIid".into()],
        issue_date: vec!["IssueTime".into(), "RequestTime".into()],
        total_amount: vec!["TotalTax-includedAmount".into()],
        tax_amount: vec!["TotalTaxAm".into()],
        tax_rate: vec!["TaxRate".into()],
        buyer_name: vec!["BuyerName".into()],
        seller_name: vec!["SellerName".into()],
    }
}

/// 根据扩展名分派解析
fn parse_file(path: &Path) -> Result<ParsedInvoice, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .ok_or("无扩展名")?;

    let bytes = fs::read(path)
        .map_err(|e| format!("读取文件失败: {}", e))?;

    let hints = builtin_hints();
    let ticket_type = TicketType::Other;

    // 使用 catch_unwind 防止 panic
    let dispatch = std::panic::AssertUnwindSafe(|| match ext.as_str() {
        "xml" => invoice_parse::xml::parse_invoice_xml(&bytes, path, &hints, ticket_type),
        "ofd" => invoice_parse::ofd::parse_invoice_ofd(&bytes, path, &hints, ticket_type),
        "pdf" => {
            // 先尝试 L1 坐标路径，失败再降级 flat-text
            invoice_parse::pdf_text::parse_vat_invoice_from_boxes(&bytes, path)
                .or_else(|_| invoice_parse::pdf::parse_invoice_pdf(&bytes, path, &hints, ticket_type))
        }
        _ => Err(invoice_parse::model::ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format: "unsupported",
            detail: format!("不支持的格式: .{}", ext),
        }),
    });

    std::panic::catch_unwind(dispatch)
        .map_err(|_| "解析库 panic".to_string())?
        .map_err(|e| format!("{:?}", e))
}

/// 将 ParsedInvoice 转为简化记录
fn to_record(invoice: &ParsedInvoice, file: &str) -> ParsedInvoiceRecord {
    ParsedInvoiceRecord {
        file: file.to_string(),
        invoice_number: invoice.invoice_number.clone(),
        issue_date: invoice.issue_date.format("%Y-%m-%d").to_string(),
        total_amount: invoice.total_amount.to_string(),
        tax_amount: invoice.tax_amount.as_ref().map(|d| d.to_string()),
        buyer_name: invoice.buyer_name.clone(),
        seller_name: invoice.seller_name.clone(),
        ticket_type: format!("{:?}", invoice.ticket_type).to_lowercase(),
        parse_level: format!("{:?}", invoice.parse_level),
        confidence: invoice.confidence,
    }
}

#[test]
fn validation_parse() {
    let samples_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures/samples");

    if !samples_dir.exists() {
        eprintln!("样本目录不存在: {:?}", samples_dir);
        panic!("样本目录不存在，无法运行验证");
    }

    let mut files: Vec<PathBuf> = fs::read_dir(&samples_dir)
        .expect("无法读取样本目录")
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_file() {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    files.sort();

    let mut format_counts: HashMap<String, (usize, usize)> = HashMap::new();
    let mut parse_level_counts: HashMap<String, usize> = HashMap::new();
    let mut field_counts = HashMap::new();
    let mut failures = Vec::new();
    let mut parsed_invoices = Vec::new();

    let total_files = files.len();
    let mut success_count = 0;

    println!("\n开始解析 {} 个样本文件...\n", total_files);

    for (idx, path) in files.iter().enumerate() {
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_else(|| "unknown".to_string());

        print!("[{:2}/{:2}] {} ... ", idx + 1, total_files, filename);

        // 统计格式分布
        let entry = format_counts.entry(ext.clone()).or_insert((0, 0));
        entry.0 += 1;

        match parse_file(path) {
            Ok(invoice) => {
                println!("✓ {} (conf: {:.2})", format!("{:?}", invoice.parse_level), invoice.confidence);

                success_count += 1;
                entry.1 += 1;

                // 统计 parse_level 分布
                let level_str = format!("{:?}", invoice.parse_level);
                *parse_level_counts.entry(level_str).or_insert(0) += 1;

                // 统计字段提取
                *field_counts.entry("invoice_number".to_string()).or_insert(0) += 1;
                *field_counts.entry("issue_date".to_string()).or_insert(0) += 1;
                *field_counts.entry("total_amount".to_string()).or_insert(0) += 1;

                if invoice.tax_amount.is_some() {
                    *field_counts.entry("tax_amount".to_string()).or_insert(0) += 1;
                }
                if invoice.buyer_name.is_some() {
                    *field_counts.entry("buyer_name".to_string()).or_insert(0) += 1;
                }
                if invoice.seller_name.is_some() {
                    *field_counts.entry("seller_name".to_string()).or_insert(0) += 1;
                }

                parsed_invoices.push(to_record(&invoice, &filename));
            }
            Err(reason) => {
                println!("✗ {}", reason);
                failures.push(FailureRecord {
                    file: filename.clone(),
                    reason,
                });
            }
        }
    }

    // 生成格式统计
    let mut by_format = HashMap::new();
    for (format, (total, success)) in format_counts.iter() {
        by_format.insert(
            format.clone(),
            FormatStats {
                total: *total,
                success: *success,
                failed: total - success,
                rate: if *total > 0 {
                    (*success as f64) / (*total as f64)
                } else {
                    0.0
                },
            },
        );
    }

    // 生成字段统计
    let mut field_extraction = HashMap::new();
    for field in ["invoice_number", "issue_date", "total_amount", "tax_amount", "buyer_name", "seller_name"] {
        let extracted = field_counts.get(field).copied().unwrap_or(0);
        field_extraction.insert(
            field.to_string(),
            FieldStats {
                extracted,
                rate: if success_count > 0 {
                    (extracted as f64) / (success_count as f64)
                } else {
                    0.0
                },
            },
        );
    }

    let success_rate = if total_files > 0 {
        (success_count as f64) / (total_files as f64)
    } else {
        0.0
    };

    // 保存解析成功的发票数据
    let invoices_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../reports/parsed_invoices.json");
    fs::write(&invoices_path, serde_json::to_string_pretty(&parsed_invoices).unwrap())
        .expect("无法写入 parsed_invoices.json");

    let report = ParseReport {
        total_files,
        success_count,
        success_rate,
        by_format,
        field_extraction,
        parse_level_distribution: parse_level_counts,
        failures,
        parsed_invoices_json_path: invoices_path.display().to_string(),
    };

    // 保存报告
    let report_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../reports/parse_report.json");
    fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap())
        .expect("无法写入 parse_report.json");

    println!("\n==================== 解析验证报告 ====================");
    println!("总文件数: {}", total_files);
    println!("解析成功: {} ({:.1}%)", success_count, success_rate * 100.0);
    println!("解析失败: {} ({:.1}%)", total_files - success_count, (1.0 - success_rate) * 100.0);
    println!("\n按格式统计:");
    for (format, stats) in report.by_format.iter() {
        println!(
            "  {}: {}/{} ({:.1}%)",
            format,
            stats.success,
            stats.total,
            stats.rate * 100.0
        );
    }

    println!("\nParse Level 分布:");
    for (level, count) in report.parse_level_distribution.iter() {
        println!("  {}: {}", level, count);
    }

    println!("\n字段提取率 (基于成功解析的 {} 个发票):", success_count);
    for (field, stats) in report.field_extraction.iter() {
        println!(
            "  {}: {}/{} ({:.1}%)",
            field,
            stats.extracted,
            success_count,
            stats.rate * 100.0
        );
    }

    println!("\n报告已保存:");
    println!("  - {}", report_path.display());
    println!("  - {}", invoices_path.display());
    println!("====================================================\n");

    // 测试通过条件：至少 50% 的文件能够解析成功
    assert!(
        success_rate >= 0.5,
        "解析成功率过低: {:.1}%",
        success_rate * 100.0
    );
}
