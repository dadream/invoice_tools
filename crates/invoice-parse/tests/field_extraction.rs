use chrono::NaiveDate;
use invoice_parse::field_extractor::*;
use invoice_parse::model::TicketType;

#[test]
fn test_extract_city_from_rail_ticket() {
    let seller_name = "北京南→上海虹桥";
    let city = extract_city(&TicketType::Rail, seller_name);
    assert_eq!(city, Some("北京".to_string()));
}

#[test]
fn test_extract_city_from_flight_ticket() {
    let seller_name = "上海虹桥→深圳北";
    let city = extract_city(&TicketType::Flight, seller_name);
    assert_eq!(city, Some("上海".to_string()));
}

#[test]
fn test_extract_city_no_arrow() {
    let seller_name = "中国国际航空";
    let city = extract_city(&TicketType::Flight, seller_name);
    assert_eq!(city, None);
}

#[test]
fn test_extract_checkin_date() {
    let issue_date = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
    let checkin = extract_checkin_date(issue_date);
    assert_eq!(checkin, Some(issue_date));
}
