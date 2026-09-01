//! OCR 结构诊断工具：只输出标签/模式/置信度计数，不输出 OCR 文本或字段值。

use invoice_parse::ocr::{self, OfflineOcrError};
use regex::Regex;
use serde_json::json;
use std::env;
use std::path::PathBuf;

const LABELS: &[(&str, &str)] = &[
    ("electronic_invoice", "电子发票"),
    ("vat_invoice", "增值税"),
    ("invoice_number", "发票号码"),
    ("issue_date", "开票日期"),
    ("total_amount", "价税合计"),
    ("lowercase_amount", "小写"),
    ("amount", "金额"),
    ("tax_amount", "税额"),
    ("buyer", "购买方"),
    ("seller", "销售方"),
    ("issuance_result", "开具结果"),
    ("issuance_success", "开票成功"),
    ("scan_to_download", "扫码下载发票"),
    ("continue_issuance", "继续开票"),
];

fn parse_args() -> Result<(PathBuf, PathBuf), String> {
    let mut image = None;
    let mut assets = None;
    let mut args = env::args_os().skip(1);
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--image") => image = args.next().map(PathBuf::from),
            Some("--assets") => assets = args.next().map(PathBuf::from),
            _ => return Err("usage: ocr_structure_probe --image PATH --assets PATH".to_string()),
        }
    }
    Ok((
        image.ok_or_else(|| "--image is required".to_string())?,
        assets.ok_or_else(|| "--assets is required".to_string())?,
    ))
}

fn error_category(error: &OfflineOcrError) -> serde_json::Value {
    match error {
        OfflineOcrError::MissingField { field } => {
            json!({"status": "missing_field", "field": field})
        }
        OfflineOcrError::InvalidField { field } => {
            json!({"status": "invalid_field", "field": field})
        }
        OfflineOcrError::AssetMissing { .. } => json!({"status": "asset_missing"}),
        OfflineOcrError::AssetIntegrity { .. } => json!({"status": "asset_integrity"}),
        OfflineOcrError::ImageFileTooLarge => json!({"status": "image_too_large"}),
        OfflineOcrError::ImageDimensionsTooLarge => json!({"status": "dimensions_too_large"}),
        OfflineOcrError::ImageDecode => json!({"status": "image_decode"}),
        OfflineOcrError::EngineInitialization => json!({"status": "engine_initialization"}),
        OfflineOcrError::Inference => json!({"status": "inference"}),
        OfflineOcrError::EngineUnavailable => json!({"status": "engine_unavailable"}),
    }
}

fn main() {
    let result = (|| -> Result<serde_json::Value, String> {
        let (image, assets) = parse_args()?;
        let boxes = ocr::recognize_offline(&image, &assets).map_err(|error| error.to_string())?;
        let joined = boxes
            .iter()
            .map(|text_box| text_box.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let compact = boxes
            .iter()
            .map(|text_box| text_box.text.as_str())
            .collect::<String>();
        let label_presence = LABELS
            .iter()
            .map(|(key, label)| ((*key).to_string(), json!(joined.contains(label))))
            .collect::<serde_json::Map<String, serde_json::Value>>();
        let long_digits = Regex::new(r"\b\d{10,24}\b").map_err(|error| error.to_string())?;
        let dates = Regex::new(r"20\d{2}[-/.年]\d{1,2}[-/.月]\d{1,2}日?")
            .map_err(|error| error.to_string())?;
        let decimals = Regex::new(r"\b\d[\d,]*\.\d{2}\b").map_err(|error| error.to_string())?;
        let confidence_sum = boxes
            .iter()
            .map(|text_box| text_box.confidence as f64)
            .sum::<f64>();
        let parse = match ocr::parse_invoice_image(&image, &assets) {
            Ok(_) => json!({"status": "success"}),
            Err(error) => error_category(&error),
        };
        Ok(json!({
            "verification": "ocr-structure-probe-v1",
            "box_count": boxes.len(),
            "confidence_mean": if boxes.is_empty() { 0.0 } else { confidence_sum / boxes.len() as f64 },
            "confidence_below_0_5": boxes.iter().filter(|text_box| text_box.confidence < 0.5).count(),
            "label_presence": label_presence,
            "long_digit_pattern_count_joined": long_digits.find_iter(&joined).count(),
            "long_digit_pattern_count_compact": long_digits.find_iter(&compact).count(),
            "date_pattern_count_joined": dates.find_iter(&joined).count(),
            "date_pattern_count_compact": dates.find_iter(&compact).count(),
            "decimal_pattern_count_joined": decimals.find_iter(&joined).count(),
            "decimal_pattern_count_compact": decimals.find_iter(&compact).count(),
            "digit_character_count": joined.chars().filter(char::is_ascii_digit).count(),
            "parse": parse,
            "private_values_logged": false,
        }))
    })();

    match result {
        Ok(report) => println!("{}", serde_json::to_string_pretty(&report).unwrap()),
        Err(error) => {
            eprintln!("probe_failed={error}");
            std::process::exit(1);
        }
    }
}
