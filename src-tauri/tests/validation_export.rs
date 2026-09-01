//! Excel 导出验证测试
//!
//! 测试批次导出为 Excel 格式，验证文件格式、记录数和合计行
//! 运行：source scripts/tauri-env.sh && cargo test validation_export --release -- --nocapture

use chrono::{NaiveDate, Utc};
use invoice_store::models::{ReportedInvoice, TicketType};
use invoice_store::LedgerDb;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

/// 解析后的发票记录（从 JSON 读取）
#[derive(Debug, Clone, Deserialize)]
struct ParsedInvoiceJson {
    file: String,
    invoice_number: String,
    issue_date: String,
    total_amount: String,
    tax_amount: Option<String>,
    buyer_name: Option<String>,
    seller_name: Option<String>,
    ticket_type: String,
}

/// 导出验证结果
#[derive(Debug, Serialize)]
struct ExportValidationResult {
    export_success: bool,
    file_path: String,
    record_count: usize,
    expected_count: usize,
    total_amount_matches: bool,
    columns_count: usize,
    validation_passed: bool,
}

#[test]
fn test_excel_export_validation() {
    // 1. 读取解析后的发票数据
    let json_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parsed_invoices.json");
    let json_content = fs::read_to_string(&json_path).expect("Failed to read parsed_invoices.json");

    let parsed_invoices: Vec<ParsedInvoiceJson> =
        serde_json::from_str(&json_content).expect("Failed to parse JSON");

    println!("✓ 读取到 {} 条解析后的发票记录", parsed_invoices.len());

    // 2. 创建临时测试数据库
    let temp_dir = std::env::temp_dir();
    let db_path = temp_dir.join(format!("test_export_{}.db", Utc::now().timestamp()));
    println!("✓ 创建临时数据库: {:?}", db_path);

    let db = LedgerDb::new(&db_path).expect("Failed to create test database");

    // 3. 创建批次并插入发票
    let batch_id = db
        .create_batch("Excel导出测试批次", "2026-06")
        .expect("Failed to create batch");
    println!("✓ 创建批次成功，ID: {}", batch_id);

    let mut expected_total = Decimal::from(0);

    for parsed in &parsed_invoices {
        let amount = Decimal::from_str(&parsed.total_amount).expect("Invalid amount");
        expected_total += amount;

        let invoice = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: parsed.invoice_number.clone(),
            issue_date: NaiveDate::parse_from_str(&parsed.issue_date, "%Y-%m-%d")
                .expect("Invalid date format"),
            amount,
            tax_amount: parsed
                .tax_amount
                .as_ref()
                .and_then(|s| Decimal::from_str(s).ok()),
            buyer_name: parsed.buyer_name.clone(),
            seller_name: parsed.seller_name.clone(),
            ticket_type: match parsed.ticket_type.as_str() {
                "rail" => TicketType::Rail,
                "flight" => TicketType::Flight,
                "hotel" => TicketType::Hotel,
                "city_transport" => TicketType::CityTransport,
                "meal" => TicketType::Meal,
                _ => TicketType::Other,
            },
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: format!("/test/invoices/{}", parsed.file),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        db.add_invoice(&invoice).expect("Failed to insert invoice");
    }

    println!("✓ 插入 {} 条发票记录", parsed_invoices.len());
    println!("✓ 预期总金额: {}", expected_total);

    // 4. 调用导出函数生成 Excel
    let batch = db.get_batch(batch_id).expect("Failed to get batch");
    let invoices = db
        .list_invoices_by_batch(batch_id)
        .expect("Failed to list invoices");

    // 使用 build_excel_bytes（来自 commands/export.rs）
    let excel_bytes = invoice_assistant::commands::export::build_excel_bytes(&batch, &invoices)
        .expect("Failed to build Excel");

    println!("✓ 生成 Excel 文件，大小: {} 字节", excel_bytes.len());

    // 5. 写入临时文件
    let export_path = temp_dir.join(format!("test_export_{}.xlsx", Utc::now().timestamp()));
    fs::write(&export_path, &excel_bytes).expect("Failed to write Excel file");
    println!("✓ Excel 文件已写入: {:?}", export_path);

    // 6. 使用 calamine 读取并验证
    use calamine::{open_workbook, Reader, Xlsx};

    let mut workbook: Xlsx<_> =
        open_workbook(&export_path).expect("Failed to open Excel file with calamine");

    let sheet_names = workbook.sheet_names().to_owned();
    println!("✓ Excel 工作表: {:?}", sheet_names);

    assert!(!sheet_names.is_empty(), "工作表为空");

    // 读取第一个工作表
    let range = workbook
        .worksheet_range_at(0)
        .expect("No worksheet found")
        .expect("Failed to read worksheet");

    let (rows, cols) = range.get_size();
    println!("✓ Excel 表格尺寸: {} 行 × {} 列", rows, cols);

    // 验证列数（12 列）
    assert!(cols >= 12, "列数不足 12 列，实际: {}", cols);

    // 验证行数（表头 + 数据行 + 合计行）
    let expected_rows = 1 + parsed_invoices.len() + 1; // 表头 + 数据 + 合计
    assert_eq!(
        rows, expected_rows,
        "行数不匹配，预期: {}（表头+数据+合计），实际: {}",
        expected_rows, rows
    );

    // 验证表头
    let header_row = range.rows().next().expect("No header row");
    let headers: Vec<String> = header_row
        .iter()
        .take(12)
        .map(|cell| cell.to_string())
        .collect();

    println!("✓ 表头: {:?}", headers);

    assert_eq!(headers[0], "发票号码");
    assert_eq!(headers[1], "开票日期");
    assert_eq!(headers[2], "金额");
    assert_eq!(headers[3], "税额");
    assert_eq!(headers[11], "重复标记");

    // 验证合计行
    let total_row = range.rows().nth(rows - 1).expect("No total row");
    let total_label = total_row.first().map(|c| c.to_string()).unwrap_or_default();
    let total_amount_str = total_row.get(2).map(|c| c.to_string()).unwrap_or_default();

    println!("✓ 合计行: {} | {}", total_label, total_amount_str);

    assert_eq!(total_label, "合计", "合计行标签不正确");

    // 验证总金额
    let parsed_total = Decimal::from_str(&total_amount_str).unwrap_or_else(|_| Decimal::from(0));

    let total_matches = parsed_total == expected_total;

    if total_matches {
        println!("✓ 总金额匹配: {}", parsed_total);
    } else {
        eprintln!(
            "✗ 总金额不匹配: 预期 {}, 实际 {}",
            expected_total, parsed_total
        );
    }

    // 7. 构建验证结果
    let validation_passed =
        cols >= 12 && rows == expected_rows && total_matches && total_label == "合计";

    let result = ExportValidationResult {
        export_success: true,
        file_path: export_path.to_string_lossy().to_string(),
        record_count: invoices.len(),
        expected_count: parsed_invoices.len(),
        total_amount_matches: total_matches,
        columns_count: cols,
        validation_passed,
    };

    let result_json = serde_json::to_string_pretty(&result).expect("Failed to serialize result");

    println!("\n========== Excel 导出验证结果 ==========");
    println!("{}", result_json);
    println!("========================================\n");

    // 8. 写入结果文件
    let output_path = std::env::temp_dir().join("invoice-assistant-export-validation.json");
    fs::write(&output_path, &result_json).expect("Failed to write result");
    println!("✓ 结果已写入 {:?}", output_path);

    // 9. 清理
    db.delete_batch(batch_id).expect("Failed to delete batch");
    drop(db);
    fs::remove_file(&db_path).expect("Failed to remove temp database");

    // 保留 Excel 文件供手动检查
    println!("✓ Excel 文件保留在: {:?}", export_path);

    // 断言所有验证通过
    assert!(validation_passed, "Excel 导出验证失败");
}
