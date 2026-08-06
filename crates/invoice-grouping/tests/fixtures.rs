use chrono::{NaiveDate, NaiveDateTime};
use invoice_parse::model::{ParsedInvoice, TicketType, ParseLevel};
use rust_decimal::Decimal;
use std::path::PathBuf;

/// 构建一张合成交通票
pub fn make_transport(
    idx: usize,
    ticket_type: TicketType,
    date: NaiveDate,
    hour: u32,
    from_city: &str,
    _to_city: &str,
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
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        source_path: PathBuf::from(format!("synthetic/{}.xml", idx)),
        city: Some(from_city.to_string()),
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
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        source_path: PathBuf::from(format!("synthetic/{}.xml", idx)),
        city: Some(city.to_string()),
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
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        source_path: PathBuf::from(format!("synthetic/{}.xml", idx)),
        city: Some(city.to_string()),
        departure_time: None,
        checkin_date: None,
    }
}
