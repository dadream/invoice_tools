//! 归组逻辑验证测试：验证行程归组的合理性，统计归组质量指标。
//!
//! 运行：cargo test validation_grouping --release -- --nocapture
//! 输入：reports/parsed_invoices.json
//! 输出：通过 StructuredOutput 返回归组统计

use chrono::NaiveDate;
use invoice_grouping::group_invoices;
use invoice_grouping::types::{
    Ambiguity, AmbiguityResolution, AmbiguityResolver, GroupingConfig, TripKind,
};
use invoice_parse::model::{ParseLevel, ParsedInvoice, TicketType};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

/// 简单的 Dummy Resolver，不解决任何歧义
struct DummyResolver;

impl AmbiguityResolver for DummyResolver {
    fn resolve(
        &self,
        _ambiguities: &[Ambiguity],
    ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
        Ok(vec![])
    }
}

/// 从 JSON 反序列化的发票记录
#[derive(Debug, Clone, Deserialize)]
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

/// 组统计信息
#[derive(Debug, Clone, Serialize)]
struct GroupStats {
    group_id: usize,
    invoice_count: usize,
    time_span_days: i64,
    ticket_types: Vec<String>,
    is_business_trip: bool,
}

#[test]
fn test_grouping_validation() {
    // 1. 读取解析结果
    let json_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parsed_invoices.json");
    let json_content = fs::read_to_string(&json_path).expect("Failed to read parsed_invoices.json");

    let records: Vec<ParsedInvoiceRecord> =
        serde_json::from_str(&json_content).expect("Failed to parse JSON");

    println!("✓ 已加载 {} 张发票记录", records.len());

    // 2. 转换为 ParsedInvoice 结构
    let invoices: Vec<ParsedInvoice> = records.iter().map(convert_record_to_invoice).collect();

    // 3. 配置归组参数（使用常驻城市 "北京"）
    let config = GroupingConfig {
        home_cities: vec!["北京".to_string()],
        home_station_aliases: None,
        ambiguity_handler: Box::new(DummyResolver),
    };

    // 4. 执行归组
    let result = group_invoices(&invoices, &config).expect("Grouping failed");

    println!(
        "✓ 归组完成：{} 个行程，{} 个歧义",
        result.trips.len(),
        result.ambiguities.len()
    );

    // 5. 统计归组质量指标
    let mut group_stats_list = Vec::new();
    let mut max_time_span = 0i64;
    let mut total_invoice_count = 0usize;

    for (idx, trip) in result.trips.iter().enumerate() {
        let invoice_count = trip.invoice_ids.len();
        total_invoice_count += invoice_count;

        let time_span_days = (trip.end_date - trip.start_date).num_days();
        if time_span_days > max_time_span {
            max_time_span = time_span_days;
        }

        // 收集该组中的票种
        let mut ticket_types = std::collections::HashSet::new();
        for &inv_id in &trip.invoice_ids {
            let ticket_type = format!("{:?}", invoices[inv_id].ticket_type);
            ticket_types.insert(ticket_type);
        }

        let is_business_trip = matches!(trip.kind, TripKind::BusinessTrip { .. });

        group_stats_list.push(GroupStats {
            group_id: idx,
            invoice_count,
            time_span_days,
            ticket_types: ticket_types.into_iter().collect(),
            is_business_trip,
        });

        // 验证时间跨度合理性
        assert!(
            time_span_days <= 30,
            "Group {} has unreasonable time span: {} days",
            idx,
            time_span_days
        );
    }

    let avg_group_size = if result.trips.is_empty() {
        0.0
    } else {
        total_invoice_count as f64 / result.trips.len() as f64
    };

    // 6. 计算票种一致性率（出差行程中，多少组的票种是一致的）
    let business_trips: Vec<_> = group_stats_list
        .iter()
        .filter(|g| g.is_business_trip)
        .collect();

    let consistent_trips = business_trips
        .iter()
        .filter(|g| {
            // 如果只有一种票种，认为是一致的
            g.ticket_types.len() == 1
        })
        .count();

    let ticket_type_consistency_rate = if business_trips.is_empty() {
        0.0
    } else {
        consistent_trips as f64 / business_trips.len() as f64
    };

    // 7. 打印报告
    println!("\n=== 归组质量报告 ===");
    println!("输入发票数：{}", invoices.len());
    println!("归组数量：{}", result.trips.len());
    println!("平均组大小：{:.2}", avg_group_size);
    println!("最大时间跨度：{} 天", max_time_span);
    println!("票种一致性率：{:.2}%", ticket_type_consistency_rate * 100.0);
    println!("\n各组详情：");
    for g in &group_stats_list {
        println!(
            "  Group {}: {} 张发票, {} 天, 票种: {:?}, 出差: {}",
            g.group_id, g.invoice_count, g.time_span_days, g.ticket_types, g.is_business_trip
        );
    }

    println!("\n歧义列表：");
    for (idx, amb) in result.ambiguities.iter().enumerate() {
        println!("  Ambiguity {}: {:?} - {}", idx, amb.kind, amb.description);
    }

    // 8. 验收标准
    println!("\n=== 验收检查 ===");
    println!("✓ 无 panic 或错误");
    println!("✓ 所有组时间跨度 <= 30 天");

    // 断言验收条件
    assert!(!result.trips.is_empty(), "No groups created");
    assert!(max_time_span <= 30, "Time span exceeds 30 days");
}

/// 将 JSON 记录转换为 ParsedInvoice
fn convert_record_to_invoice(record: &ParsedInvoiceRecord) -> ParsedInvoice {
    let ticket_type = match record.ticket_type.to_lowercase().as_str() {
        "rail" => TicketType::Rail,
        "flight" => TicketType::Flight,
        "hotel" => TicketType::Hotel,
        "citytransport" => TicketType::CityTransport,
        "meal" => TicketType::Meal,
        _ => TicketType::Other,
    };

    let parse_level = match record.parse_level.as_str() {
        "L0" => ParseLevel::L0,
        "L1" => ParseLevel::L1,
        "L2" => ParseLevel::L2,
        "L4" => ParseLevel::L4,
        _ => ParseLevel::L4,
    };

    ParsedInvoice {
        invoice_number: record.invoice_number.clone(),
        issue_date: NaiveDate::from_str(&record.issue_date).expect("Invalid issue_date format"),
        total_amount: Decimal::from_str(&record.total_amount).expect("Invalid total_amount"),
        tax_amount: record
            .tax_amount
            .as_ref()
            .and_then(|s| Decimal::from_str(s).ok()),
        tax_rate: None,
        buyer_name: record.buyer_name.clone(),
        seller_name: record.seller_name.clone(),
        ticket_type,
        transport_document_kind: Default::default(),
        parse_level,
        confidence: record.confidence,
        source_path: PathBuf::from(&record.file),
        city: None,
        travel_route: record.seller_name.clone(),
        departure_time: None,
        checkin_date: None,
    }
}
