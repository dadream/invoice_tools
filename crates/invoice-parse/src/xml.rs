use crate::field_extractor;
use crate::manifest::TagHints;
use crate::model::{ParseError, ParseLevel, ParsedInvoice, TicketType};
use chrono::NaiveDate;
use quick_xml::events::Event;
use quick_xml::Reader;
use rust_decimal::Decimal;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// XML 中一个含文本的叶子元素。
#[derive(Debug, Clone, PartialEq)]
pub struct LeafElement {
    pub tag: String,
    pub text: String,
    pub depth: usize,
}

/// 遍历 XML，收集所有含非空文本的叶子元素。
/// 命名空间前缀会被剥离（`tax:TotalAmount` → `TotalAmount`），
/// 因为不同平台的前缀不同但本地名通常一致。
pub fn collect_leaf_elements(xml_bytes: &[u8]) -> Result<Vec<LeafElement>, ParseError> {
    let mut reader = Reader::from_reader(xml_bytes);
    reader.config_mut().trim_text(true);

    let mut leaves = Vec::new();
    let mut buf = Vec::new();
    // 栈顶记录当前元素的 (标签名, 深度, 是否已见过子元素)
    let mut stack: Vec<(String, usize, bool)> = Vec::new();
    let mut pending_text: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if let Some(parent) = stack.last_mut() {
                    parent.2 = true;
                }
                let tag = local_name(e.name().as_ref());
                let depth = stack.len();
                stack.push((tag, depth, false));
                pending_text = None;
            }
            Ok(Event::Text(e)) => {
                let text = e
                    .unescape()
                    .map_err(|err| ParseError::MalformedFormat {
                        path: PathBuf::new(),
                        format: "XML",
                        detail: format!("文本节点解码失败: {err}"),
                    })?
                    .trim()
                    .to_string();
                if !text.is_empty() {
                    pending_text = Some(text);
                }
            }
            Ok(Event::End(_)) => {
                if let Some((tag, depth, had_children)) = stack.pop() {
                    if !had_children {
                        if let Some(text) = pending_text.take() {
                            leaves.push(LeafElement { tag, text, depth });
                        }
                    }
                }
                pending_text = None;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(ParseError::MalformedFormat {
                    path: PathBuf::new(),
                    format: "XML",
                    detail: err.to_string(),
                })
            }
        }
        buf.clear();
    }

    if !stack.is_empty() {
        return Err(ParseError::MalformedFormat {
            path: PathBuf::new(),
            format: "XML",
            detail: format!("有 {} 个元素未闭合", stack.len()),
        });
    }

    Ok(leaves)
}

/// 剥离命名空间前缀：`tax:Number` → `Number`
fn local_name(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    match s.rsplit_once(':') {
        Some((_, local)) => local.to_string(),
        None => s.to_string(),
    }
}

pub fn parse_invoice_xml(
    bytes: &[u8],
    path: &Path,
    hints: &TagHints,
    ticket_type: TicketType,
) -> Result<ParsedInvoice, ParseError> {
    let leaves = collect_leaf_elements(bytes).map_err(|e| match e {
        // collect_leaf_elements 不知道文件路径，这里补上
        ParseError::MalformedFormat { format, detail, .. } => ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format,
            detail,
        },
        other => other,
    })?;

    let find = |candidates: &[String]| -> Option<String> {
        candidates
            .iter()
            .find_map(|want| {
                leaves
                    .iter()
                    .find(|leaf| leaf.tag == *want)
                    .map(|leaf| leaf.text.clone())
            })
    };

    let require = |candidates: &[String], field: &str| -> Result<String, ParseError> {
        find(candidates).ok_or_else(|| ParseError::MissingField {
            path: path.to_path_buf(),
            field: field.to_string(),
        })
    };

    let invoice_number = require(&hints.invoice_number, "invoice_number")?;
    let issue_date = parse_date(&require(&hints.issue_date, "issue_date")?)?;
    let total_amount = parse_amount(&require(&hints.total_amount, "total_amount")?, "total_amount")?;

    let tax_amount = find(&hints.tax_amount)
        .map(|raw| parse_amount(&raw, "tax_amount"))
        .transpose()?;
    let tax_rate = find(&hints.tax_rate)
        .map(|raw| parse_tax_rate(&raw))
        .transpose()?;

    let seller_name = find(&hints.seller_name);

    Ok(ParsedInvoice {
        invoice_number,
        issue_date,
        total_amount,
        tax_amount,
        tax_rate,
        buyer_name: find(&hints.buyer_name),
        seller_name: seller_name.clone(),
        ticket_type,
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        city: field_extractor::extract_city(&ticket_type, &seller_name.as_deref().unwrap_or("")),
        departure_time: if matches!(ticket_type, TicketType::Rail | TicketType::Flight) {
            field_extractor::extract_departure_time(&seller_name.as_deref().unwrap_or(""), issue_date)
        } else {
            None
        },
        checkin_date: if ticket_type == TicketType::Hotel {
            field_extractor::extract_checkin_date(issue_date)
        } else {
            None
        },
        source_path: path.to_path_buf(),
    })
}

/// 接受 `2026-07-03`、`2026/07/03`、`20260703`、`2026年07月03日`、`2026-06-08 13:18:44`
pub(crate) fn parse_date(raw: &str) -> Result<NaiveDate, ParseError> {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();

    if cleaned.len() >= 8 {
        let date_part = &cleaned[..8];
        if let Ok(d) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
            return Ok(d);
        }
    }
    Err(ParseError::UnparseableValue {
        field: "issue_date".to_string(),
        raw: raw.to_string(),
        expected_type: "date",
    })
}

/// 去掉货币符号、千分位逗号、空白后转 Decimal
pub(crate) fn parse_amount(raw: &str, field: &str) -> Result<Decimal, ParseError> {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();

    Decimal::from_str(&cleaned).map_err(|_| ParseError::UnparseableValue {
        field: field.to_string(),
        raw: raw.to_string(),
        expected_type: "decimal",
    })
}

/// `9%` → 0.09；`0.09` → 0.09。
/// 判据：含 `%` 则除以 100；否则若值 > 1 也视为百分数（税率不可能超过 100%）。
pub(crate) fn parse_tax_rate(raw: &str) -> Result<Decimal, ParseError> {
    let has_percent = raw.contains('%');
    let value = parse_amount(raw, "tax_rate")?;

    let normalized = if has_percent || value > Decimal::ONE {
        value / Decimal::from(100)
    } else {
        value
    };
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::TagHints;
    use crate::model::{ParseLevel, TicketType};
    use rust_decimal::prelude::FromStr;
    use rust_decimal::Decimal;
    use std::path::Path;

    fn hints() -> TagHints {
        TagHints {
            invoice_number: vec!["Fphm".into(), "InvoiceNumber".into()],
            issue_date: vec!["Kprq".into()],
            total_amount: vec!["Jshj".into()],
            tax_amount: vec!["Se".into()],
            tax_rate: vec!["Sl".into()],
            buyer_name: vec!["Gfmc".into()],
            seller_name: vec!["Xfmc".into()],
        }
    }

    const SAMPLE_XML: &[u8] = r#"<Invoice>
        <Head><Fphm>24312000000012345678</Fphm><Kprq>2026-07-03</Kprq></Head>
        <Sum><Jshj>553.00</Jshj><Se>50.73</Se><Sl>0.09</Sl></Sum>
        <Party><Gfmc>某某公司</Gfmc><Xfmc>中国铁路</Xfmc></Party>
    </Invoice>"#.as_bytes();

    #[test]
    fn parses_all_fields_from_hinted_tags() {
        let invoice = parse_invoice_xml(
            SAMPLE_XML,
            Path::new("samples/rail-01.xml"),
            &hints(),
            TicketType::Rail,
        )
        .unwrap();

        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
        assert_eq!(invoice.total_amount, Decimal::from_str("553.00").unwrap());
        assert_eq!(invoice.tax_amount, Some(Decimal::from_str("50.73").unwrap()));
        assert_eq!(invoice.buyer_name.as_deref(), Some("某某公司"));
        assert_eq!(invoice.parse_level, ParseLevel::L0);
        assert_eq!(invoice.confidence, 1.0);
    }

    #[test]
    fn falls_back_to_second_candidate_tag() {
        let xml = br#"<I><InvoiceNumber>888</InvoiceNumber><Kprq>2026-07-03</Kprq><Jshj>1.00</Jshj></I>"#;
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.invoice_number, "888");
    }

    #[test]
    fn missing_required_field_errors_with_field_name() {
        let xml = br#"<I><Kprq>2026-07-03</Kprq><Jshj>1.00</Jshj></I>"#;
        let err = parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invoice_number"), "错误信息应指出缺失字段: {msg}");
    }

    #[test]
    fn absent_optional_fields_become_none() {
        let xml = br#"<I><Fphm>1</Fphm><Kprq>2026-07-03</Kprq><Jshj>1.00</Jshj></I>"#;
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.tax_amount, None);
        assert_eq!(invoice.buyer_name, None);
    }

    #[test]
    fn slash_separated_date_is_accepted() {
        let xml = br#"<I><Fphm>1</Fphm><Kprq>2026/07/03</Kprq><Jshj>1.00</Jshj></I>"#;
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
    }

    #[test]
    fn compact_date_is_accepted() {
        let xml = br#"<I><Fphm>1</Fphm><Kprq>20260703</Kprq><Jshj>1.00</Jshj></I>"#;
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
    }

    #[test]
    fn amount_with_currency_symbol_is_cleaned() {
        let xml = r#"<I><Fphm>1</Fphm><Kprq>2026-07-03</Kprq><Jshj>￥1,553.00</Jshj></I>"#.as_bytes();
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.total_amount, Decimal::from_str("1553.00").unwrap());
    }

    #[test]
    fn percent_tax_rate_is_normalized_to_fraction() {
        let xml = br#"<I><Fphm>1</Fphm><Kprq>2026-07-03</Kprq><Jshj>1.00</Jshj><Sl>9%</Sl></I>"#;
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.tax_rate, Some(Decimal::from_str("0.09").unwrap()));
    }

    #[test]
    fn collects_nested_leaf_text() {
        let xml = br#"<Invoice>
            <Header><Number>12345</Number></Header>
            <Body><Amount>553.00</Amount><Tax>50.73</Tax></Body>
        </Invoice>"#;

        let leaves = collect_leaf_elements(xml).unwrap();

        assert_eq!(leaves.len(), 3);
        assert_eq!(leaves[0], LeafElement { tag: "Number".into(), text: "12345".into(), depth: 2 });
        assert_eq!(leaves[1], LeafElement { tag: "Amount".into(), text: "553.00".into(), depth: 2 });
        assert_eq!(leaves[2], LeafElement { tag: "Tax".into(), text: "50.73".into(), depth: 2 });
    }

    #[test]
    fn strips_namespace_prefix() {
        let xml = br#"<tax:Invoice xmlns:tax="urn:x"><tax:Number>999</tax:Number></tax:Invoice>"#;
        let leaves = collect_leaf_elements(xml).unwrap();
        assert_eq!(leaves[0].tag, "Number");
    }

    #[test]
    fn skips_whitespace_only_elements() {
        let xml = br#"<Root><Empty>   </Empty><Real>x</Real></Root>"#;
        let leaves = collect_leaf_elements(xml).unwrap();
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].tag, "Real");
    }

    #[test]
    fn trims_surrounding_whitespace_in_text() {
        let xml = "<Root><Name>  某某公司
  </Name></Root>".as_bytes();
        let leaves = collect_leaf_elements(xml).unwrap();
        assert_eq!(leaves[0].text, "某某公司");
    }

    #[test]
    fn malformed_xml_returns_error() {
        let xml = br#"<Root><Unclosed></Root>"#;
        assert!(collect_leaf_elements(xml).is_err());
    }
}
