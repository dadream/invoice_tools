//! 数据库存储验证测试
//!
//! 测试批次 CRUD、状态机转换、发票批量插入和查询
//! 运行：source scripts/tauri-env.sh && cargo test validation_store --release -- --nocapture

use chrono::{NaiveDate, Utc};
use invoice_store::models::{BatchStatus, ReportedInvoice, TicketType};
use invoice_store::LedgerDb;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Instant;

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

/// 验证结果
#[derive(Debug, Serialize)]
struct ValidationResult {
    batch_crud_success: bool,
    invoice_insert_count: usize,
    invoice_query_count: usize,
    all_transitions_valid: bool,
    insert_duration_ms: u128,
    query_duration_ms: u128,
}

#[test]
fn test_database_validation() {
    // 1. 读取解析后的发票数据
    let json_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parsed_invoices.json");
    let json_content = fs::read_to_string(&json_path).expect("Failed to read parsed_invoices.json");

    let parsed_invoices: Vec<ParsedInvoiceJson> =
        serde_json::from_str(&json_content).expect("Failed to parse JSON");

    println!("✓ 读取到 {} 条解析后的发票记录", parsed_invoices.len());

    // 2. 创建临时测试数据库
    let temp_dir = std::env::temp_dir();
    let db_path = temp_dir.join(format!("test_ledger_{}.db", Utc::now().timestamp()));
    println!("✓ 创建临时数据库: {:?}", db_path);

    let db = LedgerDb::new(&db_path).expect("Failed to create test database");

    // 3. 测试批次创建 (Draft 状态)
    let batch_id = db
        .create_batch("验证测试批次", "2026-06")
        .expect("Failed to create batch");
    println!("✓ 创建批次成功，ID: {}", batch_id);

    let batch = db.get_batch(batch_id).expect("Failed to get batch");
    assert_eq!(batch.status, BatchStatus::Draft);
    assert_eq!(batch.name, "验证测试批次");
    assert_eq!(batch.month, "2026-06");
    assert_eq!(batch.invoice_count, 0);
    assert_eq!(batch.total_amount, Decimal::from_str("0").unwrap());
    println!("✓ 批次初始状态验证通过");

    // 4. 测试发票批量插入
    let insert_start = Instant::now();
    let mut inserted_count = 0;

    for parsed in &parsed_invoices {
        // 转换为 ReportedInvoice
        let invoice = ReportedInvoice {
            id: 0, // 会被数据库自动分配
            batch_id,
            invoice_number: parsed.invoice_number.clone(),
            issue_date: NaiveDate::parse_from_str(&parsed.issue_date, "%Y-%m-%d")
                .expect("Invalid date format"),
            amount: Decimal::from_str(&parsed.total_amount).expect("Invalid amount"),
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
        inserted_count += 1;
    }

    let insert_duration = insert_start.elapsed();
    println!(
        "✓ 批量插入 {} 条发票，耗时 {} ms",
        inserted_count,
        insert_duration.as_millis()
    );

    // 5. 测试发票查询
    let query_start = Instant::now();
    let invoices = db
        .list_invoices_by_batch(batch_id)
        .expect("Failed to query invoices");
    let query_duration = query_start.elapsed();

    println!(
        "✓ 查询批次发票，返回 {} 条记录，耗时 {} ms",
        invoices.len(),
        query_duration.as_millis()
    );

    assert_eq!(
        invoices.len(),
        parsed_invoices.len(),
        "查询到的发票数量与插入数量不符"
    );

    // 验证数据一致性
    let first_invoice = &invoices[0];
    let first_parsed = &parsed_invoices[0];
    assert_eq!(first_invoice.invoice_number, first_parsed.invoice_number);
    assert_eq!(first_invoice.amount.to_string(), first_parsed.total_amount);
    println!("✓ 数据一致性检查通过");

    // 6. 测试批次状态转换 (Draft → Submitted → Approved → Completed)
    let mut all_transitions_valid = true;

    // Draft → Submitted
    match db.transition_batch_status(batch_id, BatchStatus::Submitted) {
        Ok(_) => {
            let batch = db.get_batch(batch_id).unwrap();
            assert_eq!(batch.status, BatchStatus::Submitted);
            assert!(batch.submitted_at.is_some());
            println!("✓ Draft → Submitted");
        }
        Err(e) => {
            eprintln!("✗ Draft → Submitted 失败: {:?}", e);
            all_transitions_valid = false;
        }
    }

    // Submitted → Approved
    match db.transition_batch_status(batch_id, BatchStatus::Approved) {
        Ok(_) => {
            let batch = db.get_batch(batch_id).unwrap();
            assert_eq!(batch.status, BatchStatus::Approved);
            assert!(batch.approved_at.is_some());
            println!("✓ Submitted → Approved");
        }
        Err(e) => {
            eprintln!("✗ Submitted → Approved 失败: {:?}", e);
            all_transitions_valid = false;
        }
    }

    // Approved → Completed
    match db.transition_batch_status(batch_id, BatchStatus::Completed) {
        Ok(_) => {
            let batch = db.get_batch(batch_id).unwrap();
            assert_eq!(batch.status, BatchStatus::Completed);
            assert!(batch.completed_at.is_some());
            println!("✓ Approved → Completed");
        }
        Err(e) => {
            eprintln!("✗ Approved → Completed 失败: {:?}", e);
            all_transitions_valid = false;
        }
    }

    // 测试非法转换 (Completed → Draft，应该失败)
    match db.transition_batch_status(batch_id, BatchStatus::Draft) {
        Ok(_) => {
            eprintln!("✗ Completed → Draft 应该失败但成功了");
            all_transitions_valid = false;
        }
        Err(_) => {
            println!("✓ Completed → Draft 正确拒绝（非法转换）");
        }
    }

    // 7. 测试批次列表查询
    let batches = db.list_batches().expect("Failed to list batches");
    assert!(!batches.is_empty());
    println!("✓ 批次列表查询成功，共 {} 个批次", batches.len());

    // 8. 输出结果
    let result = ValidationResult {
        batch_crud_success: true,
        invoice_insert_count: inserted_count,
        invoice_query_count: invoices.len(),
        all_transitions_valid,
        insert_duration_ms: insert_duration.as_millis(),
        query_duration_ms: query_duration.as_millis(),
    };

    let result_json = serde_json::to_string_pretty(&result).expect("Failed to serialize result");

    println!("\n========== 验证结果 ==========");
    println!("{}", result_json);
    println!("==============================\n");

    // 9. 清理测试数据
    db.delete_batch(batch_id).expect("Failed to delete batch");
    println!("✓ 删除测试批次");

    // 删除临时数据库文件
    drop(db);
    fs::remove_file(&db_path).expect("Failed to remove temp database");
    println!("✓ 清理临时数据库文件");

    // 将结果写入文件（供后续阶段使用）
    let output_path = std::env::temp_dir().join("invoice-assistant-store-validation.json");
    fs::write(&output_path, result_json).expect("Failed to write result");
    println!("✓ 结果已写入 {:?}", output_path);

    // 断言所有检查通过
    assert!(result.batch_crud_success, "批次 CRUD 操作失败");
    assert_eq!(
        result.invoice_insert_count,
        parsed_invoices.len(),
        "发票插入数量不匹配"
    );
    assert_eq!(
        result.invoice_query_count,
        parsed_invoices.len(),
        "发票查询数量不匹配"
    );
    assert!(result.all_transitions_valid, "状态转换验证失败");
}
