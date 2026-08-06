use chrono::{NaiveDate, NaiveDateTime};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 票种。与产品方案的核心数据模型保持一致，不含任何报销系统概念。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TicketType {
    Rail,
    Flight,
    Hotel,
    CityTransport,
    Meal,
    Other,
}

/// 解析级别。决定字段可信度与是否需要人工介入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParseLevel {
    /// 结构化数据直读（数电票 XML、OFD 内嵌 XML）
    L0,
    /// PDF 文本层 + 版式模板
    L1,
    /// 本地 OCR
    L2,
    /// 关键字段冲突，强制人工
    L4,
}

/// 所有解析器的统一输出。这是本 crate 唯一的输出契约。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedInvoice {
    pub invoice_number: String,
    pub issue_date: NaiveDate,
    pub total_amount: Decimal,
    pub tax_amount: Option<Decimal>,
    pub tax_rate: Option<Decimal>,
    pub buyer_name: Option<String>,
    pub seller_name: Option<String>,
    pub ticket_type: TicketType,
    pub parse_level: ParseLevel,
    /// 0.0–1.0。L0 恒为 1.0，L2 由 OCR 引擎给出。
    pub confidence: f32,
    /// 发票关联城市（交通票为出发城市，酒店为入住城市，其他为消费城市）
    pub city: Option<String>,
    /// 交通票出发时间（用于行程时间轴排序）
    pub departure_time: Option<NaiveDateTime>,
    /// 酒店入住日期（注意：不是 issue_date，酒店常延迟开票）
    pub checkin_date: Option<NaiveDate>,
    pub source_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("无法读取文件 {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} 不是有效的 {format} 格式: {detail}")]
    MalformedFormat {
        path: PathBuf,
        format: &'static str,
        detail: String,
    },
    #[error("在 {path} 中找不到必需字段 {field}")]
    MissingField { path: PathBuf, field: String },
    #[error("字段 {field} 的值 {raw:?} 无法解析为 {expected_type}")]
    UnparseableValue {
        field: String,
        raw: String,
        expected_type: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromStr;

    #[test]
    fn parsed_invoice_roundtrips_through_json() {
        let invoice = ParsedInvoice {
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
            city: None,
            departure_time: None,
            checkin_date: None,
            source_path: PathBuf::from("fixtures/samples/rail-01.xml"),
        };

        let json = serde_json::to_string(&invoice).expect("序列化失败");
        let restored: ParsedInvoice = serde_json::from_str(&json).expect("反序列化失败");

        assert_eq!(invoice, restored);
    }

    #[test]
    fn decimal_amounts_sum_without_drift() {
        // 用 f64 时 0.1 + 0.2 != 0.3，Decimal 必须精确。
        // 这是金额对账能成立的前提。
        let a = Decimal::from_str("0.1").unwrap();
        let b = Decimal::from_str("0.2").unwrap();
        let c = Decimal::from_str("0.3").unwrap();
        assert_eq!(a + b, c);
    }
}
