use invoice_parse::manifest::TagHints;
use invoice_parse::model::{ParseError, ParseLevel, TicketType};
use rust_decimal::Decimal;
use std::path::{Path, PathBuf};
use std::str::FromStr;

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("synthetic")
        .join(name)
}

fn read_fixture(name: &str) -> (PathBuf, Vec<u8>) {
    let path = fixture_path(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "failed to read synthetic fixture {}: {error}",
            path.display()
        )
    });
    (path, bytes)
}

fn hints() -> TagHints {
    TagHints {
        invoice_number: vec!["InvoiceNumber".into()],
        issue_date: vec!["IssueDate".into()],
        total_amount: vec!["TotalAmount".into()],
        tax_amount: vec!["TaxAmount".into()],
        tax_rate: vec!["TaxRate".into()],
        buyer_name: vec!["BuyerName".into()],
        seller_name: vec!["SellerName".into()],
    }
}

fn assert_golden_fields(invoice: &invoice_parse::model::ParsedInvoice) {
    assert_eq!(invoice.invoice_number, "26112000000000000001");
    assert_eq!(invoice.issue_date.to_string(), "2026-06-18");
    assert_eq!(invoice.total_amount, Decimal::from_str("1200.00").unwrap());
    assert_eq!(
        invoice.tax_amount,
        Some(Decimal::from_str("67.92").unwrap())
    );
    assert_eq!(invoice.tax_rate, Some(Decimal::from_str("0.06").unwrap()));
    assert_eq!(invoice.buyer_name.as_deref(), Some("北京示例科技有限公司"));
    assert_eq!(invoice.seller_name.as_deref(), Some("上海演示商贸有限公司"));
}

#[test]
fn generated_text_pdf_reaches_l1_with_all_golden_fields() {
    let (path, bytes) = read_fixture("vat-invoice-text.pdf");
    let invoice =
        invoice_parse::pdf::parse_invoice_pdf(&bytes, &path, &hints(), TicketType::Other).unwrap();

    assert_golden_fields(&invoice);
    assert_eq!(invoice.parse_level, ParseLevel::L1);
    assert_eq!(invoice.ticket_type, TicketType::Other);
}

#[test]
fn generated_ofd_reaches_l0_with_all_golden_fields() {
    let (path, bytes) = read_fixture("vat-invoice.ofd");
    let invoice =
        invoice_parse::ofd::parse_invoice_ofd(&bytes, &path, &hints(), TicketType::Other).unwrap();

    assert_golden_fields(&invoice);
    assert_eq!(invoice.parse_level, ParseLevel::L0);
    assert_eq!(invoice.ticket_type, TicketType::Other);
}

#[test]
fn malformed_pdf_returns_typed_error_without_panicking() {
    let (path, bytes) = read_fixture("malformed.pdf");
    let parsed = std::panic::catch_unwind(|| {
        invoice_parse::pdf::parse_invoice_pdf(&bytes, &path, &hints(), TicketType::Other)
    });

    let result = parsed.expect("malformed synthetic PDF must not panic");
    assert!(matches!(result, Err(ParseError::MalformedFormat { .. })));
}

#[test]
fn malformed_ofd_returns_typed_error_without_panicking() {
    let (path, bytes) = read_fixture("malformed.ofd");
    let parsed = std::panic::catch_unwind(|| {
        invoice_parse::ofd::parse_invoice_ofd(&bytes, &path, &hints(), TicketType::Other)
    });

    let result = parsed.expect("malformed synthetic OFD must not panic");
    assert!(matches!(result, Err(ParseError::MalformedFormat { .. })));
}
