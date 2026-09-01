use crate::types::*;
use chrono::{Datelike, Duration, NaiveDate};
use invoice_parse::model::{ParsedInvoice, TicketType};
use std::collections::HashMap;

// 使用 deterministic 模块的辅助函数
use crate::deterministic::{extract_destination, is_home_city, is_route_anchor_invoice};

const TRANSFER_THRESHOLD_HOURS: i64 = 4;
const STOPOVER_THRESHOLD_HOURS: i64 = 12;
const MULTIPLE_VISITS_WINDOW_DAYS: i64 = 30;
const MULTIPLE_VISITS_MAX_STAY_DAYS: i64 = 7;
const MULTIPLE_VISITS_MIN_COUNT: usize = 3;

/// 主入口：检测所有 5 类歧义
pub fn detect_ambiguities(
    trips: &[Trip],
    invoices: &[ParsedInvoice],
    home_cities: &[String],
    station_aliases: Option<&[StationCityAlias]>,
) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    // 1. 检测无返程票歧义
    ambiguities.extend(detect_no_return_ticket(
        trips,
        invoices,
        home_cities,
        station_aliases,
    ));

    // 2. 检测时间重叠
    ambiguities.extend(detect_time_overlap(
        trips,
        invoices,
        home_cities,
        station_aliases,
    ));

    // 3. 检测中转停留歧义
    ambiguities.extend(detect_transfer_stopover(
        trips,
        invoices,
        home_cities,
        station_aliases,
    ));

    // 4. 检测周末夹缝歧义
    ambiguities.extend(detect_weekend_between_trips(trips, invoices, home_cities));

    // 5. 检测同城多次往返歧义
    ambiguities.extend(detect_multiple_visits_same_city(
        trips,
        invoices,
        home_cities,
    ));

    ambiguities
}

/// 1. 检测无返程票歧义
fn detect_no_return_ticket(
    trips: &[Trip],
    invoices: &[ParsedInvoice],
    home_cities: &[String],
    station_aliases: Option<&[StationCityAlias]>,
) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    for trip in trips {
        if let TripKind::BusinessTrip { .. } = trip.kind {
            // 检查最后一张交通票是否回到常驻城市
            let intercity_in_trip: Vec<usize> = trip
                .invoice_ids
                .iter()
                .filter(|&&id| is_route_anchor_invoice(&invoices[id]))
                .copied()
                .collect();

            if let Some(&last_id) = intercity_in_trip.last() {
                let last_inv = &invoices[last_id];
                if let Some(dest) = extract_destination(last_inv, home_cities, station_aliases) {
                    if !is_home_city(&dest, home_cities) {
                        ambiguities.push(Ambiguity {
                            kind: AmbiguityKind::NoReturnTicket,
                            description: format!("行程最后一站为 {}，未回到常驻城市", dest),
                            involved_invoice_ids: vec![last_id],
                            candidates: vec![
                                "单程出差，无需返程".to_string(),
                                "返程票丢失，需补录".to_string(),
                            ],
                        });
                    }
                }
            }
        }
    }

    ambiguities
}

/// 2. 检测时间重叠歧义
fn detect_time_overlap(
    _trips: &[Trip],
    invoices: &[ParsedInvoice],
    home_cities: &[String],
    station_aliases: Option<&[StationCityAlias]>,
) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    // 简化实现：检查同一天是否有多张交通票从同一起点出发去不同目的地
    let mut same_day_transports: HashMap<NaiveDate, Vec<(usize, &ParsedInvoice)>> = HashMap::new();

    for (idx, inv) in invoices.iter().enumerate() {
        if is_route_anchor_invoice(inv) {
            if let Some(dt) = inv.departure_time {
                same_day_transports
                    .entry(dt.date())
                    .or_default()
                    .push((idx, inv));
            }
        }
    }

    for (date, transports) in same_day_transports {
        if transports.len() >= 2 {
            // 检查是否有不同目的地
            let destinations: Vec<String> = transports
                .iter()
                .filter_map(|(_, inv)| extract_destination(inv, home_cities, station_aliases))
                .collect();

            if destinations.len() >= 2 && destinations[0] != destinations[1] {
                ambiguities.push(Ambiguity {
                    kind: AmbiguityKind::TimeOverlap,
                    description: format!("{} 有多张交通票去往不同城市", date),
                    involved_invoice_ids: transports.iter().map(|(idx, _)| *idx).collect(),
                    candidates: vec![
                        "第一张票作废/改签".to_string(),
                        "并行出差（特殊情况）".to_string(),
                    ],
                });
            }
        }
    }

    ambiguities
}

/// 3. 检测中转停留歧义（4-12h 灰色区间）
fn detect_transfer_stopover(
    trips: &[Trip],
    invoices: &[ParsedInvoice],
    home_cities: &[String],
    station_aliases: Option<&[StationCityAlias]>,
) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    for trip in trips {
        if let TripKind::BusinessTrip { .. } = trip.kind {
            // 检查行程中的连续交通票
            let intercity_ids: Vec<usize> = trip
                .invoice_ids
                .iter()
                .filter(|&&id| is_route_anchor_invoice(&invoices[id]))
                .copied()
                .collect();

            for i in 0..intercity_ids.len().saturating_sub(1) {
                let curr_inv = &invoices[intercity_ids[i]];
                let next_inv = &invoices[intercity_ids[i + 1]];

                if let (Some(curr_time), Some(next_time)) =
                    (curr_inv.departure_time, next_inv.departure_time)
                {
                    let interval_hours = (next_time - curr_time).num_hours();

                    // 4-12h 灰色区间
                    if (TRANSFER_THRESHOLD_HOURS..STOPOVER_THRESHOLD_HOURS)
                        .contains(&interval_hours)
                    {
                        if let Some(dest) =
                            extract_destination(curr_inv, home_cities, station_aliases)
                        {
                            // 检查该城市是否有酒店发票
                            let has_hotel = invoices.iter().any(|inv| {
                                inv.ticket_type == TicketType::Hotel
                                    && inv.city.as_deref() == Some(&dest)
                                    && inv.checkin_date == Some(curr_time.date())
                            });
                            let next_returns_home =
                                extract_destination(next_inv, home_cities, station_aliases)
                                    .is_some_and(|next_destination| {
                                        is_home_city(&next_destination, home_cities)
                                    });

                            // 无酒店且继续去往另一外地城市时才是中转歧义；若下一程
                            // 直接返回常驻城市，则属于可确定的同日短途出差。
                            if !has_hotel && !next_returns_home {
                                ambiguities.push(Ambiguity {
                                    kind: AmbiguityKind::TransferStopover,
                                    description: format!(
                                        "在 {} 停留 {} 小时，无酒店记录，中转还是行程点？",
                                        dest, interval_hours
                                    ),
                                    involved_invoice_ids: vec![
                                        intercity_ids[i],
                                        intercity_ids[i + 1],
                                    ],
                                    candidates: vec![
                                        "中转站，不计入行程城市".to_string(),
                                        "行程点，在此停留办事".to_string(),
                                    ],
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    ambiguities
}

/// 4. 检测周末夹缝歧义
fn detect_weekend_between_trips(
    trips: &[Trip],
    _invoices: &[ParsedInvoice],
    _home_cities: &[String],
) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    // 找出所有出差行程，按开始时间排序
    let mut business_trips: Vec<(usize, &Trip)> = trips
        .iter()
        .enumerate()
        .filter(|(_, trip)| matches!(trip.kind, TripKind::BusinessTrip { .. }))
        .collect();

    business_trips.sort_by_key(|(_, trip)| trip.start_date);

    // 检查连续两趟行程是否有周末夹缝
    for i in 0..business_trips.len().saturating_sub(1) {
        let (_, trip1) = business_trips[i];
        let (_, trip2) = business_trips[i + 1];

        let gap_days = (trip2.start_date - trip1.end_date).num_days();

        // 2-3 天间隔（可能包含周末）
        if (2..=3).contains(&gap_days) {
            // 检查间隔中是否包含周六或周日
            let mut has_weekend = false;
            for day_offset in 1..=gap_days {
                let check_date = trip1.end_date + Duration::days(day_offset);
                let weekday = check_date.weekday();
                if matches!(weekday, chrono::Weekday::Sat | chrono::Weekday::Sun) {
                    has_weekend = true;
                    break;
                }
            }

            if has_weekend {
                // 收集两趟行程的所有发票 ID
                let mut involved_ids = trip1.invoice_ids.clone();
                involved_ids.extend(trip2.invoice_ids.clone());

                ambiguities.push(Ambiguity {
                    kind: AmbiguityKind::WeekendBetweenTrips,
                    description: format!(
                        "周末夹在两趟行程之间（{} 到 {}，间隔 {} 天）",
                        trip1.end_date, trip2.start_date, gap_days
                    ),
                    involved_invoice_ids: involved_ids,
                    candidates: vec![
                        "周末回家，两趟独立行程".to_string(),
                        "周末仍在外地，合并为一趟".to_string(),
                    ],
                });
            }
        }
    }

    ambiguities
}

/// 5. 检测同城多次往返歧义
fn detect_multiple_visits_same_city(
    trips: &[Trip],
    _invoices: &[ParsedInvoice],
    home_cities: &[String],
) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    // 收集所有出差行程中访问的城市及时间
    let mut city_visits: HashMap<String, Vec<(NaiveDate, NaiveDate, Vec<usize>)>> = HashMap::new();

    for trip in trips {
        if let TripKind::BusinessTrip { start, end, cities } = &trip.kind {
            for city in cities {
                // 排除常驻城市
                if !is_home_city(city, home_cities) {
                    city_visits.entry(city.clone()).or_default().push((
                        *start,
                        *end,
                        trip.invoice_ids.clone(),
                    ));
                }
            }
        }
    }

    // 检测 30 天内多次访问同一城市
    for (city, visits) in city_visits {
        if visits.len() >= MULTIPLE_VISITS_MIN_COUNT {
            // 检查是否在 30 天窗口内
            let mut sorted_visits = visits.clone();
            sorted_visits.sort_by_key(|(start, _, _)| *start);

            for window_start_idx in 0..sorted_visits.len() {
                let window_start_date = sorted_visits[window_start_idx].0;
                let window_end_date =
                    window_start_date + chrono::Duration::days(MULTIPLE_VISITS_WINDOW_DAYS);

                // 统计窗口内的访问次数
                let visits_in_window: Vec<_> = sorted_visits
                    .iter()
                    .filter(|(start, end, _)| {
                        *start >= window_start_date
                            && *start <= window_end_date
                            && (*end - *start).num_days() < MULTIPLE_VISITS_MAX_STAY_DAYS
                    })
                    .collect();

                if visits_in_window.len() >= MULTIPLE_VISITS_MIN_COUNT {
                    // 收集所有相关发票 ID
                    let involved_ids: Vec<usize> = visits_in_window
                        .iter()
                        .flat_map(|(_, _, ids)| ids.clone())
                        .collect();

                    ambiguities.push(Ambiguity {
                        kind: AmbiguityKind::MultipleVisitsSameCity,
                        description: format!(
                            "{} 天内 {} 次访问 {}，每次停留 < {} 天",
                            MULTIPLE_VISITS_WINDOW_DAYS,
                            visits_in_window.len(),
                            city,
                            MULTIPLE_VISITS_MAX_STAY_DAYS
                        ),
                        involved_invoice_ids: involved_ids,
                        candidates: vec![
                            "同一客户多次拜访，合并为一趟".to_string(),
                            "不同客户/项目，拆分为独立行程".to_string(),
                        ],
                    });

                    // 每个城市只报告一次
                    break;
                }
            }
        }
    }

    ambiguities
}
