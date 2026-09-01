use regex::Regex;
use std::path::Path;

const NUMBER_LABEL: &str = r"账单号码\s*/\s*Invoice\s*No\.?";
const DATE_LABEL: &str = r"打印日期\s*/\s*Print\s*Date";
const TOTAL_LABEL: &str = r"总计\s*/\s*Total";
const NUMBER: &str = r"\d{6,20}";
const DMY_DATE: &str = r"\d{1,2}/\d{1,2}/\d{4}";
const DMY_TIMESTAMP: &str = r"\d{1,2}/\d{1,2}/\d{4}\s+\d{1,2}:\d{2}:\d{2}";
const AMOUNT: &str = r"[￥¥]?\s*[\d,]+\.\d{1,2}";

fn has(pattern: &str, text: &str) -> bool {
    Regex::new(pattern)
        .expect("probe regex must compile")
        .is_match(text)
}

fn around(label: &str, value: &str, text: &str) -> (bool, bool) {
    let after = format!(r"(?:{label})[\s：:]*({value})");
    let before = format!(r"({value})[\s：:]*(?:{label})");
    (has(&before, text), has(&after, text))
}

fn main() {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: pdf_structure_probe <pdf>");
        std::process::exit(2);
    };
    let path = Path::new(&path);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => {
            println!("read_succeeded=false");
            std::process::exit(1);
        }
    };
    println!("read_succeeded=true");

    let extracted = std::panic::catch_unwind(|| invoice_parse::pdf::extract_text(&bytes, path));
    let text = match extracted {
        Ok(Ok(text)) => text,
        Ok(Err(_)) => {
            println!("text_extract_succeeded=false");
            println!("text_extract_panicked=false");
            return;
        }
        Err(_) => {
            println!("text_extract_succeeded=false");
            println!("text_extract_panicked=true");
            return;
        }
    };
    println!("text_extract_succeeded=true");
    println!("text_extract_panicked=false");
    println!(
        "non_whitespace_chars={}",
        text.chars()
            .filter(|character| !character.is_whitespace())
            .count()
    );

    let (number_before, number_after) = around(NUMBER_LABEL, NUMBER, &text);
    let (date_before, date_after) = around(DATE_LABEL, DMY_DATE, &text);
    let (total_before, total_after) = around(TOTAL_LABEL, AMOUNT, &text);
    println!("number_label={}", has(NUMBER_LABEL, &text));
    println!("number_before_label={number_before}");
    println!("number_after_label={number_after}");
    println!("date_label={}", has(DATE_LABEL, &text));
    println!("date_before_label={date_before}");
    println!("date_after_label={date_after}");
    println!(
        "dmy_timestamp_count={}",
        Regex::new(DMY_TIMESTAMP)
            .expect("timestamp regex must compile")
            .find_iter(&text)
            .count()
    );
    println!("total_label={}", has(TOTAL_LABEL, &text));
    println!("amount_before_total_label={total_before}");
    println!("amount_after_total_label={total_after}");

    match invoice_parse::pdf::parse_vat_invoice_text(&text, path) {
        Ok(parsed) => {
            println!("parse_succeeded=true");
            println!("ticket_type={:?}", parsed.ticket_type);
        }
        Err(error) => {
            let message = error.to_string();
            let category = ["invoice_number", "issue_date", "total_amount"]
                .into_iter()
                .find(|field| message.contains(field))
                .unwrap_or("other");
            println!("parse_succeeded=false");
            println!("error_field={category}");
        }
    }
    println!("private_text_or_values_logged=false");
}
