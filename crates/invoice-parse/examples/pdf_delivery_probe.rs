use chrono::NaiveDate;
use invoice_parse::manifest::TagHints;
use invoice_parse::model::TicketType;
use regex::Regex;
use std::collections::BTreeSet;
use std::path::Path;

const MARKERS: &[(&str, &str)] = &[
    ("invoice_number_label", "发票号码"),
    ("issue_time_label", "开票时间"),
    ("issue_date_label", "开票日期"),
    ("shipment_time_label", "寄件时间"),
    ("shipment_number_label", "运单号码"),
    ("invoice_total_label", "发票总金额"),
    ("delivery_detail_anchor", "运单明细"),
];

fn marker_names(text: &str) -> Vec<&'static str> {
    MARKERS
        .iter()
        .filter_map(|(name, marker)| text.contains(marker).then_some(*name))
        .collect()
}

fn number_tokens(text: &str) -> Vec<String> {
    let re = Regex::new(r"[A-Za-z]*\d{10,}").expect("probe regex must compile");
    let mut unique = Vec::new();
    for value in re.find_iter(text).map(|m| {
        m.as_str()
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
    }) {
        if !unique.contains(&value) {
            unique.push(value);
        }
    }
    unique
}

fn date_tokens(text: &str) -> Vec<NaiveDate> {
    let re =
        Regex::new(r"(\d{4})[-/年](\d{1,2})[-/月](\d{1,2})日?").expect("probe regex must compile");
    let mut unique = Vec::new();
    for captures in re.captures_iter(text) {
        let parsed = captures[1]
            .parse::<i32>()
            .ok()
            .zip(captures[2].parse::<u32>().ok())
            .zip(captures[3].parse::<u32>().ok())
            .and_then(|((year, month), day)| NaiveDate::from_ymd_opt(year, month, day));
        if let Some(value) = parsed {
            if !unique.contains(&value) {
                unique.push(value);
            }
        }
    }
    unique
}

fn number_token(value: &str, tokens: &[String]) -> String {
    tokens
        .iter()
        .position(|candidate| candidate == value)
        .map(|index| format!("N{}-len{}", index + 1, value.len()))
        .unwrap_or_else(|| format!("unmapped-len{}", value.len()))
}

fn date_token(value: NaiveDate, tokens: &[NaiveDate]) -> String {
    tokens
        .iter()
        .position(|candidate| *candidate == value)
        .map(|index| format!("D{}", index + 1))
        .unwrap_or_else(|| "unmapped".to_string())
}

fn main() {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: pdf_delivery_probe <pdf>");
        std::process::exit(2);
    };
    let path = Path::new(&path);
    let bytes = std::fs::read(path).expect("probe input must be readable");
    let text = invoice_parse::pdf::extract_text(&bytes, path).expect("text extraction must work");
    let numbers = number_tokens(&text);
    let dates = date_tokens(&text);

    println!("verification=private-pdf-delivery-structure-probe-v1");
    println!("number_tokens={}", numbers.len());
    for (index, value) in numbers.iter().enumerate() {
        println!("number_token_{}_length={}", index + 1, value.len());
    }
    println!("date_tokens={}", dates.len());
    for (index, line) in text.lines().enumerate() {
        let markers = marker_names(line);
        let line_numbers = number_tokens(line);
        let line_dates = date_tokens(line);
        if !markers.is_empty() || !line_numbers.is_empty() || !line_dates.is_empty() {
            let lengths = line_numbers
                .iter()
                .map(String::len)
                .map(|length| length.to_string())
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "line={} markers={} number_lengths={} date_count={}",
                index + 1,
                markers.join(","),
                lengths,
                line_dates.len()
            );
        }
    }

    let boxes = invoice_parse::pdf_text::extract_text_boxes(&bytes, path)
        .expect("positioned extraction must work");
    let mut box_summaries = BTreeSet::new();
    let decimal_re =
        Regex::new(r"^[￥¥]?\s*[\d,]+\.\d{1,2}\s*$").expect("probe regex must compile");
    for text_box in &boxes {
        let markers = marker_names(&text_box.text);
        let box_numbers = number_tokens(&text_box.text);
        let box_dates = date_tokens(&text_box.text);
        let is_decimal = decimal_re.is_match(text_box.text.trim());
        if !markers.is_empty() || !box_numbers.is_empty() || !box_dates.is_empty() || is_decimal {
            let lengths = box_numbers
                .iter()
                .map(String::len)
                .map(|length| length.to_string())
                .collect::<Vec<_>>()
                .join(",");
            box_summaries.insert(format!(
                "box x={:.0} y={:.0} w={:.0} markers={} number_lengths={} date_count={} decimal={}",
                text_box.x,
                text_box.y,
                text_box.width,
                markers.join(","),
                lengths,
                box_dates.len(),
                is_decimal
            ));
        }
    }
    println!("relevant_boxes={}", box_summaries.len());
    for summary in box_summaries {
        println!("{summary}");
    }

    let merged = invoice_parse::ocr::merge_line_fragments(boxes.clone(), 12.0);
    println!("merged_boxes={}", merged.len());
    for text_box in &merged {
        let markers = marker_names(&text_box.text);
        let box_numbers = number_tokens(&text_box.text);
        let box_dates = date_tokens(&text_box.text);
        let is_decimal = decimal_re.is_match(text_box.text.trim());
        if !markers.is_empty() || !box_numbers.is_empty() || !box_dates.is_empty() || is_decimal {
            println!(
                "merged x={:.0} y={:.0} w={:.0} markers={} number_lengths={} date_count={} decimal={}",
                text_box.x,
                text_box.y,
                text_box.width,
                markers.join(","),
                box_numbers.iter().map(String::len).map(|length| length.to_string()).collect::<Vec<_>>().join(","),
                box_dates.len(),
                is_decimal
            );
        }
    }

    match invoice_parse::pdf_text::parse_vat_invoice_from_boxes(&bytes, path) {
        Ok(parsed) => {
            println!("positioned_succeeded=true");
            println!(
                "positioned_number_token={}",
                number_token(&parsed.invoice_number, &numbers)
            );
            println!(
                "positioned_date_token={}",
                date_token(parsed.issue_date, &dates)
            );
        }
        Err(_) => println!("positioned_succeeded=false"),
    }

    let hints = TagHints {
        invoice_number: vec![],
        issue_date: vec![],
        total_amount: vec![],
        tax_amount: vec![],
        tax_rate: vec![],
        buyer_name: vec![],
        seller_name: vec![],
    };
    match invoice_parse::pdf::parse_invoice_pdf(&bytes, path, &hints, TicketType::Other) {
        Ok(parsed) => {
            println!("flat_succeeded=true");
            println!(
                "flat_number_token={}",
                number_token(&parsed.invoice_number, &numbers)
            );
            println!("flat_date_token={}", date_token(parsed.issue_date, &dates));
        }
        Err(_) => println!("flat_succeeded=false"),
    }
    println!("private_values_logged=false");
}
