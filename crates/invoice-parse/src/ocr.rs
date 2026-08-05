use crate::model::{ParseError, ParseLevel, ParsedInvoice, TicketType};
use crate::xml::{parse_amount, parse_date, parse_tax_rate};
use std::path::Path;
use serde::{Deserialize, Serialize};

/// OCR 识别出的一个文本框。坐标为左上角原点的像素值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextBox {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub confidence: f32,
}

impl TextBox {
    fn center_y(&self) -> f32 {
        self.y + self.height / 2.0
    }
    fn right(&self) -> f32 {
        self.x + self.width
    }
}

/// 同一行的判定阈值：两个框的垂直中心相差不超过这个像素数
const SAME_LINE_TOLERANCE: f32 = 15.0;

/// 在文本框集合中查找某个字段的值。
///
/// 先试同框内提取（标签后面就是值），失败则找同一行、位于标签右侧、
/// 且水平距离最近的框。
///
/// 返回 (值文本, 该值所在框的置信度)
fn find_value(
    boxes: &[TextBox],
    labels: &[&str],
    validate: impl Fn(&str) -> bool,
) -> Option<(String, f32)> {
    for label in labels {
        for b in boxes.iter().filter(|b| b.text.contains(label)) {
            // 情况 1：标签和值在同一个框
            if let Some(rest) = b.text.split(label).nth(1) {
                let candidate = rest.trim_start_matches([' ', '：', ':', '\u{3000}']).trim();
                if !candidate.is_empty() && validate(candidate) {
                    return Some((candidate.to_string(), b.confidence));
                }
            }

            // 情况 2：值在同一行的右邻框
            let mut right_neighbors: Vec<&TextBox> = boxes
                .iter()
                .filter(|other| {
                    (other.center_y() - b.center_y()).abs() <= SAME_LINE_TOLERANCE
                        && other.x >= b.right() - 1.0
                        && !std::ptr::eq(*other, b)
                })
                .collect();
            right_neighbors.sort_by(|p, q| p.x.partial_cmp(&q.x).unwrap());

            for n in right_neighbors {
                let candidate = n.text.trim();
                if !candidate.is_empty() && validate(candidate) {
                    return Some((candidate.to_string(), n.confidence));
                }
            }
        }
    }
    None
}

fn looks_like_digits(s: &str) -> bool {
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    digits >= 8
}

fn looks_like_date(s: &str) -> bool {
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    (6..=8).contains(&digits)
}

fn looks_like_amount(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_digit() || "￥¥,. ".contains(c))
}

fn looks_like_rate(s: &str) -> bool {
    s.contains('%') || s.chars().any(|c| c.is_ascii_digit())
}

fn any_text(_s: &str) -> bool {
    true
}

/// 从 OCR 文本框中定位增值税发票字段。
///
/// 两种版式都要支持：
/// - 标签与值在同一个框内（"发票号码 12345"）
/// - 标签与值是相邻的两个框（"发票号码" | "12345"）
pub fn locate_vat_fields(
    boxes: &[TextBox],
    path: &Path,
    level: ParseLevel,
) -> Result<ParsedInvoice, ParseError> {
    let missing = |field: &str| ParseError::MissingField {
        path: path.to_path_buf(),
        field: field.to_string(),
    };

    let (number_raw, c1) = find_value(boxes, &["发票号码", "发票号"], looks_like_digits)
        .ok_or_else(|| missing("invoice_number"))?;
    let (date_raw, c2) =
        find_value(boxes, &["开票日期"], looks_like_date).ok_or_else(|| missing("issue_date"))?;
    let (amount_raw, c3) = find_value(
        boxes,
        &["价税合计", "合计金额", "小写"],
        looks_like_amount,
    )
    .ok_or_else(|| missing("total_amount"))?;

    let tax = find_value(boxes, &["税额"], looks_like_amount);
    let rate = find_value(boxes, &["税率"], looks_like_rate);
    let buyer = find_value(boxes, &["购买方名称", "购买方"], any_text);
    let seller = find_value(boxes, &["销售方名称", "销售方"], any_text);

    // 整张票的置信度取所有实际采用的框的最小值——
    // 一个字段错了，整张票就不能直接用
    let mut confidences = vec![c1, c2, c3];
    confidences.extend([&tax, &rate].iter().filter_map(|o| o.as_ref().map(|(_, c)| *c)));
    let confidence = confidences.iter().copied().fold(f32::INFINITY, f32::min);

    Ok(ParsedInvoice {
        invoice_number: number_raw.chars().filter(|c| c.is_ascii_digit()).collect(),
        issue_date: parse_date(&date_raw)?,
        total_amount: parse_amount(&amount_raw, "total_amount")?,
        tax_amount: tax
            .map(|(raw, _)| parse_amount(&raw, "tax_amount"))
            .transpose()?,
        tax_rate: rate.map(|(raw, _)| parse_tax_rate(&raw)).transpose()?,
        buyer_name: buyer.map(|(raw, _)| raw),
        seller_name: seller.map(|(raw, _)| raw),
        ticket_type: TicketType::Other,
        parse_level: level,
        confidence,
        source_path: path.to_path_buf(),
    })
}

/// 把同一行内水平相邻的碎片框合并成一个。
///
/// 有些 OFD 把一个词拆成多个 TextObject（"发"/"票号"/"码："），
/// 不合并就找不到 "发票号码" 这个标签。
/// `max_gap` 是允许的最大水平间隙（像素）：小于它就认为属于同一串文本。
pub fn merge_line_fragments(mut boxes: Vec<TextBox>, max_gap: f32) -> Vec<TextBox> {
    if boxes.is_empty() {
        return boxes;
    }
    // 先按行（y 中心）再按 x 排序
    boxes.sort_by(|a, b| {
        a.center_y()
            .partial_cmp(&b.center_y())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut out: Vec<TextBox> = Vec::with_capacity(boxes.len());
    for b in boxes {
        match out.last_mut() {
            Some(prev)
                if (prev.center_y() - b.center_y()).abs() <= SAME_LINE_TOLERANCE
                    && b.x - prev.right() <= max_gap
                    && b.x >= prev.x =>
            {
                // 同一行且紧邻：拼接文本，扩展边框
                prev.text.push_str(&b.text);
                let right = prev.right().max(b.right());
                prev.width = right - prev.x;
                prev.height = prev.height.max(b.height);
                prev.confidence = prev.confidence.min(b.confidence);
            }
            _ => out.push(b),
        }
    }
    out
}

/// 通过 Python sidecar 进行 OCR 识别。
///
/// 调用 tools/ocr_sidecar.py，返回文本框数组。
pub fn recognize_via_sidecar(image_path: &Path) -> anyhow::Result<Vec<TextBox>> {
    let project_root = std::env::current_dir()?;
    let sidecar_path = project_root.join("tools/ocr_sidecar.py");

    let output = std::process::Command::new("python3")
        .arg(sidecar_path)
        .arg(image_path)
        .output()?;

    if !output.status.success() {
        anyhow::bail!(
            "OCR sidecar failed with exit code: {:?}",
            output.status.code()
        );
    }

    let json = String::from_utf8(output.stdout)?;
    let boxes: Vec<TextBox> = serde_json::from_str(&json)?;
    Ok(boxes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromStr;
    use rust_decimal::Decimal;

    fn tb(text: &str, x: f32, y: f32, conf: f32) -> TextBox {
        TextBox {
            text: text.to_string(),
            x,
            y,
            width: text.chars().count() as f32 * 12.0,
            height: 20.0,
            confidence: conf,
        }
    }

    /// 版式 A：标签和值在同一个框
    fn inline_layout() -> Vec<TextBox> {
        vec![
            tb("发票号码 24312000000012345678", 400.0, 40.0, 0.97),
            tb("开票日期 2026年07月03日", 400.0, 70.0, 0.95),
            tb("价税合计 ￥1280.00", 400.0, 300.0, 0.96),
            tb("税额 ￥72.45", 500.0, 260.0, 0.93),
            tb("税率 6%", 400.0, 260.0, 0.94),
        ]
    }

    /// 版式 B：标签和值是相邻的两个框（同一行）
    fn adjacent_layout() -> Vec<TextBox> {
        vec![
            tb("发票号码", 400.0, 40.0, 0.97),
            tb("24312000000012345678", 520.0, 42.0, 0.96),
            tb("开票日期", 400.0, 70.0, 0.95),
            tb("2026-07-03", 520.0, 71.0, 0.94),
            tb("价税合计", 400.0, 300.0, 0.96),
            tb("￥1280.00", 520.0, 301.0, 0.92),
        ]
    }

    #[test]
    fn locates_fields_in_inline_layout() {
        let invoice = locate_vat_fields(&inline_layout(), Path::new("a.jpg"), ParseLevel::L2).unwrap();
        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
        assert_eq!(invoice.total_amount, Decimal::from_str("1280.00").unwrap());
        assert_eq!(invoice.tax_amount, Some(Decimal::from_str("72.45").unwrap()));
        assert_eq!(invoice.tax_rate, Some(Decimal::from_str("0.06").unwrap()));
        assert_eq!(invoice.parse_level, ParseLevel::L2);
    }

    #[test]
    fn locates_fields_in_adjacent_layout() {
        let invoice = locate_vat_fields(&adjacent_layout(), Path::new("b.jpg"), ParseLevel::L2).unwrap();
        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
        assert_eq!(invoice.total_amount, Decimal::from_str("1280.00").unwrap());
    }

    #[test]
    fn confidence_is_minimum_across_used_boxes() {
        // 整张票的可信度由最弱的字段决定——一个字段错了整张就不能用
        let invoice = locate_vat_fields(&inline_layout(), Path::new("a.jpg"), ParseLevel::L2).unwrap();
        assert!(
            (invoice.confidence - 0.93).abs() < 0.001,
            "应取最低置信度 0.93，实际 {}",
            invoice.confidence
        );
    }

    #[test]
    fn ignores_label_box_on_a_different_line() {
        // "价税合计" 在 y=300，候选值在 y=500（不同行），不应被采用
        let boxes = vec![
            tb("发票号码", 400.0, 40.0, 0.9),
            tb("12345678901234567890", 520.0, 42.0, 0.9),
            tb("开票日期", 400.0, 70.0, 0.9),
            tb("2026-07-03", 520.0, 71.0, 0.9),
            tb("价税合计", 400.0, 300.0, 0.9),
            tb("￥999.00", 520.0, 500.0, 0.9),
        ];
        let err = locate_vat_fields(&boxes, Path::new("c.jpg"), ParseLevel::L2).unwrap_err();
        assert!(err.to_string().contains("total_amount"), "实际: {err}");
    }

    #[test]
    fn missing_invoice_number_reports_field() {
        let boxes = vec![
            tb("开票日期 2026-07-03", 400.0, 70.0, 0.9),
            tb("价税合计 ￥100.00", 400.0, 300.0, 0.9),
        ];
        let err = locate_vat_fields(&boxes, Path::new("d.jpg"), ParseLevel::L2).unwrap_err();
        assert!(err.to_string().contains("invoice_number"), "实际: {err}");
    }
}
