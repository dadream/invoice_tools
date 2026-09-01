use crate::types::*;
use chrono::{Datelike, Duration, NaiveDate};
use invoice_parse::model::{ParsedInvoice, TicketType, TransportDocumentKind};
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

    // Step 1: 只用有效售票建立路线。退票费、改签费属于行程费用，
    // 但不代表实际发生了一段交通，也不能用于结束一次行程。
    let mut intercity: Vec<(usize, &ParsedInvoice)> = invoices
        .iter()
        .enumerate()
        .filter(|(_, inv)| is_route_anchor_invoice(inv))
        .collect();
    let transport_adjustments = invoices
        .iter()
        .enumerate()
        .filter(|(_, invoice)| is_transport_adjustment(invoice))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();

    intercity.sort_by_key(|(_, inv)| inv.departure_time);

    // Step 2: 切分行程段
    let station_aliases = config.home_station_aliases.as_deref();
    let segments = split_into_segments(&intercity, &config.home_cities, station_aliases);

    // Step 3-4: 为每个行程段挂载住宿和零散票，同时收集歧义
    for seg in segments {
        let (trip, mut ambiguities) =
            build_trip_from_segment(seg, invoices, &config.home_cities, station_aliases);
        trips.push(trip);
        all_ambiguities.append(&mut ambiguities);
    }

    // 没有个人交通票也可能是真实出差，例如公司统一购买机票。异地住宿是
    // 足够强的候选锚点，先形成待确认出差组，再挂载同期同城费用。
    let (hotel_trips, mut hotel_ambiguities) =
        build_hotel_anchored_trips(invoices, &trips, &config.home_cities);
    trips.extend(hotel_trips);
    all_ambiguities.append(&mut hotel_ambiguities);

    // 退票/改签费用按路线与日期挂载到所属出差，但永远不进入路线时间轴。
    all_ambiguities.extend(attach_transport_adjustments(
        &mut trips,
        &transport_adjustments,
        invoices,
        &config.home_cities,
        station_aliases,
    ));

    // 一笔费用只能属于一个归组。日期重叠的多段行程可能同时命中同一笔
    // 餐饮/市内交通；确定性结果先保留最早行程的归属，并把冲突交给用户复核。
    all_ambiguities.extend(deduplicate_trip_members(&mut trips, invoices));

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
    all_ambiguities.extend(detect_ambiguities(
        &trips,
        invoices,
        &config.home_cities,
        station_aliases,
    ));

    (trips, all_ambiguities)
}

pub(crate) fn is_route_anchor_invoice(invoice: &ParsedInvoice) -> bool {
    matches!(invoice.ticket_type, TicketType::Rail | TicketType::Flight)
        && invoice.transport_document_kind.is_route_anchor()
}

fn is_transport_adjustment(invoice: &ParsedInvoice) -> bool {
    matches!(invoice.ticket_type, TicketType::Rail | TicketType::Flight)
        && matches!(
            invoice.transport_document_kind,
            TransportDocumentKind::Refund | TransportDocumentKind::Change
        )
}

fn inferred_hotel_end(invoice: &ParsedInvoice) -> NaiveDate {
    let start = invoice.checkin_date.unwrap_or(invoice.issue_date);
    if invoice.issue_date >= start && invoice.issue_date <= start + Duration::days(14) {
        invoice.issue_date
    } else {
        start
    }
}

fn build_hotel_anchored_trips(
    invoices: &[ParsedInvoice],
    existing_trips: &[Trip],
    home_cities: &[String],
) -> (Vec<Trip>, Vec<Ambiguity>) {
    #[derive(Debug)]
    struct HotelCluster {
        city: String,
        start: NaiveDate,
        end: NaiveDate,
        hotel_ids: Vec<usize>,
    }

    let assigned = existing_trips
        .iter()
        .flat_map(|trip| trip.invoice_ids.iter().copied())
        .collect::<HashSet<_>>();
    let mut hotels = invoices
        .iter()
        .enumerate()
        .filter(|(index, invoice)| {
            !assigned.contains(index)
                && invoice.ticket_type == TicketType::Hotel
                && invoice.checkin_date.is_some()
                && invoice
                    .city
                    .as_deref()
                    .is_some_and(|city| !is_home_city(city, home_cities))
        })
        .collect::<Vec<_>>();
    hotels.sort_by_key(|(_, invoice)| (invoice.city.clone(), invoice.checkin_date));

    let mut clusters = Vec::<HotelCluster>::new();
    for (hotel_id, hotel) in hotels {
        let city = hotel.city.clone().unwrap_or_default();
        let start = hotel.checkin_date.unwrap_or(hotel.issue_date);
        let end = inferred_hotel_end(hotel);
        if let Some(cluster) = clusters
            .iter_mut()
            .rev()
            .find(|cluster| cluster.city == city && start <= cluster.end + Duration::days(7))
        {
            cluster.start = cluster.start.min(start);
            cluster.end = cluster.end.max(end);
            cluster.hotel_ids.push(hotel_id);
        } else {
            clusters.push(HotelCluster {
                city,
                start,
                end,
                hotel_ids: vec![hotel_id],
            });
        }
    }

    let mut trips = Vec::new();
    let mut ambiguities = Vec::new();
    for cluster in clusters {
        let cities = vec![cluster.city.clone()];
        let mut invoice_ids = cluster.hotel_ids.clone();
        invoice_ids.extend(attach_local_expenses(
            &cluster.hotel_ids,
            invoices,
            cluster.start,
            cluster.end,
            &cities,
            home_cities,
        ));
        invoice_ids.sort_unstable();
        invoice_ids.dedup();
        ambiguities.push(Ambiguity {
            kind: AmbiguityKind::MissingTransportEvidence,
            description: format!(
                "{}存在异地住宿，但未发现个人交通票，请确认是否由公司统一购买",
                cluster.city
            ),
            involved_invoice_ids: cluster.hotel_ids,
            candidates: vec![
                "交通由公司统一购买".to_string(),
                "无需个人交通凭证".to_string(),
                "稍后补充交通材料".to_string(),
            ],
        });
        trips.push(Trip {
            kind: TripKind::BusinessTrip {
                start: cluster.start,
                end: cluster.end,
                cities,
            },
            invoice_ids,
            start_date: cluster.start,
            end_date: cluster.end,
            confidence: 0.8,
        });
    }
    (trips, ambiguities)
}

fn attach_transport_adjustments(
    trips: &mut [Trip],
    adjustment_ids: &[usize],
    invoices: &[ParsedInvoice],
    home_cities: &[String],
    station_aliases: Option<&[StationCityAlias]>,
) -> Vec<Ambiguity> {
    let mut ambiguities = Vec::new();
    for adjustment_id in adjustment_ids {
        let adjustment = &invoices[*adjustment_id];
        let mut candidates = trips
            .iter()
            .enumerate()
            .filter(|(_, trip)| matches!(trip.kind, TripKind::BusinessTrip { .. }))
            .map(|(trip_index, trip)| {
                (
                    trip_index,
                    transport_adjustment_score(
                        adjustment,
                        trip,
                        invoices,
                        home_cities,
                        station_aliases,
                    ),
                )
            })
            .filter(|(_, score)| *score >= 8)
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        let Some((best_trip, best_score)) = candidates.first().copied() else {
            ambiguities.push(Ambiguity {
                kind: AmbiguityKind::TransportAdjustmentMatch,
                description: "退票/改签费用未找到可可靠匹配的出差组".to_string(),
                involved_invoice_ids: vec![*adjustment_id],
                candidates: vec!["人工选择所属出差组".to_string()],
            });
            continue;
        };
        let tied = candidates
            .iter()
            .filter(|(_, score)| *score == best_score)
            .map(|(trip_index, _)| *trip_index)
            .collect::<Vec<_>>();
        if tied.len() > 1 {
            ambiguities.push(Ambiguity {
                kind: AmbiguityKind::TransportAdjustmentMatch,
                description: "退票/改签费用同时匹配多个出差组，暂归入最早行程".to_string(),
                involved_invoice_ids: vec![*adjustment_id],
                candidates: tied
                    .iter()
                    .filter_map(|index| trips.get(*index).map(trip_label))
                    .collect(),
            });
        }
        trips[best_trip].invoice_ids.push(*adjustment_id);
        trips[best_trip].invoice_ids.sort_unstable();
        trips[best_trip].invoice_ids.dedup();
    }
    ambiguities
}

fn transport_adjustment_score(
    adjustment: &ParsedInvoice,
    trip: &Trip,
    invoices: &[ParsedInvoice],
    home_cities: &[String],
    station_aliases: Option<&[StationCityAlias]>,
) -> i32 {
    let adjustment_date = adjustment
        .departure_time
        .map(|value| value.date())
        .unwrap_or(adjustment.issue_date);
    let mut score = if adjustment_date >= trip.start_date - Duration::days(1)
        && adjustment_date <= trip.end_date + Duration::days(1)
    {
        6
    } else {
        0
    };
    let departure = extract_departure(adjustment, home_cities, station_aliases);
    let destination = extract_destination(adjustment, home_cities, station_aliases);
    if let TripKind::BusinessTrip { cities, .. } = &trip.kind {
        if departure.as_ref().is_some_and(|city| cities.contains(city)) {
            score += 4;
        }
        if destination
            .as_ref()
            .is_some_and(|city| cities.contains(city))
        {
            score += 4;
        }
    }
    if departure
        .as_deref()
        .is_some_and(|city| is_home_city(city, home_cities))
        || destination
            .as_deref()
            .is_some_and(|city| is_home_city(city, home_cities))
    {
        score += 1;
    }
    let exact_route = trip.invoice_ids.iter().any(|invoice_id| {
        let candidate = &invoices[*invoice_id];
        is_route_anchor_invoice(candidate)
            && extract_departure(candidate, home_cities, station_aliases) == departure
            && extract_destination(candidate, home_cities, station_aliases) == destination
    });
    if exact_route {
        score += 12;
    }
    score
}

fn trip_label(trip: &Trip) -> String {
    match &trip.kind {
        TripKind::BusinessTrip { start, end, cities } => {
            format!("{} 至 {} · {}", start, end, cities.join(" → "))
        }
        TripKind::LocalMonth { year, month } => format!("{year} 年 {month} 月市内消费"),
        TripKind::Excluded => "已排除票据".to_string(),
        TripKind::NeedsReview { reason } => format!("待人工复核：{reason}"),
    }
}

/// 保证每个输入发票索引至多出现在一个行程中。
fn deduplicate_trip_members(trips: &mut [Trip], invoices: &[ParsedInvoice]) -> Vec<Ambiguity> {
    let mut owner_by_invoice = HashMap::<usize, usize>::new();
    let mut candidates_by_invoice = HashMap::<usize, Vec<usize>>::new();

    for (trip_index, trip) in trips.iter_mut().enumerate() {
        trip.invoice_ids.retain(|invoice_id| {
            if let Some(owner) = owner_by_invoice.get(invoice_id).copied() {
                let candidates = candidates_by_invoice
                    .entry(*invoice_id)
                    .or_insert_with(|| vec![owner]);
                if !candidates.contains(&trip_index) {
                    candidates.push(trip_index);
                }
                false
            } else {
                owner_by_invoice.insert(*invoice_id, trip_index);
                true
            }
        });
    }

    candidates_by_invoice
        .into_iter()
        .map(|(invoice_id, candidate_indexes)| {
            let invoice_label = invoices
                .get(invoice_id)
                .map(|invoice| invoice.invoice_number.as_str())
                .filter(|value| !value.is_empty())
                .unwrap_or("无发票号费用");
            Ambiguity {
                kind: AmbiguityKind::MultipleTripMatch,
                description: format!(
                    "费用 {invoice_label} 同时符合多个行程，当前暂归入最早命中的行程"
                ),
                involved_invoice_ids: vec![invoice_id],
                candidates: candidate_indexes
                    .into_iter()
                    .filter_map(|index| trips.get(index).map(trip_label))
                    .collect(),
            }
        })
        .collect()
}

/// 行程段结构
struct Segment {
    intercity_ids: Vec<usize>,
}

/// Step 2: 切分行程段（从常驻城市出发→回到常驻城市）
fn split_into_segments(
    intercity: &[(usize, &ParsedInvoice)],
    home_cities: &[String],
    station_aliases: Option<&[StationCityAlias]>,
) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut current_segment_ids = Vec::new();
    let mut in_trip = false;

    for (idx, inv) in intercity {
        let from_city = extract_departure(inv, home_cities, station_aliases)
            .or_else(|| inv.city.clone())
            .unwrap_or_default();

        // 提取目的城市
        let dest = extract_destination(inv, home_cities, station_aliases);

        // 从常驻城市出发去非常驻城市 = 行程起点
        if is_home_city(&from_city, home_cities) {
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
    station_aliases: Option<&[StationCityAlias]>,
) -> (Trip, Vec<Ambiguity>) {
    let mut invoice_ids = segment.intercity_ids.clone();
    let mut ambiguities = Vec::new();

    if invoice_ids.is_empty() {
        // 空段，返回空行程
        return (
            Trip {
                kind: TripKind::NeedsReview {
                    reason: "空行程段".to_string(),
                },
                invoice_ids: vec![],
                start_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                end_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                confidence: 0.0,
            },
            ambiguities,
        );
    }

    // 计算行程时间范围和城市链
    let first_inv = all_invoices.get(invoice_ids[0]).unwrap();
    let last_inv = all_invoices.get(*invoice_ids.last().unwrap()).unwrap();

    let start = first_inv
        .departure_time
        .map(|dt| dt.date())
        .unwrap_or(first_inv.issue_date);
    let end = last_inv
        .departure_time
        .map(|dt| dt.date())
        .unwrap_or(last_inv.issue_date);

    // 构建城市链（去除中转点，只保留停留点），同时收集中转歧义
    let (cities, mut transfer_ambiguities) = build_city_chain_with_ambiguities(
        &segment.intercity_ids,
        all_invoices,
        home_cities,
        station_aliases,
    );
    ambiguities.append(&mut transfer_ambiguities);

    // Step 3: 挂载住宿票
    let hotel_ids = attach_hotels(&segment.intercity_ids, all_invoices, start, end, &cities);
    invoice_ids.extend(hotel_ids);

    // Step 4: 挂载零散票（出租车、餐饮等）
    let local_ids = attach_local_expenses(
        &segment.intercity_ids,
        all_invoices,
        start,
        end,
        &cities,
        home_cities,
    );
    invoice_ids.extend(local_ids);

    // 按索引排序，保持一致的输出顺序
    invoice_ids.sort_unstable();

    (
        Trip {
            kind: TripKind::BusinessTrip { start, end, cities },
            invoice_ids,
            start_date: start,
            end_date: end,
            confidence: 1.0,
        },
        ambiguities,
    )
}

/// 构建城市链（识别中转点），同时收集歧义
fn build_city_chain_with_ambiguities(
    intercity_ids: &[usize],
    all_invoices: &[ParsedInvoice],
    home_cities: &[String],
    station_aliases: Option<&[StationCityAlias]>,
) -> (Vec<String>, Vec<Ambiguity>) {
    let mut cities = Vec::new();
    let mut ambiguities = Vec::new();

    for i in 0..intercity_ids.len() {
        let inv = &all_invoices[intercity_ids[i]];

        // 提取目的城市
        if let Some(dest) = extract_destination(inv, home_cities, station_aliases) {
            // 如果是常驻城市，不加入城市链（行程终点）
            if is_home_city(&dest, home_cities) {
                break;
            }

            // 检查是否为中转点（下一段是否在短时间内继续）
            if i + 1 < intercity_ids.len() {
                let next_inv = &all_invoices[intercity_ids[i + 1]];

                if let (Some(curr_time), Some(next_time)) =
                    (inv.departure_time, next_inv.departure_time)
                {
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
                        let next_returns_home =
                            extract_destination(next_inv, home_cities, station_aliases)
                                .is_some_and(|next_destination| {
                                    is_home_city(&next_destination, home_cities)
                                });

                        // 同日到达目的地后直接返回常驻城市是常见的短途出差，不能因
                        // 没有酒店而误判为中转。只有后续仍前往外地时才保守标记中转。
                        if !has_hotel && !next_returns_home {
                            // 没有酒店，判为中转，但记录歧义
                            ambiguities.push(Ambiguity {
                                kind: AmbiguityKind::TransferStopover,
                                description: format!(
                                    "在 {} 停留 {} 小时，无酒店记录，判为中转",
                                    dest, interval_hours
                                ),
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
    already_assigned.extend(attach_hotels(
        intercity_ids,
        all_invoices,
        start,
        end,
        cities,
    ));

    let start_with_buffer = start - Duration::days(1);
    let end_with_buffer = end + Duration::days(1);

    all_invoices
        .iter()
        .enumerate()
        .filter(|(idx, inv)| {
            !already_assigned.contains(idx)
                && matches!(
                    inv.ticket_type,
                    TicketType::CityTransport
                        | TicketType::Meal
                        | TicketType::CourierLogistics
                        | TicketType::Other
                )
                && inv.city.is_some()
        })
        .filter(|(_, inv)| {
            // 市内交通发票通常在行程结束后统一开具。行程单已经提供实际上车
            // 时间时，归组必须使用上车日期，不能再用开票日期。
            let date = inv
                .departure_time
                .map(|value| value.date())
                .unwrap_or(inv.issue_date);
            let city = inv.city.as_ref().unwrap();

            // 时间在缓冲范围内
            if date < start_with_buffer || date > end_with_buffer {
                return false;
            }

            // 异地餐饮/零散费用必须匹配行程城市；常驻地只允许机场/车站接驳交通。
            cities.contains(city)
                || (inv.ticket_type == TicketType::CityTransport && is_home_city(city, home_cities))
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// Step 5: 残余票按月归入市内桶
fn group_local_by_month(invoice_ids: &[usize], all_invoices: &[ParsedInvoice]) -> Vec<Trip> {
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
    station_aliases: Option<&[StationCityAlias]>,
) -> Vec<Ambiguity> {
    crate::ambiguity::detect_ambiguities(trips, invoices, home_cities, station_aliases)
}

// ============================================================================
// 辅助函数（供模块内和 ambiguity 模块使用）
// ============================================================================

/// 判断城市是否为常驻城市
pub(crate) fn is_home_city(city: &str, home_cities: &[String]) -> bool {
    home_cities.iter().any(|h| city.contains(h))
}

fn normalize_station_key(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn normalize_route_place(
    place: &str,
    home_cities: &[String],
    station_aliases: Option<&[StationCityAlias]>,
) -> Option<String> {
    let configured_match = station_aliases.and_then(|aliases| {
        let key = normalize_station_key(place);
        aliases
            .iter()
            .find(|alias| normalize_station_key(&alias.station_name) == key)
            .filter(|alias| is_home_city(&alias.city_name, home_cities))
            .map(|alias| alias.city_name.clone())
    });
    configured_match
        .or_else(|| {
            station_aliases
                .is_none()
                .then(|| {
                    invoice_parse::station_city::resolve_home_city_station(place, home_cities)
                        .map(|record| record.city_name.clone())
                })
                .flatten()
        })
        .or_else(|| invoice_parse::field_extractor::normalize_travel_place(place))
}

/// 从交通票中提取出发城市。常驻城市站点库优先于通用名称剥离。
fn extract_departure(
    inv: &ParsedInvoice,
    home_cities: &[String],
    station_aliases: Option<&[StationCityAlias]>,
) -> Option<String> {
    for route in [inv.travel_route.as_ref(), inv.seller_name.as_ref()]
        .into_iter()
        .flatten()
    {
        if let Some(arrow_pos) = route.find('→') {
            let departure = route[..arrow_pos].trim();
            if let Some(city) = normalize_route_place(departure, home_cities, station_aliases) {
                return Some(city);
            }
        }
    }
    None
}

/// 从交通票中提取目的城市。优先使用独立路线事实；兼容旧数据中把路线写入销方的记录。
/// 常驻城市站点库只用于识别返程终点，不把同名外地站误归为常驻城市。
pub(crate) fn extract_destination(
    inv: &ParsedInvoice,
    home_cities: &[String],
    station_aliases: Option<&[StationCityAlias]>,
) -> Option<String> {
    for route in [inv.travel_route.as_ref(), inv.seller_name.as_ref()]
        .into_iter()
        .flatten()
    {
        if let Some(arrow_pos) = route.find('→') {
            let dest = route[arrow_pos + '→'.len_utf8()..].trim();
            if let Some(city) = normalize_route_place(dest, home_cities, station_aliases) {
                return Some(city);
            }
        }
    }
    None
}
