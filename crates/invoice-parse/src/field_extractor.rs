//! 归组字段提取器
//!
//! 从已解析的发票字段中提取归组引擎需要的信息：
//! - city: 交通票出发城市、酒店城市
//! - departure_time: 交通票出发时间
//! - checkin_date: 酒店入住日期

use chrono::{NaiveDate, NaiveDateTime};
use crate::model::TicketType;
use regex::Regex;
use once_cell::sync::Lazy;

// 匹配 "北京南→上海虹桥" 或 "北京南->上海虹桥" 格式
static CITY_ARROW_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^([^→\->]+)(?:→|->)").unwrap());
// 常见站点后缀（需要剥离），支持多个后缀连续出现
static STATION_SUFFIX_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(南|北|东|西|站|虹桥|浦东|首都|机场)+$").unwrap());

/// 从交通票 seller_name 提取出发城市
///
/// 示例：
/// - "北京南→上海虹桥" → Some("北京")
/// - "上海虹桥→深圳北" → Some("上海")
/// - "中国国际航空" → None（无明确城市信息）
pub fn extract_city(ticket_type: &TicketType, seller_name: &str) -> Option<String> {
    // 只处理交通票
    match ticket_type {
        TicketType::Rail | TicketType::Flight => {}
        _ => return None,
    }

    // 提取箭头前的部分
    let departure = CITY_ARROW_RE
        .captures(seller_name)?
        .get(1)?
        .as_str()
        .trim();

    // 剥离站点后缀
    let city = STATION_SUFFIX_RE.replace(departure, "").to_string();

    if city.is_empty() {
        None
    } else {
        Some(city)
    }
}

/// 从交通票 seller_name 和 issue_date 推导出发时间
///
/// seller_name 中可能包含时间信息（如 "北京南 08:00→上海虹桥 13:28"）
/// 如果没有，回退到 issue_date 的 00:00:00
pub fn extract_departure_time(_seller_name: &str, _issue_date: NaiveDate) -> Option<NaiveDateTime> {
    // TODO: 实现时间提取逻辑
    None
}

/// 从 issue_date 推导酒店入住日期
///
/// 暂时使用 issue_date 作为 checkin_date
/// 后续可以改进（如从 seller_name 提取入住日期范围）
pub fn extract_checkin_date(issue_date: NaiveDate) -> Option<NaiveDate> {
    Some(issue_date)
}
