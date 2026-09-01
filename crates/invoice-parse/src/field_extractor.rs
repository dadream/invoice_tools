//! 归组字段提取器
//!
//! 从已解析的发票字段中提取归组引擎需要的信息：
//! - city: 交通票出发城市、酒店城市
//! - departure_time: 交通票出发时间
//! - checkin_date: 酒店入住日期

use crate::model::{TicketType, TransportDocumentKind};
use chrono::{NaiveDate, NaiveDateTime};
use once_cell::sync::Lazy;
use regex::Regex;

// 匹配 "北京南→上海虹桥" 或 "北京南->上海虹桥" 格式
static CITY_ARROW_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^([^→\->]+)(?:→|->)").unwrap());
// 常见站点后缀（需要剥离），支持多个后缀连续出现
static STATION_SUFFIX_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(南|北|东|西|站|虹桥|浦东|首都|机场)+$").unwrap());
// 匹配时间格式 "08:00" 或 "8:00"
static TIME_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(\d{1,2}):(\d{2})").unwrap());
// 文本票常见路线："北京南→上海虹桥"、"北京首都 - 深圳宝安"。
static EXPLICIT_ROUTE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)([^\s，,;；:：]{2,20})\s*(?:→|->|—|–|\s-\s)\s*([^\s，,;；:：]{2,20})").unwrap()
});
// 铁路文本层常见布局："车次 G13 北京南 上海虹桥"。
static RAIL_ROUTE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)车次\s*[A-Z0-9]+\s+([^\s，,;；:：]{2,20})\s+([^\s，,;；:：]{2,20})").unwrap()
});
static ADDRESS_CITY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?:(?:[\p{Han}]{2,8}省)|(?:(?:内蒙古|广西壮族|西藏|宁夏回族|新疆维吾尔)自治区))?([\p{Han}]{2,8})市",
    )
    .unwrap()
});
static SELLER_ADDRESS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?m)(?:销\s*售\s*方(?:\s*信\s*息)?|销\s*方)[^\r\n]{0,32}?(?:注\s*册\s*地\s*址|地\s*址(?:\s*[、/]\s*电\s*话|\s*电\s*话)?)[\s：:]*([^\r\n]{2,120})",
    )
    .unwrap()
});
static PARENTHESIZED_CITY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[（(]([\p{Han}]{2,8}?)(?:市)?[）)]").unwrap());
static TAX_BUREAU_CITY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)(?:国家税务总局|^)([\p{Han}]{2,8})市税务局").unwrap());

/// 从可见文本或结构化字段文本判断交通票据性质。
/// 结构化解析器会优先读取 `TypeOfBusiness`；本函数用于文本层/OCR 回落。
pub fn extract_transport_document_kind(text: &str) -> TransportDocumentKind {
    if text.contains("退票费")
        || text.contains("退票报销凭证")
        || text.lines().any(|line| line.trim() == "退")
    {
        TransportDocumentKind::Refund
    } else if text.contains("改签费") || text.lines().any(|line| line.trim() == "改") {
        TransportDocumentKind::Change
    } else if text.lines().any(|line| line.trim() == "售") {
        TransportDocumentKind::Sale
    } else {
        TransportDocumentKind::Unknown
    }
}

/// 从明确的地址值中提取地级市/直辖市名称。
///
/// 该函数只处理已经由上层确认为地址字段的文本，不从企业名称猜测城市。
pub fn extract_address_city(address: &str) -> Option<String> {
    ADDRESS_CITY_RE
        .captures(address)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
}

/// 从票面明确标注的销售方地址中提取消费城市。
///
/// 买方地址不能代表消费发生地，因此没有“销售方/销方”语义标签时保持未知，
/// 交由带坐标的解析器或用户审核处理。
pub fn extract_seller_address_city(text: &str) -> Option<String> {
    SELLER_ADDRESS_RE
        .captures_iter(text)
        .filter_map(|captures| captures.get(1))
        .find_map(|value| extract_address_city(value.as_str()))
}

/// 当数电票不展示销售方地址时，仅在两个独立的票面证据一致时回落：
/// 销售方完整法定名称中的括号城市，与票面税务机关的城市必须相同。
/// 单独出现企业名称或税务局都不足以推断消费地。
pub fn extract_consistent_seller_jurisdiction_city(
    text: &str,
    seller_name: Option<&str>,
) -> Option<String> {
    let tax_city = TAX_BUREAU_CITY_RE
        .captures(text)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())?;
    let legal_name_city = seller_name
        .and_then(|name| PARENTHESIZED_CITY_RE.captures(name))
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().trim_end_matches('市').to_string())?;
    (tax_city == legal_name_city).then_some(tax_city)
}

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
    let departure = CITY_ARROW_RE.captures(seller_name)?.get(1)?.as_str().trim();

    normalize_travel_place(departure)
}

/// 将车站/机场名称归一为归组使用的城市名，同时保留原始路线作审核证据。
pub fn normalize_travel_place(place: &str) -> Option<String> {
    let place = place.trim();
    if place.is_empty() {
        return None;
    }

    // 剥离常见站点后缀。
    let mut city = STATION_SUFFIX_RE.replace(place, "").to_string();
    // 航空行程单常省略“机场”二字，只保留机场专名。
    for airport_suffix in [
        "大兴",
        "宝安",
        "白云",
        "双流",
        "天府",
        "萧山",
        "滨海",
        "禄口",
        "江北",
        "咸阳",
        "新郑",
        "高崎",
        "长水",
        "胶东",
        "龙嘉",
        "桃仙",
        "周水子",
    ] {
        if let Some(stripped) = city.strip_suffix(airport_suffix) {
            if stripped.chars().count() >= 2 {
                city = stripped.to_string();
                break;
            }
        }
    }

    // 防止过度剥离（如"济南站"不应变成"济"）
    if city.chars().count() < 2 && place.chars().count() >= 2 {
        // 改用保守策略：只剥离单一后缀词，不连续剥离
        if let Some(stripped) = place.strip_suffix('站') {
            Some(stripped.to_string())
        } else if let Some(stripped) = place.strip_suffix("机场") {
            Some(stripped.to_string())
        } else if let Some(stripped) = place.strip_suffix("虹桥") {
            Some(stripped.to_string())
        } else if let Some(stripped) = place.strip_suffix("浦东") {
            Some(stripped.to_string())
        } else if let Some(stripped) = place.strip_suffix("首都") {
            Some(stripped.to_string())
        } else {
            // 无可剥离的后缀，保持原样
            Some(place.to_string())
        }
    } else if city.is_empty() {
        None
    } else {
        Some(city)
    }
}

/// 从交通票文本中提取并规范化行程路线，不改变销方/承运人字段。
pub fn extract_travel_route(ticket_type: &TicketType, text: &str) -> Option<String> {
    match ticket_type {
        TicketType::Rail | TicketType::Flight => {}
        _ => return None,
    }

    let captures = EXPLICIT_ROUTE_RE.captures(text).or_else(|| {
        (ticket_type == &TicketType::Rail)
            .then(|| RAIL_ROUTE_RE.captures(text))
            .flatten()
    })?;
    let departure = captures.get(1)?.as_str().trim();
    let destination = captures.get(2)?.as_str().trim();
    if departure == destination {
        return None;
    }
    Some(format!("{departure}→{destination}"))
}

/// 从交通票 seller_name 和 issue_date 推导出发时间
///
/// seller_name 中可能包含时间信息（如 "北京南 08:00→上海虹桥 13:28"）
/// 如果没有，回退到 issue_date 的 00:00:00
pub fn extract_departure_time(seller_name: &str, issue_date: NaiveDate) -> Option<NaiveDateTime> {
    // 尝试从 seller_name 提取时间
    if let Some(caps) = TIME_RE.captures(seller_name) {
        let hour: u32 = caps.get(1)?.as_str().parse().ok()?;
        let minute: u32 = caps.get(2)?.as_str().parse().ok()?;

        return issue_date.and_hms_opt(hour, minute, 0);
    }

    // 回退到 issue_date 00:00:00
    issue_date.and_hms_opt(0, 0, 0)
}

/// 从 issue_date 推导酒店入住日期
///
/// 暂时使用 issue_date 作为 checkin_date
/// 后续可以改进（如从 seller_name 提取入住日期范围）
pub fn extract_checkin_date(issue_date: NaiveDate) -> Option<NaiveDate> {
    Some(issue_date)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rail_route_without_using_carrier_name() {
        let text = "承运人 中国铁路\n车次 G13 北京南 上海虹桥\n09:00开";
        assert_eq!(
            extract_travel_route(&TicketType::Rail, text).as_deref(),
            Some("北京南→上海虹桥")
        );
    }

    #[test]
    fn extracts_flight_route_with_spaced_hyphen() {
        let text = "承运人 CZ 航班号 CZ3001\n北京首都 - 深圳宝安";
        assert_eq!(
            extract_travel_route(&TicketType::Flight, text).as_deref(),
            Some("北京首都→深圳宝安")
        );
        assert_eq!(normalize_travel_place("深圳宝安").as_deref(), Some("深圳"));
    }

    #[test]
    fn does_not_treat_non_transport_text_as_route() {
        assert_eq!(extract_travel_route(&TicketType::Meal, "北京 - 上海"), None);
    }

    #[test]
    fn extracts_city_only_from_explicit_seller_address() {
        let text = "购买方地址：北京市海淀区\n销售方地址：上海市浦东新区世纪大道 1 号";
        assert_eq!(extract_seller_address_city(text).as_deref(), Some("上海"));
        assert_eq!(
            extract_address_city("内蒙古自治区赤峰市松山区").as_deref(),
            Some("赤峰")
        );
    }

    #[test]
    fn buyer_address_alone_does_not_become_expense_city() {
        assert_eq!(
            extract_seller_address_city("购买方地址：北京市海淀区"),
            None
        );
    }

    #[test]
    fn requires_tax_jurisdiction_and_legal_name_city_to_agree() {
        assert_eq!(
            extract_consistent_seller_jurisdiction_city(
                "国家税务总局上海市税务局\n销售方信息",
                Some("示例餐饮管理（上海）有限公司")
            )
            .as_deref(),
            Some("上海")
        );
        assert_eq!(
            extract_consistent_seller_jurisdiction_city(
                "国家税务总局北京市税务局",
                Some("示例餐饮管理（上海）有限公司")
            ),
            None
        );
        assert_eq!(
            extract_consistent_seller_jurisdiction_city(
                "国家税务总局上海市税务局",
                Some("示例餐饮管理有限公司")
            ),
            None
        );
    }
}
