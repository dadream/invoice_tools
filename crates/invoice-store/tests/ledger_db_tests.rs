use chrono::{NaiveDate, Utc};
use invoice_store::ledger_db::LedgerDb;
use invoice_store::models::{BatchStatus, ReportedInvoice, TicketType};
use rust_decimal::Decimal;
use std::str::FromStr;
use tempfile::TempDir;

#[test]
fn full_batch_lifecycle() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("ledger.db");
    let db = LedgerDb::new(&db_path).unwrap();

    // 创建批次
    let batch_id = db.create_batch("2026年7月出差报销", "2026-07").unwrap();
    let batch = db.get_batch(batch_id).unwrap();
    assert_eq!(batch.name, "2026年7月出差报销");
    assert_eq!(batch.status, BatchStatus::Draft);

    // 添加发票
    let invoice = ReportedInvoice {
        id: 0,
        batch_id,
        invoice_number: "12345678901234567890".to_string(),
        issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        amount: Decimal::from_str("258.50").unwrap(),
        tax_amount: Some(Decimal::from_str("23.50").unwrap()),
        buyer_name: Some("测试公司".to_string()),
        seller_name: Some("北京南→上海虹桥".to_string()),
        ticket_type: TicketType::Rail,
        city: Some("北京".to_string()),
        departure_time: None,
        checkin_date: None,
        file_path: "/tmp/invoice.xml".to_string(),
        created_at: Utc::now().naive_utc(),
        updated_at: Utc::now().naive_utc(),
        verification_result: None,
        is_duplicate: false,
        duplicate_reason: None,
    };

    db.add_invoice(&invoice).unwrap();

    // 验证批次统计更新
    let batch = db.get_batch(batch_id).unwrap();
    assert_eq!(batch.invoice_count, 1);
    assert_eq!(batch.total_amount, Decimal::from_str("258.50").unwrap());

    // 更新批次状态
    db.transition_batch_status(batch_id, BatchStatus::Submitted)
        .unwrap();
    let batch = db.get_batch(batch_id).unwrap();
    assert_eq!(batch.status, BatchStatus::Submitted);
    assert!(batch.submitted_at.is_some());

    // 列出发票
    let invoices = db.list_invoices_by_batch(batch_id).unwrap();
    assert_eq!(invoices.len(), 1);
    assert_eq!(invoices[0].invoice_number, "12345678901234567890");
}

#[test]
fn multiple_batches_and_invoices() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("ledger.db");
    let db = LedgerDb::new(&db_path).unwrap();

    // 创建两个批次
    let batch1 = db.create_batch("6月报销", "2026-06").unwrap();
    let batch2 = db.create_batch("7月报销", "2026-07").unwrap();

    // 批次1添加2张发票
    for i in 1..=2 {
        let invoice = ReportedInvoice {
            id: 0,
            batch_id: batch1,
            invoice_number: format!("1111111111111111111{}", i),
            issue_date: NaiveDate::from_ymd_opt(2026, 6, 10 + i).unwrap(),
            amount: Decimal::from_str("100.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Meal,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: format!("/tmp/invoice{}.xml", i),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };
        db.add_invoice(&invoice).unwrap();
    }

    // 批次2添加1张发票
    let invoice = ReportedInvoice {
        id: 0,
        batch_id: batch2,
        invoice_number: "22222222222222222222".to_string(),
        issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        amount: Decimal::from_str("500.00").unwrap(),
        tax_amount: None,
        buyer_name: None,
        seller_name: None,
        ticket_type: TicketType::Hotel,
        city: None,
        departure_time: None,
        checkin_date: None,
        file_path: "/tmp/invoice3.xml".to_string(),
        created_at: Utc::now().naive_utc(),
        updated_at: Utc::now().naive_utc(),
        verification_result: None,
        is_duplicate: false,
        duplicate_reason: None,
    };
    db.add_invoice(&invoice).unwrap();

    // 验证批次统计
    let b1 = db.get_batch(batch1).unwrap();
    assert_eq!(b1.invoice_count, 2);
    assert_eq!(b1.total_amount, Decimal::from_str("200.00").unwrap());

    let b2 = db.get_batch(batch2).unwrap();
    assert_eq!(b2.invoice_count, 1);
    assert_eq!(b2.total_amount, Decimal::from_str("500.00").unwrap());

    // 验证按月份列出批次
    let june_batches = db.list_batches_by_month("2026-06").unwrap();
    assert_eq!(june_batches.len(), 1);
    assert_eq!(june_batches[0].name, "6月报销");
}

#[test]
fn database_persistence_with_invoices() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("ledger.db");

    let batch_id = {
        let db = LedgerDb::new(&db_path).unwrap();
        let batch_id = db.create_batch("持久化测试", "2026-08").unwrap();

        let invoice = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "99999999999999999999".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            amount: Decimal::from_str("123.45").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Flight,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/tmp/persistent.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };
        db.add_invoice(&invoice).unwrap();
        batch_id
    };

    // 重新打开数据库
    let db = LedgerDb::new(&db_path).unwrap();
    let batch = db.get_batch(batch_id).unwrap();
    assert_eq!(batch.invoice_count, 1);

    let invoices = db.list_invoices_by_batch(batch_id).unwrap();
    assert_eq!(invoices.len(), 1);
    assert_eq!(invoices[0].invoice_number, "99999999999999999999");
}
