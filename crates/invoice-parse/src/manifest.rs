use crate::model::{ParseLevel, ParsedInvoice, TicketType};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Manifest {
    #[serde(default, rename = "sample")]
    pub samples: Vec<Sample>,
}

/// 一个样本的期望值声明。数值一律用 String 存，
/// 让清单保持人类可写，比对时再转成 Decimal/NaiveDate。
#[derive(Debug, Deserialize)]
pub struct Sample {
    pub path: PathBuf,
    pub format: String,
    pub ticket_type: TicketType,
    pub invoice_number: String,
    pub issue_date: String,
    pub total_amount: String,
    #[serde(default)]
    pub tax_amount: Option<String>,
    #[serde(default)]
    pub tax_rate: Option<String>,
    #[serde(default)]
    pub buyer_name: Option<String>,
    #[serde(default)]
    pub seller_name: Option<String>,
    /// 该样本是否为已作废/红冲票（验签负例）
    #[serde(default)]
    pub is_voided: bool,
    /// XML/OFD 元素名提示，由 Task 3 的探查工具填入
    #[serde(default)]
    pub xml_tag_hints: Option<TagHints>,
}

/// 不同开票平台的数电票 XML 元素名不统一，
/// 用这个结构声明每个字段的候选标签名（按优先级排列）。
#[derive(Debug, Clone, Deserialize)]
pub struct TagHints {
    #[serde(default)]
    pub invoice_number: Vec<String>,
    #[serde(default)]
    pub issue_date: Vec<String>,
    #[serde(default)]
    pub total_amount: Vec<String>,
    #[serde(default)]
    pub tax_amount: Vec<String>,
    #[serde(default)]
    pub tax_rate: Vec<String>,
    #[serde(default)]
    pub buyer_name: Vec<String>,
    #[serde(default)]
    pub seller_name: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub struct FieldComparison {
    pub field: &'static str,
    pub expected: String,
    pub actual: String,
    pub matched: bool,
}

impl Manifest {
    pub fn load(path: &Path) -> anyhow::Result<Manifest> {
        let src = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读不到清单 {}: {e}", path.display()))?;
        let manifest: Manifest = toml::from_str(&src)
            .map_err(|e| anyhow::anyhow!("清单 {} 格式错误: {e}", path.display()))?;
        Ok(manifest)
    }
}

impl Sample {
    pub fn compare(&self, actual: &ParsedInvoice) -> Vec<FieldComparison> {
        let mut out = Vec::new();

        out.push(compare_str(
            "invoice_number",
            &self.invoice_number,
            &actual.invoice_number,
        ));

        out.push(compare_date("issue_date", &self.issue_date, actual.issue_date));

        out.push(compare_decimal(
            "total_amount",
            &self.total_amount,
            Some(actual.total_amount),
        ));

        if let Some(expected) = &self.tax_amount {
            out.push(compare_decimal("tax_amount", expected, actual.tax_amount));
        }
        if let Some(expected) = &self.tax_rate {
            out.push(compare_decimal("tax_rate", expected, actual.tax_rate));
        }
        if let Some(expected) = &self.buyer_name {
            out.push(compare_opt_str(
                "buyer_name",
                expected,
                actual.buyer_name.as_deref(),
            ));
        }
        if let Some(expected) = &self.seller_name {
            out.push(compare_opt_str(
                "seller_name",
                expected,
                actual.seller_name.as_deref(),
            ));
        }

        out.push(FieldComparison {
            field: "ticket_type",
            expected: format!("{:?}", self.ticket_type),
            actual: format!("{:?}", actual.ticket_type),
            matched: self.ticket_type == actual.ticket_type,
        });

        out
    }
}

fn compare_str(field: &'static str, expected: &str, actual: &str) -> FieldComparison {
    FieldComparison {
        field,
        expected: expected.to_string(),
        actual: actual.to_string(),
        matched: expected == actual,
    }
}

fn compare_opt_str(
    field: &'static str,
    expected: &str,
    actual: Option<&str>,
) -> FieldComparison {
    FieldComparison {
        field,
        expected: expected.to_string(),
        actual: actual.unwrap_or("<缺失>").to_string(),
        matched: actual == Some(expected),
    }
}

/// 数值比对走 Decimal，"553" 与 "553.00" 视为相等。
fn compare_decimal(
    field: &'static str,
    expected_raw: &str,
    actual: Option<Decimal>,
) -> FieldComparison {
    use rust_decimal::prelude::FromStr;

    let expected = Decimal::from_str(expected_raw).ok();
    let matched = match (expected, actual) {
        (Some(e), Some(a)) => e == a,
        _ => false,
    };
    FieldComparison {
        field,
        expected: expected_raw.to_string(),
        actual: actual.map(|d| d.to_string()).unwrap_or_else(|| "<缺失>".into()),
        matched,
    }
}

fn compare_date(
    field: &'static str,
    expected_raw: &str,
    actual: chrono::NaiveDate,
) -> FieldComparison {
    let expected = chrono::NaiveDate::parse_from_str(expected_raw, "%Y-%m-%d").ok();
    FieldComparison {
        field,
        expected: expected_raw.to_string(),
        actual: actual.to_string(),
        matched: expected == Some(actual),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rust_decimal::prelude::FromStr;

    fn sample_fixture() -> Sample {
        Sample {
            path: PathBuf::from("samples/rail-01.xml"),
            format: "xml".to_string(),
            ticket_type: TicketType::Rail,
            invoice_number: "24312000000012345678".to_string(),
            issue_date: "2026-07-03".to_string(),
            total_amount: "553.00".to_string(),
            tax_amount: Some("50.73".to_string()),
            tax_rate: Some("0.09".to_string()),
            buyer_name: Some("某某公司".to_string()),
            seller_name: None,
            is_voided: false,
            xml_tag_hints: None,
        }
    }

    fn parsed_fixture() -> ParsedInvoice {
        ParsedInvoice {
            invoice_number: "24312000000012345678".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 3).unwrap(),
            total_amount: Decimal::from_str("553.00").unwrap(),
            tax_amount: Some(Decimal::from_str("50.73").unwrap()),
            tax_rate: Some(Decimal::from_str("0.09").unwrap()),
            buyer_name: Some("某某公司".to_string()),
            seller_name: Some("中国铁路".to_string()),
            ticket_type: TicketType::Rail,
            parse_level: ParseLevel::L0,
            confidence: 1.0,
            source_path: PathBuf::from("samples/rail-01.xml"),
        }
    }

    #[test]
    fn all_declared_fields_match() {
        let comparisons = sample_fixture().compare(&parsed_fixture());
        let failed: Vec<_> = comparisons.iter().filter(|c| !c.matched).collect();
        assert!(failed.is_empty(), "预期全部匹配，实际失败项: {failed:?}");
    }

    #[test]
    fn amount_mismatch_is_detected() {
        let mut parsed = parsed_fixture();
        parsed.total_amount = Decimal::from_str("12.80").unwrap();

        let comparisons = sample_fixture().compare(&parsed);
        let amount = comparisons
            .iter()
            .find(|c| c.field == "total_amount")
            .expect("应有 total_amount 比对项");

        assert!(!amount.matched);
        assert_eq!(amount.expected, "553.00");
        assert_eq!(amount.actual, "12.80");
    }

    #[test]
    fn trailing_zeros_do_not_cause_false_mismatch() {
        // 清单写 "553.00"，解析出 553 —— Decimal 数值相等，应判匹配
        let mut parsed = parsed_fixture();
        parsed.total_amount = Decimal::from_str("553").unwrap();

        let comparisons = sample_fixture().compare(&parsed);
        let amount = comparisons
            .iter()
            .find(|c| c.field == "total_amount")
            .unwrap();

        assert!(amount.matched, "553 与 553.00 应视为相等");
    }

    #[test]
    fn fields_absent_from_manifest_are_not_compared() {
        // sample_fixture 的 seller_name 是 None，不应产生比对项
        let comparisons = sample_fixture().compare(&parsed_fixture());
        assert!(
            comparisons.iter().all(|c| c.field != "seller_name"),
            "未声明的字段不应参与比对"
        );
    }

    #[test]
    fn manifest_parses_sample_array() {
        let toml_src = r#"
[[sample]]
path = "samples/a.xml"
format = "xml"
ticket_type = "Rail"
invoice_number = "111"
issue_date = "2026-07-03"
total_amount = "100.00"

[[sample]]
path = "samples/b.ofd"
format = "ofd"
ticket_type = "Hotel"
invoice_number = "222"
issue_date = "2026-07-04"
total_amount = "200.00"
tax_rate = "0.06"
"#;
        let manifest: Manifest = toml::from_str(toml_src).expect("清单应能解析");
        assert_eq!(manifest.samples.len(), 2);
        assert_eq!(manifest.samples[1].tax_rate.as_deref(), Some("0.06"));
    }
}
