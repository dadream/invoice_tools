use crate::model::{ParsedInvoice, TicketType};
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
///
/// 所有期望字段都是 Option：`None` 表示"人工尚未核对"，
/// 不代表解析失败。这是通过率能被正确统计的前提。
#[derive(Debug, Deserialize)]
pub struct Sample {
    pub path: PathBuf,
    pub format: String,
    #[serde(default)]
    pub ticket_type: Option<TicketType>,
    #[serde(default)]
    pub invoice_number: Option<String>,
    #[serde(default)]
    pub issue_date: Option<String>,
    #[serde(default)]
    pub total_amount: Option<String>,
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
    /// 人工判定该文件是否为发票。None 表示未判定。
    /// 由 Task 6 的分类器对照，非发票不计入解析通过率分母。
    #[serde(default)]
    pub is_invoice: Option<bool>,
    /// `is_invoice = false` 时说明排除理由，会出现在验证报告中。
    #[serde(default)]
    pub not_invoice_reason: Option<String>,
    /// XML/OFD 元素名提示，由 explore-xml 工具填入
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

/// 单个字段的比对结果。
#[derive(Debug, Clone, PartialEq)]
pub struct FieldComparison {
    pub field: &'static str,
    pub expected: String,
    pub actual: String,
    pub status: FieldStatus,
}

/// 比对状态。`Unverified` 表示清单里没填期望值——
/// 既不算通过也不算失败，只是还没人核对过。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldStatus {
    Match,
    Mismatch,
    Unverified,
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
    /// 清单里是否已有可比对的期望值（三个必填字段任一已填）。
    pub fn has_ground_truth(&self) -> bool {
        self.invoice_number.is_some()
            || self.issue_date.is_some()
            || self.total_amount.is_some()
    }

    pub fn compare(&self, actual: &ParsedInvoice) -> Vec<FieldComparison> {
        vec![
            cmp_str("invoice_number", self.invoice_number.as_deref(), Some(actual.invoice_number.as_str())),
            cmp_date("issue_date", self.issue_date.as_deref(), actual.issue_date),
            cmp_decimal("total_amount", self.total_amount.as_deref(), Some(actual.total_amount)),
            cmp_decimal("tax_amount", self.tax_amount.as_deref(), actual.tax_amount),
            cmp_decimal("tax_rate", self.tax_rate.as_deref(), actual.tax_rate),
            cmp_str("buyer_name", self.buyer_name.as_deref(), actual.buyer_name.as_deref()),
            cmp_str("seller_name", self.seller_name.as_deref(), actual.seller_name.as_deref()),
            cmp_ticket_type("ticket_type", self.ticket_type, actual.ticket_type),
        ]
    }
}

const MISSING: &str = "<缺失>";

/// 没填期望值 → Unverified。填了但实际缺失 → Mismatch。
fn cmp_str(
    field: &'static str,
    expected: Option<&str>,
    actual: Option<&str>,
) -> FieldComparison {
    let actual_s = actual.unwrap_or(MISSING).to_string();
    match expected {
        None => FieldComparison {
            field,
            expected: String::new(),
            actual: actual_s,
            status: FieldStatus::Unverified,
        },
        Some(e) => FieldComparison {
            field,
            expected: e.to_string(),
            actual: actual_s,
            status: if actual == Some(e) {
                FieldStatus::Match
            } else {
                FieldStatus::Mismatch
            },
        },
    }
}

/// 数值比对走 Decimal，"553" 与 "553.00" 视为相等。
/// 期望值本身解析不出来时算 Mismatch，不 panic。
fn cmp_decimal(
    field: &'static str,
    expected: Option<&str>,
    actual: Option<Decimal>,
) -> FieldComparison {
    use rust_decimal::prelude::FromStr;
    let actual_s = actual.map(|d| d.to_string()).unwrap_or_else(|| MISSING.into());
    match expected {
        None => FieldComparison {
            field,
            expected: String::new(),
            actual: actual_s,
            status: FieldStatus::Unverified,
        },
        Some(raw) => {
            let matched = match (Decimal::from_str(raw).ok(), actual) {
                (Some(e), Some(a)) => e == a,
                _ => false,
            };
            FieldComparison {
                field,
                expected: raw.to_string(),
                actual: actual_s,
                status: if matched { FieldStatus::Match } else { FieldStatus::Mismatch },
            }
        }
    }
}

fn cmp_date(
    field: &'static str,
    expected: Option<&str>,
    actual: chrono::NaiveDate,
) -> FieldComparison {
    let actual_s = actual.to_string();
    match expected {
        None => FieldComparison {
            field,
            expected: String::new(),
            actual: actual_s,
            status: FieldStatus::Unverified,
        },
        Some(raw) => {
            let matched = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok() == Some(actual);
            FieldComparison {
                field,
                expected: raw.to_string(),
                actual: actual_s,
                status: if matched { FieldStatus::Match } else { FieldStatus::Mismatch },
            }
        }
    }
}

fn cmp_ticket_type(
    field: &'static str,
    expected: Option<TicketType>,
    actual: TicketType,
) -> FieldComparison {
    let actual_s = format!("{actual:?}");
    match expected {
        None => FieldComparison {
            field,
            expected: String::new(),
            actual: actual_s,
            status: FieldStatus::Unverified,
        },
        Some(e) => FieldComparison {
            field,
            expected: format!("{e:?}"),
            actual: actual_s,
            status: if e == actual { FieldStatus::Match } else { FieldStatus::Mismatch },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rust_decimal::prelude::FromStr;

    fn parsed() -> ParsedInvoice {
        ParsedInvoice {
            invoice_number: "24312000000012345678".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 3).unwrap(),
            total_amount: Decimal::from_str("553.00").unwrap(),
            tax_amount: Some(Decimal::from_str("50.73").unwrap()),
            tax_rate: None,
            buyer_name: Some("某某公司".to_string()),
            seller_name: None,
            ticket_type: TicketType::Rail,
            parse_level: crate::model::ParseLevel::L0,
            confidence: 1.0,
            city: None,
            departure_time: None,
            checkin_date: None,
            source_path: PathBuf::from("samples/rail-01.xml"),
        }
    }

    /// 完全没填期望值的样本：每个字段都是 Unverified，一个 Mismatch 都不能有。
    /// 这是 Task 1 的核心——空清单不等于解析错误。
    fn blank_sample() -> Sample {
        Sample {
            path: PathBuf::from("samples/rail-01.xml"),
            format: "xml".to_string(),
            ticket_type: None,
            invoice_number: None,
            issue_date: None,
            total_amount: None,
            tax_amount: None,
            tax_rate: None,
            buyer_name: None,
            seller_name: None,
            is_voided: false,
            is_invoice: None,
            not_invoice_reason: None,
            xml_tag_hints: None,
        }
    }

    #[test]
    fn blank_expectations_are_unverified_not_mismatched() {
        let cs = blank_sample().compare(&parsed());
        assert!(
            cs.iter().all(|c| c.status == FieldStatus::Unverified),
            "空期望值必须全部是 Unverified，实际: {cs:?}"
        );
        assert!(!cs.is_empty(), "仍应为每个字段产出一条记录");
    }

    #[test]
    fn blank_sample_reports_no_ground_truth() {
        assert!(!blank_sample().has_ground_truth());
    }

    #[test]
    fn filled_sample_reports_ground_truth() {
        let mut s = blank_sample();
        s.invoice_number = Some("24312000000012345678".to_string());
        s.issue_date = Some("2026-07-03".to_string());
        s.total_amount = Some("553.00".to_string());
        assert!(s.has_ground_truth());
    }

    #[test]
    fn matching_values_are_match() {
        let mut s = blank_sample();
        s.invoice_number = Some("24312000000012345678".to_string());
        s.issue_date = Some("2026-07-03".to_string());
        s.total_amount = Some("553.00".to_string());
        s.ticket_type = Some(TicketType::Rail);
        let cs = s.compare(&parsed());
        for f in ["invoice_number", "issue_date", "total_amount", "ticket_type"] {
            let c = cs.iter().find(|c| c.field == f).expect(f);
            assert_eq!(c.status, FieldStatus::Match, "{f} 应匹配: {c:?}");
        }
    }

    #[test]
    fn wrong_value_is_mismatch() {
        let mut s = blank_sample();
        s.invoice_number = Some("99999999999999999999".to_string());
        let c = s
            .compare(&parsed())
            .into_iter()
            .find(|c| c.field == "invoice_number")
            .unwrap();
        assert_eq!(c.status, FieldStatus::Mismatch);
    }

    /// "553" 与 "553.00" 必须相等——金额比对走 Decimal，不是字符串。
    #[test]
    fn decimal_compare_ignores_trailing_zeros() {
        let mut s = blank_sample();
        s.total_amount = Some("553".to_string());
        let c = s
            .compare(&parsed())
            .into_iter()
            .find(|c| c.field == "total_amount")
            .unwrap();
        assert_eq!(c.status, FieldStatus::Match);
    }

    /// 期望值填了但解析结果缺失该字段 —— 必须是 Mismatch，不能悄悄算过。
    #[test]
    fn expected_but_missing_is_mismatch() {
        let mut s = blank_sample();
        s.tax_rate = Some("0.09".to_string());
        let c = s
            .compare(&parsed())
            .into_iter()
            .find(|c| c.field == "tax_rate")
            .unwrap();
        assert_eq!(c.status, FieldStatus::Mismatch);
        assert_eq!(c.actual, "<缺失>");
    }

    /// 期望值本身写坏了（不是合法 Decimal）要报 Mismatch，而不是 panic。
    #[test]
    fn unparseable_expectation_is_mismatch_not_panic() {
        let mut s = blank_sample();
        s.total_amount = Some("五百五十三".to_string());
        let c = s
            .compare(&parsed())
            .into_iter()
            .find(|c| c.field == "total_amount")
            .unwrap();
        assert_eq!(c.status, FieldStatus::Mismatch);
    }
}
