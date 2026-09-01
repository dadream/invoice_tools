//! 流水线集成测试
//!
//! 注意：这些测试需要 IMAP 凭证和实际邮箱环境，因此默认标记为 #[ignore]。
//! 运行测试时使用：cargo test --test pipeline_integration -- --ignored

use chrono::NaiveDate;
use invoice_store::{models::BatchStatus, LedgerDb};
use rust_decimal::Decimal;
use std::str::FromStr;

/// 创建测试用的内存数据库
fn create_test_db() -> LedgerDb {
    LedgerDb::new(":memory:").expect("创建内存数据库失败")
}

#[test]
fn test_batch_creation_flow() {
    let db = create_test_db();

    // 创建批次
    let batch_id = db
        .create_batch("测试流水线批次", "2026-08")
        .expect("创建批次失败");

    // 验证批次存在
    let batch = db.get_batch(batch_id).expect("获取批次失败");
    assert_eq!(batch.name, "测试流水线批次");
    assert_eq!(batch.month, "2026-08");
    assert_eq!(batch.status, BatchStatus::Draft);
    assert_eq!(batch.invoice_count, 0);
    assert_eq!(batch.total_amount, Decimal::ZERO);
}

#[test]
fn test_invoice_storage() {
    let db = create_test_db();

    let batch_id = db.create_batch("测试批次", "2026-08").unwrap();

    // 创建测试发票
    let invoice = invoice_store::models::ReportedInvoice {
        id: 0,
        batch_id,
        invoice_number: "12345678901234567890".to_string(),
        issue_date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        amount: Decimal::from_str("100.50").unwrap(),
        tax_amount: Some(Decimal::from_str("9.05").unwrap()),
        buyer_name: Some("测试公司".to_string()),
        seller_name: Some("供应商A".to_string()),
        ticket_type: invoice_store::models::TicketType::Rail,
        city: Some("北京".to_string()),
        departure_time: Some(
            NaiveDate::from_ymd_opt(2026, 8, 1)
                .unwrap()
                .and_hms_opt(9, 30, 0)
                .unwrap(),
        ),
        checkin_date: None,
        file_path: "/tmp/test.xml".to_string(),
        created_at: chrono::Utc::now().naive_utc(),
        updated_at: chrono::Utc::now().naive_utc(),
        verification_result: Some("valid".to_string()),
        is_duplicate: false,
        duplicate_reason: None,
    };

    // 添加发票
    db.add_invoice(&invoice).expect("添加发票失败");

    // 验证发票已保存
    let invoices = db.list_invoices_by_batch(batch_id).expect("查询发票失败");
    assert_eq!(invoices.len(), 1);
    assert_eq!(invoices[0].invoice_number, "12345678901234567890");
    assert_eq!(invoices[0].amount, Decimal::from_str("100.50").unwrap());
}

#[test]
fn test_duplicate_check() {
    let db = create_test_db();

    let batch_id = db.create_batch("测试批次", "2026-08").unwrap();

    let invoice = invoice_store::models::ReportedInvoice {
        id: 0,
        batch_id,
        invoice_number: "12345678901234567890".to_string(),
        issue_date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        amount: Decimal::from_str("100.50").unwrap(),
        tax_amount: None,
        buyer_name: None,
        seller_name: None,
        ticket_type: invoice_store::models::TicketType::Other,
        city: None,
        departure_time: None,
        checkin_date: None,
        file_path: "/tmp/test.xml".to_string(),
        created_at: chrono::Utc::now().naive_utc(),
        updated_at: chrono::Utc::now().naive_utc(),
        verification_result: None,
        is_duplicate: false,
        duplicate_reason: None,
    };

    // 第一次添加应该成功
    db.add_invoice(&invoice).expect("添加发票失败");

    // 检查重复：同一发票号应命中
    let dups = db
        .find_potential_duplicates(
            "12345678901234567890",
            &Decimal::from_str("100.50").unwrap(),
            &NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            "other",
            None,
        )
        .expect("查重失败");
    assert!(!dups.is_empty(), "应该检测到重复发票");

    // 检查不存在的发票号（金额/日期也不同，避免模糊命中）
    let dups = db
        .find_potential_duplicates(
            "99999999999999999999",
            &Decimal::from_str("999.99").unwrap(),
            &NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            "other",
            None,
        )
        .expect("查重失败");
    assert!(dups.is_empty(), "不应该检测到重复");
}

#[test]
#[ignore] // 需要实际 IMAP 环境
fn test_full_pipeline_with_real_email() {
    // 此测试需要环境变量：
    // - INVOICE_IMAP_PASSWORD
    // - TEST_EMAIL (测试邮箱地址)

    let email = std::env::var("TEST_EMAIL").expect("需要设置 TEST_EMAIL 环境变量");
    let password =
        std::env::var("INVOICE_IMAP_PASSWORD").expect("需要设置 INVOICE_IMAP_PASSWORD 环境变量");

    // TODO: 完整的端到端流水线测试
    // 1. 连接 IMAP
    // 2. 采集附件
    // 3. 解析发票
    // 4. 去重检查
    // 5. 归组
    // 6. 保存到数据库
    // 7. 导出 Excel

    println!("使用邮箱: {}", email);
    println!("密码长度: {}", password.len());
}

#[test]
fn test_grouping_integration() {
    use invoice_grouping::group_invoices;
    use invoice_grouping::types::{
        Ambiguity, AmbiguityResolution, AmbiguityResolver, GroupingConfig,
    };
    use invoice_parse::model::{ParseLevel, ParsedInvoice, TicketType};

    /// 测试用的空解决器：不处理任何歧义
    struct NoOpResolver;
    impl AmbiguityResolver for NoOpResolver {
        fn resolve(
            &self,
            _ambiguities: &[Ambiguity],
        ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
            Ok(Vec::new())
        }
    }

    // 创建测试发票
    let invoices = vec![
        ParsedInvoice {
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            total_amount: Decimal::from_str("100.50").unwrap(),
            tax_amount: None,
            tax_rate: None,
            buyer_name: None,
            seller_name: Some("中国铁路".to_string()),
            ticket_type: TicketType::Rail,
            transport_document_kind: Default::default(),
            parse_level: ParseLevel::L0,
            confidence: 1.0,
            city: Some("北京".to_string()),
            travel_route: Some("北京南→上海虹桥".to_string()),
            departure_time: Some(
                NaiveDate::from_ymd_opt(2026, 8, 1)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap(),
            ),
            checkin_date: None,
            source_path: "/tmp/test1.xml".into(),
        },
        ParsedInvoice {
            invoice_number: "98765432109876543210".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 8, 3).unwrap(),
            total_amount: Decimal::from_str("200.00").unwrap(),
            tax_amount: None,
            tax_rate: None,
            buyer_name: None,
            seller_name: Some("如家酒店".to_string()),
            ticket_type: TicketType::Hotel,
            transport_document_kind: Default::default(),
            parse_level: ParseLevel::L0,
            confidence: 1.0,
            city: Some("上海".to_string()),
            travel_route: None,
            departure_time: None,
            checkin_date: Some(NaiveDate::from_ymd_opt(2026, 8, 3).unwrap()),
            source_path: "/tmp/test2.xml".into(),
        },
    ];

    // 归组
    let config = GroupingConfig {
        home_cities: vec!["北京".to_string()],
        home_station_aliases: None,
        ambiguity_handler: Box::new(NoOpResolver),
    };

    let result = group_invoices(&invoices, &config).expect("归组失败");

    // 验证归组结果
    assert!(!result.trips.is_empty(), "应该产生至少一个行程");
    println!("归组产生 {} 个行程", result.trips.len());
    println!("整体置信度: {}", result.overall_confidence);
    println!("未解决歧义: {}", result.ambiguities.len());
}

#[test]
fn test_temp_dir_creation() {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());

    let temp_dir = std::path::PathBuf::from(home)
        .join(".invoice-assistant")
        .join("temp");

    // 创建目录
    std::fs::create_dir_all(&temp_dir).expect("创建临时目录失败");

    // 验证目录存在
    assert!(temp_dir.exists(), "临时目录应该存在");
    assert!(temp_dir.is_dir(), "应该是目录");
}
