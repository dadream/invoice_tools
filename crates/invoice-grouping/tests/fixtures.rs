use chrono::{NaiveDate, NaiveDateTime};
use invoice_grouping::types::{Ambiguity, AmbiguityResolution, AmbiguityResolver};
use invoice_parse::model::{ParseLevel, ParsedInvoice, TicketType};
use rust_decimal::Decimal;
use std::path::PathBuf;

/// 测试用 Dummy Resolver：不解决任何歧义，仅用于满足类型要求
pub struct DummyResolver;

impl AmbiguityResolver for DummyResolver {
    fn resolve(
        &self,
        _ambiguities: &[Ambiguity],
    ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
        // 返回空决策列表，让算法保留原始歧义检测结果
        Ok(vec![])
    }
}

/// 构建一张合成交通票
pub fn make_transport(
    idx: usize,
    ticket_type: TicketType,
    date: NaiveDate,
    hour: u32,
    from_city: &str,
    to_city: &str,
    amount: &str,
) -> ParsedInvoice {
    ParsedInvoice {
        invoice_number: format!("SYNTH{:08}", idx),
        issue_date: date,
        total_amount: amount.parse::<Decimal>().unwrap(),
        tax_amount: None,
        tax_rate: None,
        buyer_name: Some("测试公司".to_string()),
        seller_name: Some("测试承运人".to_string()),
        ticket_type,
        transport_document_kind: invoice_parse::model::TransportDocumentKind::Sale,
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        source_path: PathBuf::from(format!("synthetic/{}.xml", idx)),
        city: Some(from_city.to_string()),
        travel_route: Some(format!("{}→{}", from_city, to_city)),
        departure_time: Some(NaiveDateTime::new(
            date,
            chrono::NaiveTime::from_hms_opt(hour, 0, 0).unwrap(),
        )),
        checkin_date: None,
    }
}

/// 构建一张合成酒店票
pub fn make_hotel(
    idx: usize,
    invoice_date: NaiveDate,
    checkin_date: NaiveDate,
    city: &str,
    amount: &str,
) -> ParsedInvoice {
    ParsedInvoice {
        invoice_number: format!("SYNTH{:08}", idx),
        issue_date: invoice_date,
        total_amount: amount.parse::<Decimal>().unwrap(),
        tax_amount: None,
        tax_rate: None,
        buyer_name: Some("测试公司".to_string()),
        seller_name: None,
        ticket_type: TicketType::Hotel,
        transport_document_kind: Default::default(),
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        source_path: PathBuf::from(format!("synthetic/{}.xml", idx)),
        city: Some(city.to_string()),
        travel_route: None,
        departure_time: None,
        checkin_date: Some(checkin_date),
    }
}

/// 构建一张合成市内票（出租车、餐饮等）
pub fn make_local(
    idx: usize,
    date: NaiveDate,
    city: &str,
    ticket_type: TicketType,
    amount: &str,
) -> ParsedInvoice {
    ParsedInvoice {
        invoice_number: format!("SYNTH{:08}", idx),
        issue_date: date,
        total_amount: amount.parse::<Decimal>().unwrap(),
        tax_amount: None,
        tax_rate: None,
        buyer_name: Some("测试公司".to_string()),
        seller_name: None,
        ticket_type,
        transport_document_kind: Default::default(),
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        source_path: PathBuf::from(format!("synthetic/{}.xml", idx)),
        city: Some(city.to_string()),
        travel_route: None,
        departure_time: None,
        checkin_date: None,
    }
}
