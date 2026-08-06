use crate::types::*;
use chrono::{Datelike, NaiveDate};
use invoice_parse::model::{ParsedInvoice, TicketType};
use std::collections::HashMap;

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
) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    // 1. 检测无返程票歧义
    ambiguities.extend(detect_no_return_ticket(trips, invoices, home_cities));

    // 2. 检测时间重叠
    ambiguities.extend(detect_time_overlap(trips, invoices));

    // 3. 检测中转停留歧义
    ambiguities.extend(detect_transfer_stopover(trips, invoices));

    // 4. 检测周末夹缝歧义
    ambiguities.extend(detect_weekend_between_trips(trips, invoices, home_cities));

    // 5. 检测同城多次往返歧义
    ambiguities.extend(detect_multiple_visits_same_city(trips, invoices, home_cities));

    ambiguities
}

/// 1. 检测无返程票歧义
fn detect_no_return_ticket(
    trips: &[Trip],
    invoices: &[ParsedInvoice],
    home_cities: &[String],
) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    for trip in trips {
        if let TripKind::BusinessTrip { .. } = trip.kind {
            // 检查最后一张交通票是否回到常驻城市
            let intercity_in_trip: Vec<usize> = trip
                .invoice_ids
                .iter()
                .filter(|&&id| {
                    matches!(
                        invoices[id].ticket_type,
                        TicketType::Rail | TicketType::Flight
                    )
                })
                .copied()
                .collect();

            if let Some(&last_id) = intercity_in_trip.last() {
                let last_inv = &invoices[last_id];
                if let Some(dest) = extract_destination(last_inv) {
                    if !is_home_city(&dest, home_cities) {
                        ambiguities.push(Ambiguity {
                            kind: AmbiguityKind::NoReturnTicket,
                            description: format!("行程最后一站为 {}，未回到常驻城市", dest),
                            involved_invoice_ids: vec![last_id],
                            candidates: vec![
                                "行程未结束，等待下月数据".to_string(),
                                "行程已结束，未录入返程票".to_string(),
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
fn detect_time_overlap(_trips: &[Trip], invoices: &[ParsedInvoice]) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    // 简化实现：检查同一天是否有多张交通票从同一起点出发去不同目的地
    let mut same_day_transports: HashMap<NaiveDate, Vec<(usize, &ParsedInvoice)>> = HashMap::new();

    for (idx, inv) in invoices.iter().enumerate() {
        if matches!(inv.ticket_type, TicketType::Rail | TicketType::Flight) {
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
                .filter_map(|(_, inv)| extract_destination(inv))
                .collect();

            if destinations.len() >= 2 && destinations[0] != destinations[1] {
                ambiguities.push(Ambiguity {
                    kind: AmbiguityKind::TimeOverlap,
                    description: format!("{} 有多张交通票去往不同城市", date),
                    involved_invoice_ids: transports.iter().map(|(idx, _)| *idx).collect(),
                    candidates: vec![
                        "同事代订票据".to_string(),
                        "退改签重复".to_string(),
                    ],
                });
            }
        }
    }

    ambiguities
}

/// 3. 检测中转停留歧义（4-12h 灰色区间）
fn detect_transfer_stopover(trips: &[Trip], invoices: &[ParsedInvoice]) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    for trip in trips {
        if let TripKind::BusinessTrip { .. } = trip.kind {
            // 检查行程中的连续交通票
            let intercity_ids: Vec<usize> = trip
                .invoice_ids
                .iter()
                .filter(|&&id| {
                    matches!(
                        invoices[id].ticket_type,
                        TicketType::Rail | TicketType::Flight
                    )
                })
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
                    if interval_hours >= TRANSFER_THRESHOLD_HOURS
                        && interval_hours < STOPOVER_THRESHOLD_HOURS
                    {
                        if let Some(dest) = extract_destination(curr_inv) {
                            ambiguities.push(Ambiguity {
                                kind: AmbiguityKind::TransferStopover,
                                description: format!(
                                    "在 {} 停留 {} 小时，中转还是行程点？",
                                    dest, interval_hours
                                ),
                                involved_invoice_ids: vec![intercity_ids[i], intercity_ids[i + 1]],
                                candidates: vec![
                                    "中转点，不计入行程城市".to_string(),
                                    "行程点，计入行程城市".to_string(),
                                ],
                            });
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
    _trips: &[Trip],
    invoices: &[ParsedInvoice],
    home_cities: &[String],
) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();

    // 找出所有从常驻城市出发的交通票
    let departures_from_home: Vec<(usize, &ParsedInvoice)> = invoices
        .iter()
        .enumerate()
        .filter(|(_, inv)| {
            matches!(inv.ticket_type, TicketType::Rail | TicketType::Flight)
                && inv
                    .city
                    .as_ref()
                    .map(|c| is_home_city(c, home_cities))
                    .unwrap_or(false)
        })
        .collect();

    // 检查连续两次出发是否有周末夹缝
    for i in 0..departures_from_home.len().saturating_sub(1) {
        let (idx1, inv1) = departures_from_home[i];
        let (idx2, inv2) = departures_from_home[i + 1];

        if let (Some(time1), Some(time2)) = (inv1.departure_time, inv2.departure_time) {
            let gap_days = (time2.date() - time1.date()).num_days();

            // 2-4 天间隔（可能包含周末）
            if gap_days >= 2 && gap_days <= 4 {
                // 检查是否是周五到周一的模式
                let weekday1 = time1.date().weekday();
                let weekday2 = time2.date().weekday();

                if weekday1.number_from_monday() >= 5 && weekday2.number_from_monday() == 1 {
                    ambiguities.push(Ambiguity {
                        kind: AmbiguityKind::WeekendBetweenTrips,
                        description: format!(
                            "周末夹在两次出发之间（{} 到 {}）",
                            time1.date(),
                            time2.date()
                        ),
                        involved_invoice_ids: vec![idx1, idx2],
                        candidates: vec![
                            "周末回家了，两次独立出差".to_string(),
                            "周末留在外地，连续出差".to_string(),
                        ],
                    });
                }
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
                    city_visits
                        .entry(city.clone())
                        .or_default()
                        .push((*start, *end, trip.invoice_ids.clone()));
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
                let window_end_date = window_start_date
                    + chrono::Duration::days(MULTIPLE_VISITS_WINDOW_DAYS);

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

// ============================================================================
// 辅助函数
// ============================================================================

/// 判断城市是否为常驻城市
fn is_home_city(city: &str, home_cities: &[String]) -> bool {
    home_cities.iter().any(|h| city.contains(h))
}

/// 从交通票中提取目的城市（从 seller_name 中解析 "起点 → 终点" 格式）
fn extract_destination(inv: &ParsedInvoice) -> Option<String> {
    // 从 seller_name 中解析 "起点 → 终点" 格式
    if let Some(ref seller) = inv.seller_name {
        if let Some(arrow_pos) = seller.find("→") {
            let dest = seller[arrow_pos + "→".len()..].trim();
            if !dest.is_empty() {
                return Some(dest.to_string());
            }
        }
    }
    None
}
