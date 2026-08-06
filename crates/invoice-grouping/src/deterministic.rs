use crate::types::*;
use chrono::{Datelike, Duration, NaiveDate};
use invoice_parse::model::{ParsedInvoice, TicketType};
use std::collections::{HashMap, HashSet};

const TRANSFER_THRESHOLD_HOURS: i64 = 4;
const STOPOVER_THRESHOLD_HOURS: i64 = 12;

/// 7 步确定性归组算法主入口
pub fn group_deterministic(
    invoices: &[ParsedInvoice],
    config: &GroupingConfig,
) -> (Vec<Trip>, Vec<Ambiguity>) {
    let mut trips = Vec::new();
    let mut all_ambiguities = Vec::new();

    // Step 1: 提取城际交通票并按出发时间排序
    let mut intercity: Vec<(usize, &ParsedInvoice)> = invoices
        .iter()
        .enumerate()
        .filter(|(_, inv)| matches!(inv.ticket_type, TicketType::Rail | TicketType::Flight))
        .collect();

    intercity.sort_by_key(|(_, inv)| inv.departure_time);

    // Step 2: 切分行程段
    let segments = split_into_segments(&intercity, &config.home_cities);

    // Step 3-4: 为每个行程段挂载住宿和零散票，同时收集歧义
    for seg in segments {
        let (trip, mut ambiguities) = build_trip_from_segment(seg, invoices, &config.home_cities);
        trips.push(trip);
        all_ambiguities.append(&mut ambiguities);
    }

    // Step 5: 残余票归入市内桶
    let assigned_ids: HashSet<usize> = trips
        .iter()
        .flat_map(|t| t.invoice_ids.iter().copied())
        .collect();

    let remaining: Vec<usize> = (0..invoices.len())
        .filter(|id| !assigned_ids.contains(id))
        .collect();

    if !remaining.is_empty() {
        trips.extend(group_local_by_month(&remaining, invoices));
    }

    // Step 6: 检测全局歧义
    all_ambiguities.extend(detect_ambiguities(&trips, invoices, &config.home_cities));

    (trips, all_ambiguities)
}

/// 行程段结构
struct Segment {
    intercity_ids: Vec<usize>,
}

/// Step 2: 切分行程段（从常驻城市出发→回到常驻城市）
fn split_into_segments(
    intercity: &[(usize, &ParsedInvoice)],
    home_cities: &[String],
) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut current_segment_ids = Vec::new();
    let mut in_trip = false;

    for (idx, inv) in intercity {
        let from_city = inv.city.as_deref().unwrap_or("");

        // 提取目的城市
        let dest = extract_destination(inv);

        // 从常驻城市出发去非常驻城市 = 行程起点
        if is_home_city(from_city, home_cities) {
            // 检查目的地是否也是常驻城市
            if let Some(ref dest_city) = dest {
                if !is_home_city(dest_city, home_cities) {
                    // 从常驻城市去非常驻城市

                    // 如果已经在行程中，说明上一个行程没有返程票，需要结束上一个行程
                    if in_trip && !current_segment_ids.is_empty() {
                        segments.push(Segment {
                            intercity_ids: current_segment_ids.clone(),
                        });
                        current_segment_ids.clear();
                    }

                    // 开始新行程
                    in_trip = true;
                    current_segment_ids.push(*idx);
                }
                // 否则：常驻城市之间的交通，不算出差
            }
        }
        // 已在行程中，且不是从常驻城市出发
        else if in_trip {
            current_segment_ids.push(*idx);

            // 检查是否回到常驻城市（行程结束）
            if let Some(dest_city) = dest {
                if is_home_city(&dest_city, home_cities) {
                    // 行程结束，保存 segment
                    segments.push(Segment {
                        intercity_ids: current_segment_ids.clone(),
                    });
                    current_segment_ids.clear();
                    in_trip = false;
                }
            }
        }
    }

    // 处理未闭合的行程（无返程票）
    if !current_segment_ids.is_empty() {
        segments.push(Segment {
            intercity_ids: current_segment_ids,
        });
    }

    segments
}

/// Step 3-4: 从行程段构建完整行程（挂载住宿和零散票），并收集歧义
fn build_trip_from_segment(
    segment: Segment,
    all_invoices: &[ParsedInvoice],
    home_cities: &[String],
) -> (Trip, Vec<Ambiguity>) {
    let mut invoice_ids = segment.intercity_ids.clone();
    let mut ambiguities = Vec::new();

    if invoice_ids.is_empty() {
        // 空段，返回空行程
        return (Trip {
            kind: TripKind::NeedsReview {
                reason: "空行程段".to_string(),
            },
            invoice_ids: vec![],
            start_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            confidence: 0.0,
        }, ambiguities);
    }

    // 计算行程时间范围和城市链
    let first_inv = all_invoices.get(invoice_ids[0]).unwrap();
    let last_inv = all_invoices.get(*invoice_ids.last().unwrap()).unwrap();

    let start = first_inv.departure_time
        .map(|dt| dt.date())
        .unwrap_or(first_inv.issue_date);
    let end = last_inv.departure_time
        .map(|dt| dt.date())
        .unwrap_or(last_inv.issue_date);

    // 构建城市链（去除中转点，只保留停留点），同时收集中转歧义
    let (cities, mut transfer_ambiguities) = build_city_chain_with_ambiguities(&segment.intercity_ids, all_invoices, home_cities);
    ambiguities.append(&mut transfer_ambiguities);

    // Step 3: 挂载住宿票
    let hotel_ids = attach_hotels(&segment.intercity_ids, all_invoices, start, end, &cities);
    invoice_ids.extend(hotel_ids);

    // Step 4: 挂载零散票（出租车、餐饮等）
    let local_ids = attach_local_expenses(&segment.intercity_ids, all_invoices, start, end, &cities, home_cities);
    invoice_ids.extend(local_ids);

    // 按索引排序，保持一致的输出顺序
    invoice_ids.sort_unstable();

    (Trip {
        kind: TripKind::BusinessTrip {
            start,
            end,
            cities,
        },
        invoice_ids,
        start_date: start,
        end_date: end,
        confidence: 1.0,
    }, ambiguities)
}

/// 构建城市链（识别中转点），同时收集歧义
fn build_city_chain_with_ambiguities(
    intercity_ids: &[usize],
    all_invoices: &[ParsedInvoice],
    home_cities: &[String],
) -> (Vec<String>, Vec<Ambiguity>) {
    let mut cities = Vec::new();
    let mut ambiguities = Vec::new();

    for i in 0..intercity_ids.len() {
        let inv = &all_invoices[intercity_ids[i]];

        // 提取目的城市
        if let Some(dest) = extract_destination(inv) {
            // 如果是常驻城市，不加入城市链（行程终点）
            if is_home_city(&dest, home_cities) {
                break;
            }

            // 检查是否为中转点（下一段是否在短时间内继续）
            if i + 1 < intercity_ids.len() {
                let next_inv = &all_invoices[intercity_ids[i + 1]];

                if let (Some(curr_time), Some(next_time)) = (inv.departure_time, next_inv.departure_time) {
                    let interval_hours = (next_time - curr_time).num_hours();

                    // < 4h 判定为中转，不加入城市链
                    if interval_hours < TRANSFER_THRESHOLD_HOURS {
                        continue;
                    }

                    // 4-12h 灰色区间，检查是否有酒店
                    if interval_hours < STOPOVER_THRESHOLD_HOURS {
                        let has_hotel = all_invoices.iter().any(|hotel| {
                            hotel.ticket_type == TicketType::Hotel
                                && hotel.city.as_deref() == Some(&dest)
                                && hotel.checkin_date == Some(curr_time.date())
                        });

                        if !has_hotel {
                            // 没有酒店，判为中转，但记录歧义
                            ambiguities.push(Ambiguity {
                                kind: AmbiguityKind::TransferStopover,
                                description: format!("在 {} 停留 {} 小时，无酒店记录，判为中转", dest, interval_hours),
                                involved_invoice_ids: vec![intercity_ids[i], intercity_ids[i + 1]],
                                candidates: vec![
                                    "中转点，不计入行程城市".to_string(),
                                    "行程点，计入行程城市".to_string(),
                                ],
                            });
                            continue;
                        }
                    }
                }
            }

            // 不是中转点，加入城市链（去重）
            if !cities.contains(&dest) {
                cities.push(dest);
            }
        }
    }

    (cities, ambiguities)
}

/// Step 3: 挂载住宿票
fn attach_hotels(
    intercity_ids: &[usize],
    all_invoices: &[ParsedInvoice],
    start: NaiveDate,
    end: NaiveDate,
    cities: &[String],
) -> Vec<usize> {
    let already_assigned: HashSet<usize> = intercity_ids.iter().copied().collect();

    all_invoices
        .iter()
        .enumerate()
        .filter(|(idx, inv)| {
            !already_assigned.contains(idx)
                && inv.ticket_type == TicketType::Hotel
                && inv.checkin_date.is_some()
                && inv.city.is_some()
        })
        .filter(|(_, inv)| {
            let checkin = inv.checkin_date.unwrap();
            let city = inv.city.as_ref().unwrap();

            // 入住日期在行程范围内 且 城市在城市链中
            checkin >= start && checkin <= end && cities.contains(city)
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// Step 4: 挂载零散票
fn attach_local_expenses(
    intercity_ids: &[usize],
    all_invoices: &[ParsedInvoice],
    start: NaiveDate,
    end: NaiveDate,
    cities: &[String],
    home_cities: &[String],
) -> Vec<usize> {
    let mut already_assigned: HashSet<usize> = intercity_ids.iter().copied().collect();

    // 添加已挂载的住宿票
    already_assigned.extend(attach_hotels(intercity_ids, all_invoices, start, end, cities));

    let start_with_buffer = start - Duration::days(1);
    let end_with_buffer = end + Duration::days(1);

    all_invoices
        .iter()
        .enumerate()
        .filter(|(idx, inv)| {
            !already_assigned.contains(idx)
                && matches!(inv.ticket_type, TicketType::CityTransport | TicketType::Meal | TicketType::Other)
                && inv.city.is_some()
        })
        .filter(|(_, inv)| {
            let date = inv.issue_date;
            let city = inv.city.as_ref().unwrap();

            // 时间在缓冲范围内
            if date < start_with_buffer || date > end_with_buffer {
                return false;
            }

            // 城市匹配：行程城市链 或 常驻城市（机场往返）
            cities.contains(city) || is_home_city(city, home_cities)
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// Step 5: 残余票按月归入市内桶
fn group_local_by_month(
    invoice_ids: &[usize],
    all_invoices: &[ParsedInvoice],
) -> Vec<Trip> {
    let mut by_month: HashMap<(i32, u32), Vec<usize>> = HashMap::new();

    for &id in invoice_ids {
        let date = all_invoices[id].issue_date;
        by_month
            .entry((date.year(), date.month()))
            .or_default()
            .push(id);
    }

    by_month
        .into_iter()
        .map(|((year, month), mut ids)| {
            // 排序保持一致性
            ids.sort_unstable();

            // 计算月份的起止日期
            let start = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
            let end = if month == 12 {
                NaiveDate::from_ymd_opt(year, 12, 31).unwrap()
            } else {
                NaiveDate::from_ymd_opt(year, month + 1, 1).unwrap() - Duration::days(1)
            };

            Trip {
                kind: TripKind::LocalMonth { year, month },
                invoice_ids: ids,
                start_date: start,
                end_date: end,
                confidence: 1.0,
            }
        })
        .collect()
}

/// Step 6: 检测歧义（调用独立的 ambiguity 模块）
fn detect_ambiguities(
    trips: &[Trip],
    invoices: &[ParsedInvoice],
    home_cities: &[String],
) -> Vec<Ambiguity> {
    crate::ambiguity::detect_ambiguities(trips, invoices, home_cities)
}

// ============================================================================
// 辅助函数（供模块内和 ambiguity 模块使用）
// ============================================================================

/// 判断城市是否为常驻城市
pub(crate) fn is_home_city(city: &str, home_cities: &[String]) -> bool {
    home_cities.iter().any(|h| city.contains(h))
}

/// 从交通票中提取目的城市（从 seller_name 中解析 "起点 → 终点" 格式）
pub(crate) fn extract_destination(inv: &ParsedInvoice) -> Option<String> {
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
