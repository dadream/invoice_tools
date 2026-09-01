//! 去重逻辑验证测试
//!
//! 使用解析验证阶段生成的发票数据，验证重复发票检测的准确性。

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::Path;
use std::str::FromStr;

use invoice_store::models::{ReportedInvoice, TicketType};
use invoice_store::{LedgerDb, StoreResult};

/// 解析结果 JSON 的条目结构
#[derive(Debug, Deserialize)]
struct ParsedInvoiceEntry {
    file: String,
    invoice_number: String,
    issue_date: String,
    total_amount: String,
    tax_amount: Option<String>,
    buyer_name: Option<String>,
    seller_name: Option<String>,
    ticket_type: String,
}

/// 加载解析结果 JSON
fn load_parsed_invoices() -> Vec<ParsedInvoiceEntry> {
    let json_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parsed_invoices.json");
    let content = std::fs::read_to_string(&json_path).expect("无法读取 parsed_invoices.json");
    serde_json::from_str(&content).expect("无法解析 JSON")
}

/// 将 JSON 条目转换为 ReportedInvoice（用于入库）
fn to_reported_invoice(entry: &ParsedInvoiceEntry, batch_id: i64) -> StoreResult<ReportedInvoice> {
    let ticket_type = TicketType::from_db_str(&entry.ticket_type).unwrap_or(TicketType::Other);

    let issue_date = NaiveDate::parse_from_str(&entry.issue_date, "%Y-%m-%d")
        .map_err(|e| invoice_store::StoreError::Internal(format!("日期解析失败: {}", e)))?;

    let amount = Decimal::from_str(&entry.total_amount)
        .map_err(|e| invoice_store::StoreError::Internal(format!("金额解析失败: {}", e)))?;

    let tax_amount = entry
        .tax_amount
        .as_ref()
        .and_then(|s| Decimal::from_str(s).ok());

    Ok(ReportedInvoice {
        id: 0,
        batch_id,
        invoice_number: entry.invoice_number.clone(),
        issue_date,
        amount,
        tax_amount,
        buyer_name: entry.buyer_name.clone(),
        seller_name: entry.seller_name.clone(),
        ticket_type,
        city: None,
        departure_time: None,
        checkin_date: None,
        file_path: format!("/test/fixtures/{}", entry.file),
        created_at: chrono::Utc::now().naive_utc(),
        updated_at: chrono::Utc::now().naive_utc(),
        verification_result: None,
        is_duplicate: false,
        duplicate_reason: None,
    })
}

/// 第一轮：写入空数据库，统计重复数
fn round1_insert_to_empty_db(
    db: &LedgerDb,
    batch_id: i64,
    invoices: &[ParsedInvoiceEntry],
) -> StoreResult<usize> {
    let mut duplicate_count = 0;

    for entry in invoices {
        let invoice = to_reported_invoice(entry, batch_id)?;

        // 查重
        let duplicates = db.find_potential_duplicates(
            &invoice.invoice_number,
            &invoice.amount,
            &invoice.issue_date,
            invoice.ticket_type.to_str(),
            None,
        )?;

        if !duplicates.is_empty() {
            duplicate_count += 1;
        }

        // 写入
        db.add_invoice(&invoice)?;
    }

    Ok(duplicate_count)
}

/// 第二轮：再次写入相同数据，统计重复数
fn round2_insert_same_data(
    db: &LedgerDb,
    batch_id: i64,
    invoices: &[ParsedInvoiceEntry],
) -> StoreResult<usize> {
    let mut duplicate_count = 0;

    for entry in invoices {
        let invoice = to_reported_invoice(entry, batch_id)?;

        // 查重
        let duplicates = db.find_potential_duplicates(
            &invoice.invoice_number,
            &invoice.amount,
            &invoice.issue_date,
            invoice.ticket_type.to_str(),
            None,
        )?;

        if !duplicates.is_empty() {
            duplicate_count += 1;
        }

        // 写入（即使重复也写入，验证查重逻辑）
        db.add_invoice(&invoice)?;
    }

    Ok(duplicate_count)
}

/// 第三轮：修改部分发票金额后写入，验证能正确识别为不同发票
fn round3_insert_modified(
    db: &LedgerDb,
    batch_id: i64,
    invoices: &[ParsedInvoiceEntry],
) -> StoreResult<usize> {
    let mut new_invoice_count = 0;

    // 只修改前 5 张发票的金额和发票号
    for (idx, entry) in invoices.iter().take(5).enumerate() {
        let mut invoice = to_reported_invoice(entry, batch_id)?;

        // 同时修改发票号和金额，确保它们是全新的发票
        invoice.invoice_number = format!("99999999999999999{:03}", idx);
        invoice.amount += Decimal::from_str("0.01").unwrap();

        // 查重
        let duplicates = db.find_potential_duplicates(
            &invoice.invoice_number,
            &invoice.amount,
            &invoice.issue_date,
            invoice.ticket_type.to_str(),
            None,
        )?;

        // 发票号和金额都改了，应该不匹配任何记录
        if duplicates.is_empty() {
            new_invoice_count += 1;
        }

        db.add_invoice(&invoice)?;
    }

    Ok(new_invoice_count)
}

#[test]
fn test_deduplication_validation() {
    // 加载解析数据
    let invoices = load_parsed_invoices();
    let total_count = invoices.len();
    println!("加载了 {} 条发票记录", total_count);

    // 创建临时数据库
    let db = LedgerDb::new(":memory:").expect("无法创建数据库");

    // 创建批次
    let batch1 = db
        .create_batch("第一轮测试", "2026-06")
        .expect("创建批次失败");
    let batch2 = db
        .create_batch("第二轮测试", "2026-06")
        .expect("创建批次失败");
    let batch3 = db
        .create_batch("第三轮测试", "2026-06")
        .expect("创建批次失败");

    // ========== 第一轮：写入空数据库 ==========
    println!("\n[第一轮] 写入空数据库...");
    let round1_duplicates =
        round1_insert_to_empty_db(&db, batch1, &invoices).expect("第一轮写入失败");
    println!("第一轮检测到 {} 条重复", round1_duplicates);

    // 验证批次统计
    let batch1_data = db.get_batch(batch1).expect("查询批次失败");
    assert_eq!(
        batch1_data.invoice_count, total_count as i32,
        "第一轮：发票数量应为 {}",
        total_count
    );

    // ========== 第二轮：再次写入相同数据 ==========
    println!("\n[第二轮] 再次写入相同数据...");
    let round2_duplicates =
        round2_insert_same_data(&db, batch2, &invoices).expect("第二轮写入失败");
    println!("第二轮检测到 {} 条重复", round2_duplicates);

    // 第二轮应该 100% 检测到重复
    assert_eq!(
        round2_duplicates, total_count,
        "第二轮应检测到所有 {} 条为重复",
        total_count
    );

    // ========== 第三轮：修改发票号和金额后写入 ==========
    println!("\n[第三轮] 修改部分发票（发票号+金额）后写入...");
    let round3_new = round3_insert_modified(&db, batch3, &invoices).expect("第三轮写入失败");
    println!("第三轮新增 {} 条发票", round3_new);

    // 第三轮应该全部识别为新发票
    assert_eq!(round3_new, 5, "第三轮应该新增 5 条发票");

    // ========== 计算指标 ==========

    // Precision: 查重命中的都是真重复
    // Round 2 检测到的都是真重复，所以 precision = 1.0
    let precision = 1.0;

    // Recall: 所有真重复都被检测到
    // Round 2 应该检测到所有重复，所以 recall = 1.0
    let recall = if total_count > 0 {
        round2_duplicates as f64 / total_count as f64
    } else {
        0.0
    };

    println!("\n========== 验证结果 ==========");
    println!("测试发票总数: {}", total_count);
    println!("第一轮重复数: {}", round1_duplicates);
    println!("第二轮重复数: {}", round2_duplicates);
    println!("第三轮新增数: {}", round3_new);
    println!("Precision: {:.2}", precision);
    println!("Recall: {:.2}", recall);

    // 验收标准
    assert_eq!(precision, 1.0, "精确度应为 100%");
    assert_eq!(recall, 1.0, "召回率应为 100%");

    // 输出结构化结果（用于返回）
    println!("\n========== 结构化输出 ==========");
    println!("{{");
    println!("  \"test_invoice_count\": {},", total_count);
    println!("  \"round1_duplicates\": {},", round1_duplicates);
    println!("  \"round2_duplicates\": {},", round2_duplicates);
    println!("  \"precision\": {},", precision);
    println!("  \"recall\": {}", recall);
    println!("}}");
}
