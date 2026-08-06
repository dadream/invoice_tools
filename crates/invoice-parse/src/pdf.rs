use crate::field_extractor;
use crate::manifest::TagHints;
use crate::model::{ParseError, ParseLevel, ParsedInvoice, TicketType};
use crate::xml::{parse_amount, parse_date};
use regex::Regex;
use std::path::Path;

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
        // VAT invoices and other types use the VAT parser
        _ => parse_vat_invoice_text(&text, path),
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

/// 在文本中按标签抓取其后紧跟的值。
/// 标签与值之间允许空格、全角空格、冒号、少量换行。
fn capture_after(text: &str, labels: &[&str], value_pattern: &str) -> Option<String> {
    for label in labels {
        // Try immediate match (same line or next line)
        let pattern = format!(r"{}[\s：:]*\n?[\s：:]*({})", regex::escape(label), value_pattern);
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
const INVOICE_NUMBER_PATTERN: &str = r"\d{10,}";

fn require_field(
    value: Option<String>,
    field: &str,
    path: &Path,
) -> Result<String, ParseError> {
    value.ok_or_else(|| ParseError::MissingField {
        path: path.to_path_buf(),
        field: field.to_string(),
    })
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

    let seller_name = None;
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
        parse_level: ParseLevel::L1,
        confidence: 1.0,
        city: field_extractor::extract_city(&TicketType::Rail, &seller_name.as_deref().unwrap_or("")),
        departure_time: field_extractor::extract_departure_time(&seller_name.as_deref().unwrap_or(""), issue_date),
        checkin_date: None,
        source_path: path.to_path_buf(),
    })
}

pub fn parse_flight_itinerary(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError> {
    // 航空行程单没有"发票号码"，用电子客票号作为唯一标识
    let number_raw = require_field(
        capture_after(text, &["电子客票号码", "电子客票号", "票号"], INVOICE_NUMBER_PATTERN),
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
        parse_level: ParseLevel::L1,
        confidence: 1.0,
        city: field_extractor::extract_city(&TicketType::Flight, &seller_name.as_deref().unwrap_or("")),
        departure_time: field_extractor::extract_departure_time(&seller_name.as_deref().unwrap_or(""), issue_date),
        checkin_date: None,
        source_path: path.to_path_buf(),
    })
}

pub fn parse_vat_invoice_text(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError> {
    // VAT invoices from PDF text layers often have labels separated from values by layout
    // Try label-based capture first, fall back to pattern matching
    let number_raw = capture_after(text, &["发票号码", "发票号"], INVOICE_NUMBER_PATTERN)
        .or_else(|| find_first_match(text, INVOICE_NUMBER_PATTERN));
    let number_raw = require_field(number_raw, "invoice_number", path)?;

    let date_raw = capture_after(text, &["开票日期"], DATE_PATTERN)
        .or_else(|| find_first_match(text, DATE_PATTERN));
    let date_raw = require_field(date_raw, "issue_date", path)?;

    // For total amount: try labels first, then fall back to largest amount in text
    let amount_raw = capture_after(text, &["价税合计", "合计金额", "小写"], AMOUNT_PATTERN)
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
        ticket_type: TicketType::Other,
        parse_level: ParseLevel::L1,
        confidence: 1.0,
        city: field_extractor::extract_city(&TicketType::Other, &seller_name.as_deref().unwrap_or("")),
        departure_time: field_extractor::extract_departure_time(&seller_name.as_deref().unwrap_or(""), issue_date),
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
    }

    #[test]
    fn rail_itinerary_extracts_tax_fields() {
        let invoice = parse_rail_itinerary(RAIL_TEXT, Path::new("rail.pdf")).unwrap();
        assert_eq!(invoice.tax_amount, Some(Decimal::from_str("45.63").unwrap()));
        assert_eq!(invoice.tax_rate, Some(Decimal::from_str("0.09").unwrap()));
    }

    #[test]
    fn flight_itinerary_uses_total_not_base_fare() {
        // 陷阱：文本里有"票价 1580.00"和"合计 1850.00"，
        // 报销金额必须取合计
        let invoice = parse_flight_itinerary(FLIGHT_TEXT, Path::new("air.pdf")).unwrap();
        assert_eq!(invoice.total_amount, Decimal::from_str("1850.00").unwrap());
        assert_eq!(invoice.ticket_type, TicketType::Flight);
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
}
