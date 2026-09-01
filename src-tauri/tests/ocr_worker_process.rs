use invoice_assistant::ocr_worker::parse_with_worker;
use invoice_parse::model::{ParseLevel, TicketType};
use rust_decimal::Decimal;
use std::path::Path;
use std::str::FromStr;

fn assert_expected(invoice: &invoice_parse::model::ParsedInvoice) {
    assert_eq!(invoice.invoice_number, "26112000000000000001");
    assert_eq!(invoice.issue_date.to_string(), "2026-06-18");
    assert_eq!(invoice.total_amount, Decimal::from_str("1200.00").unwrap());
    assert_eq!(
        invoice.tax_amount,
        Some(Decimal::from_str("67.92").unwrap())
    );
    assert_eq!(invoice.buyer_name.as_deref(), Some("北京示例科技有限公司"));
    assert_eq!(invoice.seller_name.as_deref(), Some("上海演示商贸有限公司"));
    assert_eq!(invoice.parse_level, ParseLevel::L2);
    assert!(invoice.confidence >= 0.85);
}

#[test]
#[ignore = "由 scripts/verify-windows.ps1 构建并显式执行真实 OCR worker 进程 golden"]
fn production_worker_roundtrips_image_and_scanned_pdf() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let assets = root.join("src-tauri/assets/ocr");
    let image = root.join("fixtures/synthetic/ocr-vat-invoice.png");
    let scanned_pdf = root.join("fixtures/synthetic/ocr-vat-invoice-scanned.pdf");

    assert_expected(
        &parse_with_worker(&image, &assets, TicketType::Other)
            .expect("图片应通过独立 OCR worker 解析"),
    );
    assert_expected(
        &parse_with_worker(&scanned_pdf, &assets, TicketType::Other)
            .expect("扫描 PDF 应通过独立 OCR worker 解析"),
    );
}
