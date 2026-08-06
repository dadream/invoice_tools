mod fixtures;

use chrono::NaiveDate;
use invoice_grouping::{group_invoices, types::*};
use invoice_parse::model::TicketType;
use fixtures::*;

// 辅助函数：快速构造日期
fn d(month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, month, day).unwrap()
}

// 辅助函数：创建测试配置
fn make_config(home_cities: Vec<&str>) -> GroupingConfig {
    GroupingConfig {
        home_cities: home_cities.iter().map(|s| s.to_string()).collect(),
        ambiguity_handler: Box::new(DummyResolver),
    }
}

// ============================================================================
// 确定性场景：标准出差行程
// ============================================================================

#[test]
fn test_single_trip_with_return() {
    // 场景：7月3日去上海，7月5日返回北京
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 5), d(7, 3), "上海", "680.0"),
        make_local(3, d(7, 4), "上海", TicketType::CityTransport, "28.0"),
        make_transport(4, TicketType::Rail, d(7, 5), 16, "上海", "北京", "553.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：1 个出差行程，包含所有 4 张票
    assert_eq!(result.trips.len(), 1);

    match &result.trips[0].kind {
        TripKind::BusinessTrip { start, end, cities } => {
            assert_eq!(*start, d(7, 3));
            assert_eq!(*end, d(7, 5));
            assert_eq!(cities, &vec!["上海".to_string()]);
            assert_eq!(result.trips[0].invoice_ids, vec![0, 1, 2, 3]);
        }
        _ => panic!("期望 BusinessTrip，实际 {:?}", result.trips[0].kind),
    }

    assert_eq!(result.ambiguities.len(), 0);
    assert!(result.overall_confidence > 0.9);
}

#[test]
fn test_multi_city_trip() {
    // 场景：7月3日北京→上海，7月5日上海→深圳，7月7日深圳→北京
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 4), d(7, 3), "上海", "680.0"),
        make_transport(3, TicketType::Flight, d(7, 5), 14, "上海", "深圳", "850.0"),
        make_hotel(4, d(7, 6), d(7, 5), "深圳", "520.0"),
        make_transport(5, TicketType::Flight, d(7, 7), 18, "深圳", "北京", "920.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    assert_eq!(result.trips.len(), 1);
    match &result.trips[0].kind {
        TripKind::BusinessTrip { cities, .. } => {
            assert_eq!(cities, &vec!["上海".to_string(), "深圳".to_string()]);
        }
        _ => panic!("期望 BusinessTrip"),
    }
}

#[test]
fn test_local_month_only() {
    // 场景：纯市内消费，无城际交通
    let invoices = vec![
        make_local(1, d(7, 3), "北京", TicketType::CityTransport, "28.0"),
        make_local(2, d(7, 8), "北京", TicketType::Meal, "156.0"),
        make_local(3, d(7, 15), "北京", TicketType::CityTransport, "32.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    assert_eq!(result.trips.len(), 1);
    match &result.trips[0].kind {
        TripKind::LocalMonth { year, month } => {
            assert_eq!(*year, 2026);
            assert_eq!(*month, 7);
        }
        _ => panic!("期望 LocalMonth"),
    }
}

#[test]
fn test_airport_taxi_attached_to_trip() {
    // 场景：去机场的出租车应归入行程
    let invoices = vec![
        make_local(1, d(7, 3), "北京", TicketType::CityTransport, "85.0"), // 去机场
        make_transport(2, TicketType::Flight, d(7, 3), 14, "北京", "上海", "850.0"),
        make_hotel(3, d(7, 4), d(7, 3), "上海", "680.0"),
        make_transport(4, TicketType::Flight, d(7, 5), 16, "上海", "北京", "850.0"),
        make_local(5, d(7, 5), "北京", TicketType::CityTransport, "90.0"), // 回家
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：所有 5 张票归入同一行程（包括两端出租车）
    assert_eq!(result.trips.len(), 1);
    assert_eq!(result.trips[0].invoice_ids.len(), 5);
}

// ============================================================================
// 确定性场景：中转与停留
// ============================================================================

#[test]
fn test_transfer_stopover_within_4h() {
    // 确定性场景：中转 < 4h，应判定为中转而非行程点
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "郑州", "300.0"),
        make_transport(2, TicketType::Rail, d(7, 3), 12, "郑州", "广州", "450.0"), // 3小时中转
        make_hotel(3, d(7, 4), d(7, 3), "广州", "580.0"),
        make_transport(4, TicketType::Rail, d(7, 5), 16, "广州", "北京", "750.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：1 个行程，郑州不出现在 cities（只是中转）
    match &result.trips[0].kind {
        TripKind::BusinessTrip { cities, .. } => {
            assert_eq!(cities, &vec!["广州".to_string()]);
        }
        _ => panic!("期望 BusinessTrip"),
    }
}

#[test]
fn test_stopover_beyond_12h() {
    // 确定性场景：停留 > 12h，判定为行程点
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "郑州", "300.0"),
        make_hotel(2, d(7, 4), d(7, 3), "郑州", "280.0"), // 在郑州住一晚
        make_transport(3, TicketType::Rail, d(7, 4), 10, "郑州", "广州", "450.0"),
        make_hotel(4, d(7, 5), d(7, 4), "广州", "580.0"),
        make_transport(5, TicketType::Rail, d(7, 6), 16, "广州", "北京", "750.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：郑州和广州都是行程点
    match &result.trips[0].kind {
        TripKind::BusinessTrip { cities, .. } => {
            assert_eq!(cities, &vec!["郑州".to_string(), "广州".to_string()]);
        }
        _ => panic!("期望 BusinessTrip"),
    }
}

// ============================================================================
// 确定性场景：多次出差
// ============================================================================

#[test]
fn test_two_separate_trips() {
    // 场景：7月3-5日去上海，7月10-12日去深圳
    let invoices = vec![
        // 第一趟：上海
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 4), d(7, 3), "上海", "680.0"),
        make_transport(3, TicketType::Rail, d(7, 5), 16, "上海", "北京", "553.0"),
        // 第二趟：深圳
        make_transport(4, TicketType::Flight, d(7, 10), 14, "北京", "深圳", "920.0"),
        make_hotel(5, d(7, 11), d(7, 10), "深圳", "520.0"),
        make_transport(6, TicketType::Flight, d(7, 12), 18, "深圳", "北京", "920.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：2 个独立行程
    assert_eq!(result.trips.len(), 2);
    assert_eq!(result.trips[0].invoice_ids, vec![0, 1, 2]);
    assert_eq!(result.trips[1].invoice_ids, vec![3, 4, 5]);
}

#[test]
fn test_mixed_trips_and_local() {
    // 场景：出差 + 市内消费混合
    let invoices = vec![
        make_local(1, d(7, 1), "北京", TicketType::Meal, "120.0"),
        make_transport(2, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(3, d(7, 4), d(7, 3), "上海", "680.0"),
        make_transport(4, TicketType::Rail, d(7, 5), 16, "上海", "北京", "553.0"),
        make_local(5, d(7, 8), "北京", TicketType::CityTransport, "32.0"),
        make_local(6, d(7, 15), "北京", TicketType::Meal, "88.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：1 个出差行程 + 1 个本地月桶
    assert_eq!(result.trips.len(), 2);
}

// ============================================================================
// 确定性场景：边界情况
// ============================================================================

#[test]
fn test_one_way_with_hotel_only() {
    // 场景：去程票 + 酒店，但无返程票（典型歧义）
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 4), d(7, 3), "上海", "680.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：识别为行程，且必须检测到 NoReturnTicket 歧义
    assert_eq!(result.trips.len(), 1);

    // 必须检测到 NoReturnTicket 歧义
    assert!(!result.ambiguities.is_empty(), "单程+酒店应触发 NoReturnTicket 歧义");
    assert!(
        result.ambiguities.iter().any(|amb| matches!(amb.kind, AmbiguityKind::NoReturnTicket)),
        "必须包含 NoReturnTicket 类型的歧义"
    );
}

#[test]
fn test_empty_invoice_list() {
    // 边界：空输入
    let invoices = vec![];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    assert_eq!(result.trips.len(), 0);
    assert_eq!(result.ambiguities.len(), 0);
}

#[test]
fn test_single_transport_ticket() {
    // 边界：只有一张交通票
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：识别为行程，且必须检测到 NoReturnTicket 歧义
    assert_eq!(result.trips.len(), 1);

    // 必须检测到 NoReturnTicket 歧义
    assert!(!result.ambiguities.is_empty(), "单张交通票应触发 NoReturnTicket 歧义");
    assert!(
        result.ambiguities.iter().any(|amb| matches!(amb.kind, AmbiguityKind::NoReturnTicket)),
        "必须包含 NoReturnTicket 类型的歧义"
    );
}

// ============================================================================
// 歧义场景：无返程票
// ============================================================================

#[test]
fn test_no_return_ticket_ambiguity() {
    // 歧义场景：去上海无返程票，下一趟去深圳
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 4), d(7, 3), "上海", "680.0"),
        // 缺失返程票
        make_transport(3, TicketType::Flight, d(7, 10), 14, "北京", "深圳", "850.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：检测到 NoReturnTicket 歧义
    assert!(!result.ambiguities.is_empty());
    assert!(matches!(
        result.ambiguities[0].kind,
        AmbiguityKind::NoReturnTicket
    ));
}

#[test]
fn test_no_return_at_end_of_month() {
    // 歧义场景：月末去上海无返程票
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 28), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 29), d(7, 28), "上海", "680.0"),
        // 月末无返程
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：识别为行程，且必须检测到 NoReturnTicket 歧义
    assert_eq!(result.trips.len(), 1);

    // 必须检测到 NoReturnTicket 歧义
    assert!(!result.ambiguities.is_empty(), "月末无返程应触发 NoReturnTicket 歧义");
    assert!(
        result.ambiguities.iter().any(|amb| matches!(amb.kind, AmbiguityKind::NoReturnTicket)),
        "必须包含 NoReturnTicket 类型的歧义"
    );
}

// ============================================================================
// 歧义场景：周末夹在两趟中间
// ============================================================================

#[test]
fn test_weekend_between_trips_ambiguity() {
    // 歧义场景：7月4日（周五）去上海，7月7日（周一）去深圳
    // 周末是回家了还是在上海？
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 4), 18, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 5), d(7, 4), "上海", "680.0"),
        // 周末无票据
        make_transport(3, TicketType::Flight, d(7, 7), 9, "北京", "深圳", "850.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：应切分为两个独立行程，且检测到 WeekendBetweenTrips 歧义
    assert_eq!(result.trips.len(), 2, "应识别为两个独立行程");

    // 必须检测到 WeekendBetweenTrips 歧义
    assert!(!result.ambiguities.is_empty(), "周末夹缝应触发 WeekendBetweenTrips 歧义");
    assert!(
        result.ambiguities.iter().any(|amb| matches!(amb.kind, AmbiguityKind::WeekendBetweenTrips)),
        "必须包含 WeekendBetweenTrips 类型的歧义"
    );
}

// ============================================================================
// 歧义场景：中转停留 4-12 小时
// ============================================================================

#[test]
fn test_transfer_stopover_ambiguity() {
    // 歧义场景：中转停留 6 小时（在 4-12h 灰色区间），无酒店
    let invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "郑州", "300.0"),
        make_transport(2, TicketType::Rail, d(7, 3), 15, "郑州", "广州", "450.0"), // 6小时间隔
        make_hotel(3, d(7, 4), d(7, 3), "广州", "580.0"),
        make_transport(4, TicketType::Rail, d(7, 5), 16, "广州", "北京", "750.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：检测到 TransferStopover 歧义（郑州停留 6h 且无酒店）
    assert_eq!(result.trips.len(), 1);

    // 必须检测到 TransferStopover 歧义
    assert!(!result.ambiguities.is_empty(), "4-12h 无酒店应触发 TransferStopover 歧义");
    assert!(
        result.ambiguities.iter().any(|amb| matches!(amb.kind, AmbiguityKind::TransferStopover)),
        "必须包含 TransferStopover 类型的歧义"
    );

    // 郑州应被判为中转点（不在城市链中）
    match &result.trips[0].kind {
        TripKind::BusinessTrip { cities, .. } => {
            assert!(!cities.contains(&"郑州".to_string()), "郑州应被判为中转点，不计入城市链");
        }
        _ => panic!("期望 BusinessTrip"),
    }
}

// ============================================================================
// 歧义场景：同一城市多次往返
// ============================================================================

#[test]
fn test_multiple_visits_same_city_ambiguity() {
    // 歧义场景：7月连续3次去上海
    let invoices = vec![
        // 第一次
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 4), d(7, 3), "上海", "680.0"),
        make_transport(3, TicketType::Rail, d(7, 5), 16, "上海", "北京", "553.0"),
        // 第二次
        make_transport(4, TicketType::Rail, d(7, 8), 9, "北京", "上海", "553.0"),
        make_hotel(5, d(7, 9), d(7, 8), "上海", "680.0"),
        make_transport(6, TicketType::Rail, d(7, 10), 16, "上海", "北京", "553.0"),
        // 第三次
        make_transport(7, TicketType::Rail, d(7, 15), 9, "北京", "上海", "553.0"),
        make_hotel(8, d(7, 16), d(7, 15), "上海", "680.0"),
        make_transport(9, TicketType::Rail, d(7, 17), 16, "上海", "北京", "553.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：识别为 3 个独立行程，可能检测到 MultipleVisitsSameCity 模式
    assert_eq!(result.trips.len(), 3);
    // 频繁往返可能触发歧义标记（可选）
    if !result.ambiguities.is_empty() {
        assert!(matches!(result.ambiguities[0].kind, AmbiguityKind::MultipleVisitsSameCity));
    }
}

// ============================================================================
// 歧义场景：时间重叠
// ============================================================================

#[test]
fn test_time_overlap_ambiguity() {
    // 歧义场景：同一天的票据显示在两个城市
    let invoices = vec![
        make_transport(1, TicketType::Flight, d(7, 3), 9, "北京", "上海", "850.0"),
        make_transport(2, TicketType::Flight, d(7, 3), 10, "北京", "深圳", "920.0"), // 重叠
        make_hotel(3, d(7, 4), d(7, 3), "上海", "680.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：检测到 TimeOverlap 歧义
    assert!(!result.ambiguities.is_empty());
}

// ============================================================================
// 复杂场景：多常驻城市
// ============================================================================

#[test]
fn test_multiple_home_cities() {
    // 场景：常驻北京和上海，深圳是出差
    let invoices = vec![
        make_local(1, d(7, 1), "北京", TicketType::Meal, "120.0"),
        make_transport(2, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"), // 城际但都是常驻
        make_local(3, d(7, 4), "上海", TicketType::Meal, "150.0"),
        make_transport(4, TicketType::Flight, d(7, 6), 14, "上海", "深圳", "850.0"), // 真正出差
        make_hotel(5, d(7, 7), d(7, 6), "深圳", "520.0"),
        make_transport(6, TicketType::Flight, d(7, 8), 18, "深圳", "上海", "850.0"),
    ];

    let config = make_config(vec!["北京", "上海"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：至少包含深圳的出差行程，北京/上海的本地消费可能归入 LocalMonth
    assert!(!result.trips.is_empty());

    // 应有至少一个深圳出差行程
    let has_shenzhen_trip = result.trips.iter().any(|trip| {
        matches!(&trip.kind, TripKind::BusinessTrip { cities, .. } if cities.contains(&"深圳".to_string()))
    });
    assert!(has_shenzhen_trip, "应识别出深圳出差行程");

    // 北京-上海交通票应归入本地或被过滤（因为都是常驻城市）
    let business_trips: Vec<_> = result.trips.iter()
        .filter(|trip| matches!(trip.kind, TripKind::BusinessTrip { .. }))
        .collect();
    assert!(business_trips.len() <= 2, "最多 1-2 个出差行程（深圳，可能包含北京-上海往返）");
}

// ============================================================================
// 复杂场景：长时间跨度
// ============================================================================

#[test]
fn test_long_duration_trip() {
    // 场景：15天的长期出差
    let invoices = vec![
        make_transport(1, TicketType::Flight, d(7, 1), 9, "北京", "上海", "850.0"),
        make_hotel(2, d(7, 5), d(7, 1), "上海", "3200.0"), // 多天酒店
        make_local(3, d(7, 6), "上海", TicketType::Meal, "180.0"),
        make_local(4, d(7, 10), "上海", TicketType::CityTransport, "45.0"),
        make_hotel(5, d(7, 15), d(7, 10), "上海", "2400.0"), // 又续了5天
        make_transport(6, TicketType::Flight, d(7, 16), 18, "上海", "北京", "850.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：识别为一个长行程
    assert_eq!(result.trips.len(), 1);
    match &result.trips[0].kind {
        TripKind::BusinessTrip { start, end, .. } => {
            assert_eq!(*start, d(7, 1));
            assert_eq!(*end, d(7, 16));
        }
        _ => panic!("期望 BusinessTrip"),
    }
}

#[test]
fn test_cross_month_trip() {
    // 场景：跨月出差（7月30日出发，8月2日返回）
    let invoices = vec![
        make_transport(1, TicketType::Flight, d(7, 30), 14, "北京", "上海", "850.0"),
        make_hotel(2, d(7, 31), d(7, 30), "上海", "680.0"),
        make_hotel(3, d(8, 1), d(7, 31), "上海", "680.0"),
        make_transport(4, TicketType::Flight, d(8, 2), 16, "上海", "北京", "850.0"),
    ];

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：识别为跨月的单一行程
    assert_eq!(result.trips.len(), 1);
}

// ============================================================================
// 边界场景：目的地解析失败
// ============================================================================

#[test]
fn test_malformed_destination_parsing() {
    // 边界场景：交通票 seller_name 不含箭头或格式错误
    let mut invoices = vec![
        make_transport(1, TicketType::Rail, d(7, 3), 9, "北京", "上海", "553.0"),
        make_hotel(2, d(7, 4), d(7, 3), "上海", "680.0"),
    ];

    // 修改 seller_name 使其无法解析目的地
    invoices[0].seller_name = Some("北京南站出发".to_string()); // 缺少箭头

    let config = make_config(vec!["北京"]);

    let result = group_invoices(&invoices, &config).unwrap();

    // 期望：仍能正常处理，解析失败时使用 None
    // 由于缺少返程且目的地解析失败，交通票可能被归入本地
    assert!(!result.trips.is_empty());
}
