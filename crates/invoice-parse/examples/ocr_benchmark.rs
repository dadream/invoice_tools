use invoice_parse::model::{ParseLevel, ParsedInvoice, TicketType};
use invoice_parse::{ocr, pdf_ocr};
use rust_decimal::Decimal;
use serde_json::json;
use std::env;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Instant;

fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn assert_expected(invoice: &ParsedInvoice) -> Result<(), String> {
    let expected_total = Decimal::from_str("1200.00").map_err(|error| error.to_string())?;
    let expected_tax = Decimal::from_str("67.92").map_err(|error| error.to_string())?;
    if invoice.invoice_number != "26112000000000000001"
        || invoice.issue_date.to_string() != "2026-06-18"
        || invoice.total_amount != expected_total
        || invoice.tax_amount != Some(expected_tax)
        || invoice.buyer_name.as_deref() != Some("北京示例科技有限公司")
        || invoice.seller_name.as_deref() != Some("上海演示商贸有限公司")
        || invoice.parse_level != ParseLevel::L2
    {
        return Err("OCR benchmark output did not match the synthetic golden".to_string());
    }
    Ok(())
}

fn percentile(sorted: &[u128], numerator: usize, denominator: usize) -> u128 {
    let index = (sorted.len() * numerator)
        .div_ceil(denominator)
        .saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

fn summarize(mode: &str, durations_ms: &[u128]) -> serde_json::Value {
    let mut sorted = durations_ms.to_vec();
    sorted.sort_unstable();
    let warm = if sorted.len() > 1 {
        &durations_ms[1..]
    } else {
        durations_ms
    };
    let warm_sum: u128 = warm.iter().sum();
    let warm_mean = warm_sum as f64 / warm.len() as f64;
    json!({
        "schemaVersion": 1,
        "mode": mode,
        "iterations": durations_ms.len(),
        "coldMs": durations_ms[0],
        "allMs": durations_ms,
        "minMs": sorted[0],
        "p50Ms": percentile(&sorted, 50, 100),
        "p95Ms": percentile(&sorted, 95, 100),
        "maxMs": sorted[sorted.len() - 1],
        "warmMeanMs": warm_mean,
        "estimated50WarmSeconds": warm_mean * 50.0 / 1000.0,
        "target50Seconds": 300,
        "estimated50WarmWithinTarget": warm_mean * 50.0 <= 300_000.0,
    })
}

fn parse_args() -> Result<(String, usize), String> {
    let mut mode = None;
    let mut iterations = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => mode = args.next(),
            "--iterations" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "--iterations requires a value".to_string())?;
                iterations = Some(
                    raw.parse::<usize>()
                        .map_err(|_| "--iterations must be an integer".to_string())?,
                );
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    let mode = mode.ok_or_else(|| "--mode image|pdf is required".to_string())?;
    if mode != "image" && mode != "pdf" {
        return Err("--mode must be image or pdf".to_string());
    }
    let iterations = iterations.unwrap_or(3);
    if !(2..=20).contains(&iterations) {
        return Err("--iterations must be between 2 and 20".to_string());
    }
    Ok((mode, iterations))
}

fn run() -> Result<serde_json::Value, String> {
    let (mode, iterations) = parse_args()?;
    let root = project_root();
    let asset_dir = root.join("src-tauri/assets/ocr");
    let input = match mode.as_str() {
        "image" => root.join("fixtures/synthetic/ocr-vat-invoice.png"),
        "pdf" => root.join("fixtures/synthetic/ocr-vat-invoice-scanned.pdf"),
        _ => unreachable!(),
    };
    let mut durations_ms = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        let invoice = if mode == "image" {
            ocr::parse_invoice_image(&input, &asset_dir).map_err(|error| error.to_string())?
        } else {
            pdf_ocr::parse_scanned_invoice_pdf(&input, &asset_dir, TicketType::Other)
                .map_err(|error| error.to_string())?
        };
        durations_ms.push(started.elapsed().as_millis());
        assert_expected(&invoice)?;
    }
    let mut result = summarize(&mode, &durations_ms);
    result["input"] = json!(input
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown"));
    result["ocrConcurrency"] = json!(1);
    result["detectionMaxSide"] = json!(1800);
    result["intraOpThreads"] = json!(2);
    result["interOpThreads"] = json!(1);
    result["cpuArena"] = json!(false);
    result["memoryPattern"] = json!(false);
    Ok(result)
}

fn main() {
    match run() {
        Ok(result) => println!("{}", serde_json::to_string(&result).unwrap()),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
