use crate::expense_classifier;
use crate::field_extractor;
use crate::manifest::TagHints;
use crate::model::{ParseError, ParseLevel, ParsedInvoice, TicketType};
use crate::xml::{parse_amount, parse_date};
use chrono::NaiveDate;
use regex::Regex;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportingDocumentFacts {
    /// ride_hailing_itinerary / courier_detail / hotel_folio
    pub kind: String,
    /// didi / caocao / courier / hotel
    pub provider: String,
    pub total_amount: Decimal,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub cities: Vec<String>,
    /// 行程单路线中明确出现的酒店名称，用于给同批次同名住宿票补充城市和入住日期。
    pub hotel_mentions: Vec<String>,
}

/// Main entry point for PDF parsing. Extracts text layer and dispatches to ticket-type-specific parser.
pub fn parse_invoice_pdf(
    pdf_bytes: &[u8],
    path: &Path,
    _hints: &TagHints,
    ticket_type: TicketType,
) -> Result<ParsedInvoice, ParseError> {
    let text = extract_text(pdf_bytes, path)?;

    match ticket_type {
        TicketType::Rail => parse_rail_itinerary(&text, path),
        TicketType::Flight => parse_flight_itinerary(&text, path),
        // 邮箱采集阶段的发件人/文件名分类提示不会跨越文件检查点持久化，因此
        // 对 Other 再根据票面强特征识别铁路/航空票，避免它们先被通用 VAT
        // 解析器以 Other 类型接收。
        TicketType::Other => parse_detected_travel_invoice_text(&text, path)
            .or_else(|_| parse_vat_invoice_text(&text, path)),
        // 非城际交通票仍使用通用 VAT 解析器。
        _ => parse_vat_invoice_text(&text, path),
    }
}

/// 仅使用高区分度票面锚点判断城际交通票。返回 None 时必须继续通用发票路径，
/// 不能仅凭销售方名称中的“铁路”或普通文本中的“行程”做票种判断。
pub fn detect_travel_ticket_type(text: &str) -> Option<TicketType> {
    let compact = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let rail = compact.contains("铁路电子客票")
        || (compact.contains("车次")
            && compact.contains("票价")
            && (compact.contains("开车时间") || compact.contains("乘车日期"))
            && (compact.contains("发票号码") || compact.contains("电子客票")));
    if rail {
        return Some(TicketType::Rail);
    }

    let flight = compact.contains("航空运输电子客票行程单")
        || (compact.contains("电子客票号码")
            && compact.contains("民航发展基金")
            && compact.contains("填开日期"));
    flight.then_some(TicketType::Flight)
}

/// 在通用 VAT 解析之前尝试明确的铁路/航空文本票。未命中时返回稳定的
/// MissingField，供上层继续降级，不暴露任何原文内容。
pub fn parse_detected_travel_invoice_text(
    text: &str,
    path: &Path,
) -> Result<ParsedInvoice, ParseError> {
    match detect_travel_ticket_type(text) {
        Some(TicketType::Rail) => parse_rail_itinerary(text, path),
        Some(TicketType::Flight) => parse_flight_itinerary(text, path),
        _ => Err(ParseError::MissingField {
            path: path.to_path_buf(),
            field: "travel_ticket_markers".to_string(),
        }),
    }
}

pub fn extract_text(pdf_bytes: &[u8], path: &Path) -> Result<String, ParseError> {
    pdf_extract::extract_text_from_mem(pdf_bytes).map_err(|e| ParseError::MalformedFormat {
        path: path.to_path_buf(),
        format: "PDF",
        detail: format!("文本层提取失败: {e}"),
    })
}

/// 判断 PDF 是否含可提取的文本层。
/// 用于路由：无文本层的走 L2 OCR。
pub fn has_text_layer(pdf_bytes: &[u8]) -> bool {
    match pdf_extract::extract_text_from_mem(pdf_bytes) {
        // 少于 20 个非空白字符视为没有有效文本层（纯扫描件）
        Ok(text) => text.chars().filter(|c| !c.is_whitespace()).count() >= 20,
        Err(_) => false,
    }
}

/// 判断文本 PDF 是否是明确的网约车行程单辅助材料，而不是发票原件。
///
/// 该判定只用于三个结构化/文本解析路径全部失败后的 OCR 路由。必须同时命中
/// “行程单”和已知网约车平台，并且不能出现任何发票/税务锚点。条件故意保守：
/// 普通交通发票、航空/铁路行程单、酒店支付记录或未知平台一律继续原有 OCR 降级。
pub fn is_unambiguous_ride_hailing_itinerary(text: &str) -> bool {
    let lower = text.to_lowercase();
    let has_trip_sheet = lower.contains("行程单");
    let has_ride_hailing_platform = [
        "滴滴",
        "高德",
        "美团",
        "曹操出行",
        "t3出行",
        "首汽约车",
        "享道出行",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let has_invoice_anchor = [
        "发票",
        "invoice",
        "价税合计",
        "购买方",
        "销售方",
        "纳税人识别号",
        "税额",
        "税率",
    ]
    .iter()
    .any(|marker| lower.contains(marker));

    has_trip_sheet && has_ride_hailing_platform && !has_invoice_anchor
}

/// 从不形成独立费用的行程单、订单明细和酒店结账单中提取匹配事实。
/// 这些事实只用于把材料挂到已经解析出的同金额费用；没有唯一匹配时必须保留待办。
pub fn extract_supporting_document_facts(text: &str) -> Option<SupportingDocumentFacts> {
    let compact = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    let (kind, provider, amount_labels): (&str, &str, &[&str]) =
        if is_unambiguous_ride_hailing_itinerary(text) {
            (
                "ride_hailing_itinerary",
                if compact.contains("滴滴") {
                    "didi"
                } else {
                    "caocao"
                },
                &["合计"],
            )
        } else if (compact.contains("顺丰同城")
            && (compact.contains("运单") || compact.contains("订单详情")))
            || (compact.contains("运单起止日期") && compact.contains("帮我送"))
        {
            // 部分顺丰同城订单详情页本身不重复印平台名称，只有原始文件名带平台名；
            // “运单起止日期 + 帮我送”仍是足够明确的同城配送明细结构。
            ("courier_detail", "courier", &["合计"])
        } else if (compact.contains("结账单")
            && (compact.contains("入住日期")
                || compact.contains("到店时间")
                || compact.to_ascii_lowercase().contains("arrivaltime")))
            || ((compact.contains("账单号码")
                || compact.to_ascii_lowercase().contains("invoiceno"))
                && (compact.contains("到店日期")
                    || compact.to_ascii_lowercase().contains("arrival"))
                && (compact.contains("离店日期")
                    || compact.to_ascii_lowercase().contains("departure"))
                && (compact.contains("应付总额")
                    || compact.to_ascii_lowercase().contains("balance")))
        {
            (
                "hotel_folio",
                "hotel",
                &["消费合计", "付款合计", "总计/Total", "总计"],
            )
        } else {
            return None;
        };

    let total_amount = amount_after_labels(&compact, amount_labels).or_else(|| {
        (kind == "hotel_folio")
            .then(|| largest_decimal_amount(text))
            .flatten()
    })?;
    let (start_date, end_date) = supporting_date_range(text, kind);
    let cities = match kind {
        "ride_hailing_itinerary" => ride_hailing_cities(text),
        "hotel_folio" => hotel_folio_cities(text),
        _ => Vec::new(),
    };
    let hotel_mentions = if kind == "ride_hailing_itinerary" {
        ride_hailing_hotel_mentions(text)
    } else {
        Vec::new()
    };
    Some(SupportingDocumentFacts {
        kind: kind.to_string(),
        provider: provider.to_string(),
        total_amount,
        start_date,
        end_date,
        cities,
        hotel_mentions,
    })
}

fn largest_decimal_amount(text: &str) -> Option<Decimal> {
    Regex::new(r"[¥￥]?\s*([0-9][0-9,]*\.[0-9]{2})")
        .expect("内置金额正则有效")
        .captures_iter(text)
        .filter_map(|captures| captures.get(1))
        .filter_map(|value| value.as_str().replace(',', "").parse::<Decimal>().ok())
        .max()
}

fn amount_after_labels(compact: &str, labels: &[&str]) -> Option<Decimal> {
    let amount_regex = Regex::new(r"[¥￥]?([0-9][0-9,]*\.[0-9]{2})").expect("内置金额正则有效");
    for label in labels {
        let Some(position) = compact.find(label) else {
            continue;
        };
        let start = position + label.len();
        let tail = compact[start..].chars().take(48).collect::<String>();
        let amount = amount_regex
            .captures(&tail)
            .and_then(|captures| captures.get(1))
            .map(|value| value.as_str().replace(',', ""))
            .and_then(|value| value.parse::<Decimal>().ok());
        if amount.is_some() {
            return amount;
        }
    }
    None
}

fn supporting_date_range(text: &str, kind: &str) -> (Option<NaiveDate>, Option<NaiveDate>) {
    let patterns = if kind == "ride_hailing_itinerary" {
        vec![r"行程起止日期[：:]\s*(\d{4}-\d{2}-\d{2})\s*至\s*(\d{4}-\d{2}-\d{2})"]
    } else {
        vec![
            r"(?s)入住日期\s*[：:]\s*(\d{4}-\d{2}-\d{2}).*?离店日期\s*[：:]\s*(\d{4}-\d{2}-\d{2})",
            r"(?s)Arrival Time/到店时间\s*(\d{4}/\d{1,2}/\d{1,2}).*?Departure Time/离店时间\s*(\d{4}/\d{1,2}/\d{1,2})",
            r"(?s)到店日期/Arrival\s*[：:]?\s*(\d{1,2}/\d{1,2}/\d{4}).*?离店日期/Departure\s*[：:]?\s*(\d{1,2}/\d{1,2}/\d{4})",
        ]
    };
    for pattern in patterns {
        let regex = Regex::new(pattern).expect("内置材料日期正则有效");
        let Some(captures) = regex.captures(text) else {
            continue;
        };
        let parse = |value: &str| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .or_else(|_| NaiveDate::parse_from_str(value, "%Y/%m/%d"))
                .or_else(|_| NaiveDate::parse_from_str(value, "%d/%m/%Y"))
                .ok()
        };
        return (
            captures.get(1).and_then(|value| parse(value.as_str())),
            captures.get(2).and_then(|value| parse(value.as_str())),
        );
    }
    (None, None)
}

fn ride_hailing_cities(text: &str) -> Vec<String> {
    // 滴滴 PDF 的文本层经常把车型、星期、分钟和“市”拆到下一行，因此不能
    // 依赖整行结构；以上车日期/时间和星期为锚点，只读取它们之后的第一个词。
    let row = Regex::new(r"\d{2}-\d{2}\s+\d{2}:\s*\d{2}\s+周\s*[一二三四五六日天]\s+([^\s|]+)")
        .expect("内置行程城市正则有效");
    let mut cities = Vec::new();
    for city in row
        .captures_iter(text)
        .filter_map(|captures| captures.get(1))
        .map(|value| value.as_str().trim_end_matches('市').to_string())
        .filter(|value| !value.is_empty())
    {
        if !cities.contains(&city) {
            cities.push(city);
        }
    }
    cities
}

fn hotel_folio_cities(text: &str) -> Vec<String> {
    // “品牌 + 城市 + 市政府酒店”是高置信度物业名；不从普通酒店名中猜测城市，
    // 避免把商圈（如“虹桥”）误当作城市。
    let property = Regex::new(
        r"(?:全季|汉庭|如家|亚朵|万豪|希尔顿|喜来登|假日|智选假日)([\p{Han}]{2,6})市政府酒店",
    )
    .expect("内置酒店城市正则有效");
    let address = Regex::new(r"(?:地址|Address)[：:]?\s*(?:[\p{Han}]{2,8}省)?([\p{Han}]{2,6})市")
        .expect("内置酒店地址城市正则有效");
    // 扫描结账单的浅色页脚地址可能无法被 OCR 识别。酒店完整物业名若明确以
    // 直辖市开头（如“上海虹桥维景酒店”），城市不是商圈猜测，可作为保守回落。
    let municipality_property =
        Regex::new(r"(?m)^(北京|上海|天津|重庆)市?[\p{Han}A-Za-z0-9·（）()]{1,24}酒店$")
            .expect("内置直辖市酒店名称正则有效");
    property
        .captures_iter(text)
        .chain(address.captures_iter(text))
        .chain(municipality_property.captures_iter(text))
        .filter_map(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn ride_hailing_hotel_mentions(text: &str) -> Vec<String> {
    let hotel =
        Regex::new(r"[\p{Han}A-Za-z0-9·（）()]{2,24}酒店").expect("内置行程酒店名称正则有效");
    hotel
        .find_iter(text)
        .map(|value| value.as_str().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// 在文本中按标签抓取其后紧跟的值。
/// 标签与值之间允许空格、全角空格、冒号、少量换行。
fn capture_after(text: &str, labels: &[&str], value_pattern: &str) -> Option<String> {
    for label in labels {
        // Try immediate match (same line or next line)
        let pattern = format!(
            r"{}[\s：:]*\n?[\s：:]*({})",
            regex::escape(label),
            value_pattern
        );
        let re = Regex::new(&pattern).expect("内置正则应有效");
        if let Some(caps) = re.captures(text) {
            return Some(caps[1].trim().to_string());
        }
    }
    None
}

/// 抓取“发票总金额（元）”这类带明确币种单位的总额。
///
/// 只接受固定的总额标签和固定币种单位，避免把运单明细中的任意数字误当作发票总额。
fn capture_total_after_currency_unit(text: &str) -> Option<String> {
    for label in ["发票总金额", "总金额"] {
        let pattern = format!(
            r"{}[\s：:]*[（(]\s*(?:元|人民币|CNY|RMB)\s*[）)][\s：:]*({})",
            regex::escape(label),
            AMOUNT_PATTERN
        );
        let re = Regex::new(&pattern).expect("内置正则应有效");
        if let Some(caps) = re.captures(text) {
            return Some(caps[1].trim().to_string());
        }
    }
    None
}

/// 在整个文本中查找第一个匹配指定模式的值（不依赖标签）。
fn find_first_match(text: &str, pattern: &str) -> Option<String> {
    let re = Regex::new(pattern).expect("内置正则应有效");
    re.find(text).map(|m| m.as_str().to_string())
}

const AMOUNT_PATTERN: &str = r"[￥¥]?\s*[\d,]+\.?\d*";
const DATE_PATTERN: &str = r"\d{4}[-/年]\d{1,2}[-/月]\d{1,2}日?";
const DAY_MONTH_YEAR_PATTERN: &str = r"\d{1,2}/\d{1,2}/\d{4}";
const INVOICE_NUMBER_PATTERN: &str = r"\d{10,}";

fn require_field(value: Option<String>, field: &str, path: &Path) -> Result<String, ParseError> {
    value.ok_or_else(|| ParseError::MissingField {
        path: path.to_path_buf(),
        field: field.to_string(),
    })
}

fn parse_day_month_year(raw: &str) -> Result<NaiveDate, ParseError> {
    let re = Regex::new(r"^(\d{1,2})/(\d{1,2})/(\d{4})$").expect("内置日期正则应有效");
    let parsed = re.captures(raw.trim()).and_then(|captures| {
        let day = captures[1].parse::<u32>().ok()?;
        let month = captures[2].parse::<u32>().ok()?;
        let year = captures[3].parse::<i32>().ok()?;
        NaiveDate::from_ymd_opt(year, month, day)
    });
    parsed.ok_or_else(|| ParseError::UnparseableValue {
        field: "issue_date".to_string(),
        raw: raw.to_string(),
        expected_type: "DD/MM/YYYY date",
    })
}

fn capture_relative_to_pattern(
    text: &str,
    label_pattern: &str,
    value_pattern: &str,
    value_before_label: bool,
) -> Option<String> {
    let pattern = if value_before_label {
        format!(r"({value_pattern})[\s：:]*(?:{label_pattern})")
    } else {
        format!(r"(?:{label_pattern})[\s：:]*({value_pattern})")
    };
    let re = Regex::new(&pattern).expect("内置相对字段正则应有效");
    re.captures(text)
        .map(|captures| captures[1].trim().to_string())
}

fn capture_unique_day_month_year_timestamp(text: &str) -> Option<String> {
    let re =
        Regex::new(r"(\d{1,2}/\d{1,2}/\d{4})\s+\d{1,2}:\d{2}:\d{2}").expect("内置时间戳正则应有效");
    let mut matches = re.captures_iter(text);
    let value = matches.next().map(|captures| captures[1].to_string())?;
    if matches.next().is_some() {
        return None;
    }
    Some(value)
}

fn has_delivery_invoice_anchors(text: &str) -> bool {
    [
        "运单明细",
        "运单号码",
        "寄件时间",
        "发票号码",
        "开票时间",
        "发票总金额",
    ]
    .iter()
    .all(|anchor| text.contains(anchor))
}

/// 解析“电子发票—运单明细”模板。
///
/// 该模板的 PDF 文本层按表格绘制顺序输出：先输出“寄件时间/运单号码”及其值，
/// 再输出“开票时间/发票号码”及其值。通用的“首个日期/长数字”回退因此会把运单
/// 字段误当作发票字段。只有六个模板锚点全部存在时才启用此分支，并要求：
/// - 页面内恰好一个 18–24 位纯数字候选；
/// - 开票日期与该候选出现在同一文本行；
/// - 总额带明确的“发票总金额（元/人民币/CNY/RMB）”标签。
///
/// 任一条件不满足即失败，不回退到易产生静默误判的首值策略。
fn parse_delivery_invoice_text(
    text: &str,
    path: &Path,
) -> Option<Result<ParsedInvoice, ParseError>> {
    if !has_delivery_invoice_anchors(text) {
        return None;
    }

    Some((|| {
        let number_re = Regex::new(r"(?:^|[^\d])(\d{18,24})(?:[^\d]|$)")
            .expect("内置运单明细发票号码正则应有效");
        let mut numbers = number_re
            .captures_iter(text)
            .map(|captures| captures[1].to_string())
            .collect::<Vec<_>>();
        numbers.sort();
        numbers.dedup();
        let number_raw = if numbers.len() == 1 {
            numbers.remove(0)
        } else {
            return Err(ParseError::MissingField {
                path: path.to_path_buf(),
                field: "invoice_number".to_string(),
            });
        };

        let date_re = Regex::new(DATE_PATTERN).expect("内置日期正则应有效");
        let date_raw = text
            .lines()
            .find(|line| line.contains(&number_raw))
            .and_then(|line| date_re.find(line))
            .map(|matched| matched.as_str().to_string())
            .or_else(|| capture_after(text, &["开票时间"], DATE_PATTERN))
            .ok_or_else(|| ParseError::MissingField {
                path: path.to_path_buf(),
                field: "issue_date".to_string(),
            })?;
        let amount_raw =
            capture_total_after_currency_unit(text).ok_or_else(|| ParseError::MissingField {
                path: path.to_path_buf(),
                field: "total_amount".to_string(),
            })?;
        let issue_date = parse_date(&date_raw)?;

        Ok(ParsedInvoice {
            invoice_number: number_raw,
            issue_date,
            total_amount: parse_amount(&amount_raw, "total_amount")?,
            tax_amount: None,
            tax_rate: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Other,
            transport_document_kind: Default::default(),
            parse_level: ParseLevel::L1,
            confidence: 1.0,
            city: None,
            travel_route: None,
            departure_time: None,
            checkin_date: None,
            source_path: path.to_path_buf(),
        })
    })())
}

/// 解析带中英双语固定标签的酒店账单。
///
/// 这类单据的账单号可能短于中国数电票号码，因此只有三个模板锚点同时存在时才启用，
/// 不放宽普通发票解析器的号码和日期规则。
fn parse_bilingual_hotel_folio(
    text: &str,
    path: &Path,
) -> Option<Result<ParsedInvoice, ParseError>> {
    const NUMBER_LABEL: &str = r"账单号码\s*/\s*Invoice\s*No\.?";
    const DATE_LABEL: &str = r"打印日期\s*/\s*Print\s*Date";
    const TOTAL_LABEL: &str = r"总计\s*/\s*Total";
    if ![NUMBER_LABEL, DATE_LABEL, TOTAL_LABEL].iter().all(|label| {
        Regex::new(label)
            .expect("内置酒店标签正则应有效")
            .is_match(text)
    }) {
        return None;
    }

    Some((|| {
        let number_raw = require_field(
            capture_relative_to_pattern(text, NUMBER_LABEL, r"\d{6,20}", false)
                .or_else(|| capture_relative_to_pattern(text, NUMBER_LABEL, r"\d{6,20}", true)),
            "invoice_number",
            path,
        )?;
        let date_raw = require_field(
            capture_relative_to_pattern(text, DATE_LABEL, DAY_MONTH_YEAR_PATTERN, false)
                .or_else(|| capture_unique_day_month_year_timestamp(text)),
            "issue_date",
            path,
        )?;
        let amount_raw = require_field(
            capture_relative_to_pattern(text, TOTAL_LABEL, AMOUNT_PATTERN, false),
            "total_amount",
            path,
        )?;

        Ok(ParsedInvoice {
            invoice_number: number_raw.chars().filter(|c| c.is_ascii_digit()).collect(),
            issue_date: parse_day_month_year(&date_raw)?,
            total_amount: parse_amount(&amount_raw, "total_amount")?,
            tax_amount: None,
            tax_rate: None,
            buyer_name: capture_after(text, &["公司名称/Company Name"], r"[^\r\n]+"),
            seller_name: None,
            ticket_type: TicketType::Hotel,
            transport_document_kind: Default::default(),
            parse_level: ParseLevel::L1,
            confidence: 1.0,
            city: None,
            travel_route: None,
            departure_time: None,
            checkin_date: None,
            source_path: path.to_path_buf(),
        })
    })())
}

pub fn parse_rail_itinerary(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError> {
    let number_raw = require_field(
        capture_after(text, &["发票号码", "发票号"], INVOICE_NUMBER_PATTERN),
        "invoice_number",
        path,
    )?;
    let date_raw = require_field(
        capture_after(text, &["开票日期"], DATE_PATTERN),
        "issue_date",
        path,
    )?;
    let amount_raw = require_field(
        capture_after(text, &["票价", "金额", "价税合计"], AMOUNT_PATTERN),
        "total_amount",
        path,
    )?;

    let seller_name = capture_after(text, &["销售方名称", "承运人", "售票单位"], r"\S+");
    let travel_route = field_extractor::extract_travel_route(&TicketType::Rail, text);
    let issue_date = parse_date(&date_raw)?;

    Ok(ParsedInvoice {
        invoice_number: number_raw.chars().filter(|c| c.is_ascii_digit()).collect(),
        issue_date,
        total_amount: parse_amount(&amount_raw, "total_amount")?,
        tax_amount: capture_after(text, &["税额"], AMOUNT_PATTERN)
            .map(|raw| parse_amount(&raw, "tax_amount"))
            .transpose()?,
        tax_rate: capture_after(text, &["税率"], r"\d+\.?\d*%?")
            .map(|raw| crate::xml::parse_tax_rate(&raw))
            .transpose()?,
        buyer_name: capture_after(text, &["购买方名称", "购买方"], r"\S+"),
        seller_name: seller_name.clone(),
        ticket_type: TicketType::Rail,
        transport_document_kind: field_extractor::extract_transport_document_kind(text),
        parse_level: ParseLevel::L1,
        confidence: 1.0,
        city: travel_route
            .as_deref()
            .and_then(|route| field_extractor::extract_city(&TicketType::Rail, route)),
        travel_route: travel_route.clone(),
        departure_time: field_extractor::extract_departure_time(text, issue_date),
        checkin_date: None,
        source_path: path.to_path_buf(),
    })
}

pub fn parse_flight_itinerary(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError> {
    // 航空行程单没有"发票号码"，用电子客票号作为唯一标识
    let number_raw = require_field(
        capture_after(
            text,
            &["电子客票号码", "电子客票号", "票号"],
            INVOICE_NUMBER_PATTERN,
        ),
        "invoice_number",
        path,
    )?;
    let date_raw = require_field(
        capture_after(text, &["填开日期", "开票日期"], DATE_PATTERN),
        "issue_date",
        path,
    )?;
    // 必须取"合计"，不能取"票价"——票价不含基金和燃油附加费
    let amount_raw = require_field(
        capture_after(text, &["合计", "价税合计", "总额"], AMOUNT_PATTERN),
        "total_amount",
        path,
    )?;

    let seller_name = capture_after(text, &["承运人"], r"\S+");
    let travel_route = field_extractor::extract_travel_route(&TicketType::Flight, text);
    let issue_date = parse_date(&date_raw)?;

    Ok(ParsedInvoice {
        invoice_number: number_raw.chars().filter(|c| c.is_ascii_digit()).collect(),
        issue_date,
        total_amount: parse_amount(&amount_raw, "total_amount")?,
        tax_amount: None,
        tax_rate: None,
        buyer_name: capture_after(text, &["旅客姓名", "购买方名称"], r"\S+"),
        seller_name: seller_name.clone(),
        ticket_type: TicketType::Flight,
        transport_document_kind: field_extractor::extract_transport_document_kind(text),
        parse_level: ParseLevel::L1,
        confidence: 1.0,
        city: travel_route
            .as_deref()
            .and_then(|route| field_extractor::extract_city(&TicketType::Flight, route)),
        travel_route: travel_route.clone(),
        departure_time: field_extractor::extract_departure_time(text, issue_date),
        checkin_date: None,
        source_path: path.to_path_buf(),
    })
}

pub fn parse_vat_invoice_text(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError> {
    if let Some(result) = parse_delivery_invoice_text(text, path) {
        return result;
    }
    if let Some(result) = parse_bilingual_hotel_folio(text, path) {
        return result;
    }

    // VAT invoices from PDF text layers often have labels separated from values by layout
    // Try label-based capture first, fall back to pattern matching
    let number_raw = capture_after(text, &["发票号码", "发票号"], INVOICE_NUMBER_PATTERN)
        .or_else(|| find_first_match(text, INVOICE_NUMBER_PATTERN));
    let number_raw = require_field(number_raw, "invoice_number", path)?;

    let date_raw = capture_after(text, &["开票日期", "开票时间"], DATE_PATTERN)
        .or_else(|| find_first_match(text, DATE_PATTERN));
    let date_raw = require_field(date_raw, "issue_date", path)?;

    // For total amount: try labels first, then fall back to largest amount in text
    let amount_raw = capture_after(text, &["价税合计", "合计金额", "小写"], AMOUNT_PATTERN)
        .or_else(|| capture_total_after_currency_unit(text))
        .or_else(|| {
            // Find all amounts and return the largest (price-tax total is usually the largest)
            let re = Regex::new(r"¥[\d,]+\.?\d*").expect("金额正则应有效");
            re.find_iter(text)
                .map(|m| m.as_str())
                .max_by_key(|s| {
                    // Parse to compare numerically
                    let cleaned = s.trim_start_matches('¥').replace(',', "");
                    cleaned.parse::<f64>().unwrap_or(0.0) as i64
                })
                .map(|s| s.to_string())
        });
    let amount_raw = require_field(amount_raw, "total_amount", path)?;

    let seller_name = capture_after(text, &["销售方名称", "销  售  方"], r"\S+");
    let issue_date = parse_date(&date_raw)?;
    let ticket_type = expense_classifier::classify_invoice_text(text).unwrap_or(TicketType::Other);

    Ok(ParsedInvoice {
        invoice_number: number_raw.chars().filter(|c| c.is_ascii_digit()).collect(),
        issue_date,
        total_amount: parse_amount(&amount_raw, "total_amount")?,
        tax_amount: capture_after(text, &["税额", "税  额"], AMOUNT_PATTERN)
            .map(|raw| parse_amount(&raw, "tax_amount"))
            .transpose()?,
        tax_rate: capture_after(text, &["税率"], r"\d+\.?\d*%?")
            .map(|raw| crate::xml::parse_tax_rate(&raw))
            .transpose()?,
        buyer_name: capture_after(text, &["购买方名称", "购  买  方"], r"\S+"),
        seller_name: seller_name.clone(),
        ticket_type,
        transport_document_kind: field_extractor::extract_transport_document_kind(text),
        parse_level: ParseLevel::L1,
        confidence: 1.0,
        city: field_extractor::extract_city(&ticket_type, seller_name.as_deref().unwrap_or(""))
            .or_else(|| field_extractor::extract_seller_address_city(text))
            .or_else(|| {
                field_extractor::extract_consistent_seller_jurisdiction_city(
                    text,
                    seller_name.as_deref(),
                )
            }),
        travel_route: None,
        departure_time: None,
        checkin_date: None,
        source_path: path.to_path_buf(),
    })
}

/// 使用带坐标的文本框解析增值税发票（PDF）。
///
/// 相比纯文本提取，此方法利用空间关系定位字段，
/// 对版式复杂的 PDF 有更高的准确率。
pub fn parse_vat_invoice_positioned(
    pdf_bytes: &[u8],
    path: &Path,
) -> Result<ParsedInvoice, ParseError> {
    crate::pdf_text::parse_vat_invoice_from_boxes(pdf_bytes, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromStr;
    use rust_decimal::Decimal;

    #[test]
    fn ride_hailing_itinerary_without_invoice_is_supporting_only() {
        let text = "滴滴出行 行程单\n乘车时间 2026-06-18\n行程金额 88.00";
        assert!(is_unambiguous_ride_hailing_itinerary(text));
    }

    #[test]
    fn ride_hailing_invoice_is_not_treated_as_supporting_only() {
        let text = "滴滴出行 行程单\n电子发票\n发票号码 26112000000000000001\n价税合计 88.00";
        assert!(!is_unambiguous_ride_hailing_itinerary(text));
    }

    #[test]
    fn extracts_ride_hailing_itinerary_matching_facts() {
        let text = "滴滴出行-行程单\n· 行程起止日期：2026-06-24 至 2026-06-25\n· 共6笔行程， 合计151.94元\n序号 车型 上车时间 城市\n1 快车 06-24 14:18 周三 邢台 邢台泉城礼堂假日酒店";
        let facts = extract_supporting_document_facts(text).unwrap();
        assert_eq!(facts.kind, "ride_hailing_itinerary");
        assert_eq!(facts.provider, "didi");
        assert_eq!(facts.total_amount, Decimal::new(15194, 2));
        assert_eq!(facts.start_date.unwrap().to_string(), "2026-06-24");
        assert_eq!(facts.cities, vec!["邢台"]);
        assert!(facts
            .hotel_mentions
            .iter()
            .any(|name| name.contains("邢台泉城礼堂假日酒店")));
    }

    #[test]
    fn extracts_city_when_ride_hailing_rows_are_split_across_lines() {
        let text = "滴滴出行-行程单\n行程起止日期：2026-06-15 至 2026-06-18\n共1笔行程，合计8.30元\n1 特惠快\n车 06-15 13:14 周\n一 太原\n市 太榆路|太原南站";
        let facts = extract_supporting_document_facts(text).unwrap();
        assert_eq!(facts.cities, vec!["太原"]);
    }

    #[test]
    fn extracts_hotel_folio_matching_facts() {
        let text = "全季赤峰市政府酒店\n结账单\n入住日期 ： 2026-06-04 离店日期 ： 2026-06-05\n消费合计 439.00\n付款合计 439.00";
        let facts = extract_supporting_document_facts(text).unwrap();
        assert_eq!(facts.kind, "hotel_folio");
        assert_eq!(facts.total_amount, Decimal::new(43900, 2));
        assert_eq!(facts.end_date.unwrap().to_string(), "2026-06-05");
        assert_eq!(facts.cities, vec!["赤峰"]);
    }

    #[test]
    fn extracts_bilingual_hotel_folio_address_and_dmy_dates() {
        let text = "到店日期/Arrival : 24/06/2026\n离店日期/Departure : 25/06/2026\n账单号码/Invoice No. : 0010574\n总计/Total 453.05\n应付总额/Balance 0.00\n邢台泉城礼堂假日酒店\n地址：河北省邢台市襄都区";
        let facts = extract_supporting_document_facts(text).unwrap();
        assert_eq!(facts.kind, "hotel_folio");
        assert_eq!(facts.total_amount, Decimal::new(45305, 2));
        assert_eq!(facts.start_date.unwrap().to_string(), "2026-06-24");
        assert_eq!(facts.end_date.unwrap().to_string(), "2026-06-25");
        assert_eq!(facts.cities, vec!["邢台"]);
    }

    #[test]
    fn extracts_image_style_hotel_folio_without_a_total_label() {
        let text = "上海虹桥维景酒店\nINFORMATION INVOICE\n结账单\nArrival Time/到店时间\n2026/6/1 16:37:19\nDeparture Time/离店时间\n2026/6/3 6:26:37\nDebit/消费 Credit/付款\n750.00\n750.00\n1500.00\n￥1500.00 ￥1500.00\nBalance/余额：￥0.00";

        let facts = extract_supporting_document_facts(text).unwrap();

        assert_eq!(facts.kind, "hotel_folio");
        assert_eq!(facts.total_amount, Decimal::new(150_000, 2));
        assert_eq!(facts.start_date.unwrap().to_string(), "2026-06-01");
        assert_eq!(facts.end_date.unwrap().to_string(), "2026-06-03");
        assert_eq!(facts.cities, vec!["上海"]);
    }

    #[test]
    fn extracts_courier_detail_without_repeated_platform_name() {
        let text = "运单起止日期：2026-04-28至2026-05-28 共2笔运单，合计68.82元\n1 帮我送 05-28 周四 46.92元 在线支付";
        let facts = extract_supporting_document_facts(text).unwrap();
        assert_eq!(facts.kind, "courier_detail");
        assert_eq!(facts.provider, "courier");
        assert_eq!(facts.total_amount, Decimal::new(6882, 2));
    }

    #[test]
    fn amount_window_is_truncated_on_character_boundaries() {
        let text = "合计中文材料内容用于覆盖多字节字符边界并确保金额提取不会因为截断位置落在汉字中间而异常123.45";
        assert_eq!(
            amount_after_labels(text, &["合计"]),
            Some(Decimal::new(12345, 2))
        );
    }

    #[test]
    fn other_travel_and_payment_documents_keep_existing_ocr_fallback() {
        assert!(!is_unambiguous_ride_hailing_itinerary(
            "航空运输电子客票行程单\n电子客票号码 7812345678901"
        ));
        assert!(!is_unambiguous_ride_hailing_itinerary(
            "酒店住宿记录\n支付金额 453.05"
        ));
        assert!(!is_unambiguous_ride_hailing_itinerary(
            "未知平台 行程单\n行程金额 20.00"
        ));
    }

    // 铁路电子客票行程单的典型文本层内容（字段顺序可能因版式而异，
    // 所以解析器必须靠关键词锚定而非位置）
    const RAIL_TEXT: &str = "电子发票（铁路电子客票）
发票号码 24312000000012345678
开票日期 2026年07月03日
车次 G13 北京南 上海虹桥
2026年07月03日09:00开
票价 ￥553.00
税率 9% 税额 ￥45.63
购买方名称 某某科技有限公司";

    const FLIGHT_TEXT: &str = "航空运输电子客票行程单
电子客票号码 7812345678901
填开日期 2026-07-10
承运人 CZ 航班号 CZ3001
北京首都 - 深圳宝安
票价 1580.00
民航发展基金 50.00
燃油附加费 220.00
合计 1850.00";

    #[test]
    fn rail_itinerary_yields_number_date_amount() {
        let invoice = parse_rail_itinerary(RAIL_TEXT, Path::new("rail.pdf")).unwrap();

        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
        assert_eq!(invoice.total_amount, Decimal::from_str("553.00").unwrap());
        assert_eq!(invoice.ticket_type, TicketType::Rail);
        assert_eq!(invoice.parse_level, ParseLevel::L1);
        assert_eq!(invoice.travel_route.as_deref(), Some("北京南→上海虹桥"));
        assert_eq!(invoice.city.as_deref(), Some("北京"));
    }

    #[test]
    fn detects_and_parses_rail_without_an_external_ticket_hint() {
        assert_eq!(detect_travel_ticket_type(RAIL_TEXT), Some(TicketType::Rail));
        let invoice = parse_detected_travel_invoice_text(RAIL_TEXT, Path::new("rail.pdf")).unwrap();
        assert_eq!(invoice.ticket_type, TicketType::Rail);
        assert!(invoice.travel_route.is_some());
        assert!(invoice.departure_time.is_some());
    }

    #[test]
    fn does_not_treat_a_generic_railway_vendor_invoice_as_a_train_ticket() {
        let text = "电子发票（普通发票）\n销售方名称 中国铁路网络有限公司\n发票号码 12345678901234567890\n开票日期 2026年06月01日\n价税合计 ¥100.00";
        assert_eq!(detect_travel_ticket_type(text), None);
    }

    #[test]
    fn rail_itinerary_extracts_tax_fields() {
        let invoice = parse_rail_itinerary(RAIL_TEXT, Path::new("rail.pdf")).unwrap();
        assert_eq!(
            invoice.tax_amount,
            Some(Decimal::from_str("45.63").unwrap())
        );
        assert_eq!(invoice.tax_rate, Some(Decimal::from_str("0.09").unwrap()));
    }

    #[test]
    fn flight_itinerary_uses_total_not_base_fare() {
        // 陷阱：文本里有"票价 1580.00"和"合计 1850.00"，
        // 报销金额必须取合计
        let invoice = parse_flight_itinerary(FLIGHT_TEXT, Path::new("air.pdf")).unwrap();
        assert_eq!(invoice.total_amount, Decimal::from_str("1850.00").unwrap());
        assert_eq!(invoice.ticket_type, TicketType::Flight);
        assert_eq!(invoice.travel_route.as_deref(), Some("北京首都→深圳宝安"));
        assert_eq!(invoice.city.as_deref(), Some("北京"));
    }

    #[test]
    fn flight_itinerary_uses_ticket_number_as_invoice_number() {
        let invoice = parse_flight_itinerary(FLIGHT_TEXT, Path::new("air.pdf")).unwrap();
        assert_eq!(invoice.invoice_number, "7812345678901");
    }

    #[test]
    fn missing_amount_reports_field_name() {
        let text = "电子发票（铁路电子客票）\n发票号码 12345678901234\n开票日期 2026年07月03日";
        let err = parse_rail_itinerary(text, Path::new("x.pdf")).unwrap_err();
        assert!(err.to_string().contains("total_amount"), "实际: {err}");
    }

    #[test]
    fn empty_text_is_treated_as_no_text_layer() {
        // 纯扫描件 PDF 提取出的文本为空或只有空白
        assert!(!has_text_layer(b"%PDF-1.4\n%%EOF"));
    }

    #[test]
    fn real_vat_invoice_text_extracts_fields() {
        // Real extracted text from fixtures/samples/05-unknown-b4511bc3.pdf
        const REAL_VAT: &str = "电子发票（普通发票） 发票号码：
开票日期：
购买方信息 统一社会信用代码/纳税人识别号：
销售方信息 统一社会信用代码/纳税人识别号：
名称： 名称：
项目名称 规格型号 单  位 数  量 单  价 金  额 税率/征收率 税  额
合 计
价税合计（大写） （小写）
备注 开票人：
26112000002267104336
2026年06月04日
赛比亚医疗诊断器械（上海）有限公司
91310000MA1FPFFF8P
河北融元商贸有限公司北京第三分公司
91110105MAEM51QE2N
¥64.75 ¥0.65
陆拾伍圆肆角整 ¥65.40
陈俊刚
陈俊刚
*其他食品*其他食品 1%64.75 0.65
购买方地址:-;    电话:15313153611";

        let invoice = parse_vat_invoice_text(REAL_VAT, Path::new("test.pdf")).unwrap();
        assert_eq!(invoice.invoice_number, "26112000002267104336");
        assert_eq!(invoice.issue_date.to_string(), "2026-06-04");
        assert_eq!(invoice.total_amount, Decimal::from_str("65.40").unwrap());
    }

    #[test]
    fn meal_invoice_uses_explicit_seller_address_for_city() {
        let text = "电子发票\n发票号码 26312000003400000001\n开票日期 2026-06-01\n价税合计 646.70\n销售方名称 示例餐饮有限公司\n销售方地址：上海市浦东新区世纪大道 1 号\n*餐饮服务*餐饮费";

        let invoice = parse_vat_invoice_text(text, Path::new("meal.pdf")).unwrap();

        assert_eq!(invoice.ticket_type, TicketType::Meal);
        assert_eq!(invoice.city.as_deref(), Some("上海"));
    }

    #[test]
    fn delivery_invoice_accepts_labeled_total_with_currency_unit() {
        let text = "电子发票 - 运单明细\n开票时间 2026/06/08 发票号码\n26117000000000000001\n寄件时间\n2026/05/27\n发票总金额(元)\n15.00";

        let invoice = parse_vat_invoice_text(text, Path::new("delivery.pdf")).unwrap();

        assert_eq!(invoice.invoice_number, "26117000000000000001");
        assert_eq!(invoice.issue_date.to_string(), "2026-06-08");
        assert_eq!(invoice.total_amount, Decimal::from_str("15.00").unwrap());
    }

    #[test]
    fn delivery_invoice_does_not_confuse_waybill_fields_with_invoice_fields() {
        let text = "电子发票 - 运单明细
寄件时间 运单号码
2026/05/27 SF0211592685223
开票时间 发票号码
2026/06/08 26117000000000000001
发票总金额(元)
15.00";

        let invoice = parse_vat_invoice_text(text, Path::new("delivery.pdf")).unwrap();

        assert_eq!(invoice.invoice_number, "26117000000000000001");
        assert_eq!(invoice.issue_date.to_string(), "2026-06-08");
        assert_eq!(invoice.total_amount, Decimal::from_str("15.00").unwrap());
    }

    #[test]
    fn delivery_invoice_rejects_ambiguous_long_numbers() {
        let text = "电子发票 - 运单明细
寄件时间 运单号码
2026/05/27 123456789012345678
开票时间 发票号码
2026/06/08 26117000000000000001
发票总金额(元)
15.00";

        let error = parse_vat_invoice_text(text, Path::new("delivery.pdf")).unwrap_err();

        assert!(
            error.to_string().contains("invoice_number"),
            "实际: {error}"
        );
    }

    #[test]
    fn total_currency_suffix_rejects_unknown_parenthetical_text() {
        let text =
            "电子发票\n开票时间 2026/06/08\n发票号码 26117000000000000001\n发票总金额(备注)\n15.00";

        let error = parse_vat_invoice_text(text, Path::new("delivery.pdf")).unwrap_err();

        assert!(error.to_string().contains("total_amount"), "实际: {error}");
    }

    #[test]
    fn bilingual_hotel_folio_uses_scoped_short_number_and_dmy_date() {
        let text = "公司名称/Company Name : Example Travel Ltd\n打印日期/Print Date : 25/06/2026 08:57:50\n账单号码/Invoice No. : 0010574\n日期/Date 账目/Item 消费/Charge 付款/Payment\n24/06/2026 Accommodation 453.05\n总计/Total 453.05 453.05";

        let invoice = parse_vat_invoice_text(text, Path::new("hotel.pdf")).unwrap();

        assert_eq!(invoice.invoice_number, "0010574");
        assert_eq!(invoice.issue_date.to_string(), "2026-06-25");
        assert_eq!(invoice.total_amount, Decimal::from_str("453.05").unwrap());
        assert_eq!(invoice.ticket_type, TicketType::Hotel);
    }

    #[test]
    fn bilingual_hotel_folio_handles_pdf_text_reordering() {
        let text = "公司名称/Company Name : Example Travel Ltd\n打印日期/Print Date :\n其他版式字段\n25/06/2026 08:57:50\n0010574账单号码/Invoice No.\n日期/Date 账目/Item 消费/Charge 付款/Payment\n24/06/2026 Accommodation 453.05\n总计/Total 453.05 453.05";

        let invoice = parse_vat_invoice_text(text, Path::new("hotel.pdf")).unwrap();

        assert_eq!(invoice.invoice_number, "0010574");
        assert_eq!(invoice.issue_date.to_string(), "2026-06-25");
        assert_eq!(invoice.total_amount, Decimal::from_str("453.05").unwrap());
        assert_eq!(invoice.ticket_type, TicketType::Hotel);
    }

    #[test]
    fn bilingual_hotel_folio_rejects_ambiguous_timestamps() {
        let text = "打印日期/Print Date\n其他版式字段\n25/06/2026 08:57:50\n26/06/2026 09:00:00\n0010574账单号码/Invoice No.\n总计/Total 453.05";

        let error = parse_vat_invoice_text(text, Path::new("hotel.pdf")).unwrap_err();

        assert!(error.to_string().contains("issue_date"), "实际: {error}");
    }

    #[test]
    fn short_invoice_number_is_not_accepted_without_all_hotel_anchors() {
        let text = "账单号码/Invoice No. : 0010574\n打印日期/Print Date : 25/06/2026\n金额 453.05";

        let error = parse_vat_invoice_text(text, Path::new("hotel.pdf")).unwrap_err();

        assert!(
            error.to_string().contains("invoice_number"),
            "实际: {error}"
        );
    }
}
