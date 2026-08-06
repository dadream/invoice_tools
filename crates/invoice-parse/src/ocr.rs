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
            // 允许轻微重叠（-5.0）以应对 PDF 文本框宽度计算的不精确
            let mut right_neighbors: Vec<&TextBox> = boxes
                .iter()
                .filter(|other| {
                    (other.center_y() - b.center_y()).abs() <= SAME_LINE_TOLERANCE
                        && other.x >= b.right() - 5.0
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
    // 支持日期（6-8位）或日期+时间（14位左右）
    (6..=8).contains(&digits) || (12..=20).contains(&digits)
}

fn looks_like_amount(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_digit() || "￥¥,. 元".contains(c))
}

/// 从包含中文大写金额的混合文本中提取阿拉伯数字金额。
///
/// 例如 "壹拾伍圆整 ¥15.00" → Some("15.00")
///      "（小写）¥15.00"   → Some("15.00")
///      "（小写）143.40¥"  → Some("143.40")
///      "合计20.98元"      → Some("20.98")
fn extract_amount_from_mixed(text: &str) -> Option<String> {
    // 策略 1：找最后一个 ¥ 或 ￥，优先取其后的数字串，如果后面为空则取前面的
    if let Some((pos, ch)) = text.char_indices().rfind(|(_, c)| *c == '¥' || *c == '￥') {
        let after = text[pos + ch.len_utf8()..].trim();
        let num_after: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
            .filter(|c| *c != ',')
            .collect();
        if !num_after.is_empty() {
            return Some(num_after);
        }

        // ¥ 后面为空，尝试取前面的数字（逆序扫描到第一个非数字字符）
        let before = text[..pos].trim();
        let num_before: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
            .filter(|c| *c != ',')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if !num_before.is_empty() {
            return Some(num_before);
        }
    }

    // 策略 2：找「元」，取其前面的数字（逆序扫描到第一个非数字字符）
    if let Some(pos) = text.rfind('元') {
        let before = text[..pos].trim();
        let num: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
            .filter(|c| *c != ',')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if !num.is_empty() {
            return Some(num);
        }
    }

    None
}

/// 在文本框集合中查找金额字段，支持三级降级：
/// 1. 标准逻辑（同框或同行右邻，纯数字/符号）
/// 2. 同框/同行右邻混合文本中提取（"壹拾伍圆整 ¥15.00" → "15.00"）
/// 3. 跨行：label 下方 NEXT_LINE_TOLERANCE 像素内的相邻框
fn find_amount_value(boxes: &[TextBox], labels: &[&str]) -> Option<(String, f32)> {
    const NEXT_LINE_TOLERANCE: f32 = 30.0;

    // 级别 1：标准逻辑
    if let Some(result) = find_value(boxes, labels, looks_like_amount) {
        return Some(result);
    }

    // 级别 2 & 3：逐标签尝试混合文本提取和跨行搜索
    for label in labels {
        for b in boxes.iter().filter(|b| b.text.contains(label)) {
            // 2a：同框混合（"（小写）¥15.00" 整体在一个框）
            if let Some(amount) = extract_amount_from_mixed(&b.text) {
                return Some((amount, b.confidence));
            }

            // 2b：同行右侧框包含混合文本
            let mut right_neighbors: Vec<&TextBox> = boxes
                .iter()
                .filter(|other| {
                    (other.center_y() - b.center_y()).abs() <= SAME_LINE_TOLERANCE
                        && other.x >= b.right() - 5.0
                        && !std::ptr::eq(*other, b)
                })
                .collect();
            right_neighbors.sort_by(|p, q| p.x.partial_cmp(&q.x).unwrap());
            for n in &right_neighbors {
                if let Some(amount) = extract_amount_from_mixed(&n.text) {
                    return Some((amount, n.confidence));
                }
            }

            // 3：跨行——在 label 下方 NEXT_LINE_TOLERANCE px 内搜索
            let label_bottom = b.y + b.height;
            let mut below: Vec<&TextBox> = boxes
                .iter()
                .filter(|other| {
                    other.y >= label_bottom - 5.0
                        && other.y <= label_bottom + NEXT_LINE_TOLERANCE
                        && !std::ptr::eq(*other, b)
                })
                .collect();
            below.sort_by(|p, q| {
                p.y.partial_cmp(&q.y)
                    .unwrap()
                    .then(p.x.partial_cmp(&q.x).unwrap())
            });
            for n in &below {
                let t = n.text.trim();
                if looks_like_amount(t) {
                    return Some((t.to_string(), n.confidence));
                }
                if let Some(amount) = extract_amount_from_mixed(t) {
                    return Some((amount, n.confidence));
                }
            }
        }
    }
    None
}

fn looks_like_rate(s: &str) -> bool {
    s.contains('%') || s.chars().any(|c| c.is_ascii_digit())
}

fn any_text(_s: &str) -> bool {
    true
}

/// 在指定列中查找数值。
///
/// 增值税发票是表格版式：「税额」「金额」等字段是列标题，
/// 值在该列下方的若干行中。本函数先找列标题框，
/// 再在标题框的 x 坐标±容差范围内、y > 标题底部的框中找第一个满足校验的值。
///
/// 返回 (值文本, 置信度)
fn find_in_column(
    boxes: &[TextBox],
    column_labels: &[&str],
    validate: impl Fn(&str) -> bool,
    x_tolerance: f32,
) -> Option<(String, f32)> {
    for label in column_labels {
        for header in boxes.iter().filter(|b| b.text.contains(label)) {
            // 跳过宽度过大的表头（可能是多列合并，如「税率/征收率税 额」）
            // 正常列标题宽度不超过 80px（「税额」「金额」等 2-4 字）
            if header.width > 80.0 {
                continue;
            }
            let col_x = header.x;
            let col_bottom = header.y + header.height;

            // 在该列（x 坐标±容差）下方找第一个满足校验的框
            let mut candidates: Vec<&TextBox> = boxes
                .iter()
                .filter(|b| {
                    b.y > col_bottom
                        && (b.x - col_x).abs() <= x_tolerance
                        && !std::ptr::eq(*b, header)
                })
                .collect();
            candidates.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap());

            for c in candidates {
                let text = c.text.trim();
                if !text.is_empty() && validate(text) {
                    return Some((text.to_string(), c.confidence));
                }
            }
        }
    }
    None
}

/// 在左右两列区块中查找「名称：」字段。
///
/// 增值税发票的购销方信息是左右分栏：买方在左侧（x < 中线），
/// 卖方在右侧（x > 中线）。每个区块内都有「名称：」标签，
/// 本函数先按 x 坐标判定买方/卖方区块，再在对应区块内找「名称：」右侧的值。
///
/// 返回 (buyer_name, buyer_conf, seller_name, seller_conf)
fn find_buyer_seller_names(
    boxes: &[TextBox],
) -> (Option<(String, f32)>, Option<(String, f32)>) {
    const MID_X: f32 = 250.0; // 增值税发票标准版式的左右分栏中线
    const NAME_LABEL: &str = "名称：";

    let mut buyer = None;
    let mut seller = None;

    for label_box in boxes.iter().filter(|b| b.text.contains(NAME_LABEL)) {
        // 判定区块：x < 中线 → 买方，x >= 中线 → 卖方
        let is_buyer_side = label_box.x < MID_X;

        // 在同一行右侧**且在同一个区块内**找值
        let mut right_neighbors: Vec<&TextBox> = boxes
            .iter()
            .filter(|other| {
                (other.center_y() - label_box.center_y()).abs() <= SAME_LINE_TOLERANCE
                    && other.x >= label_box.right() - 5.0
                    && !std::ptr::eq(*other, label_box)
                    // 关键约束：右侧值必须在同一个区块内
                    && (is_buyer_side == (other.x < MID_X))
            })
            .collect();
        right_neighbors.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap());

        if let Some(value_box) = right_neighbors.first() {
            let name = value_box.text.trim().to_string();
            if !name.is_empty() {
                let result = Some((name, value_box.confidence));
                if is_buyer_side {
                    buyer = result;
                } else {
                    seller = result;
                }
            }
        } else {
            eprintln!("  → 无候选");
        }
    }

    (buyer, seller)
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
        find_value(boxes, &["开票日期", "开具时间"], looks_like_date).ok_or_else(|| missing("issue_date"))?;
    let (amount_raw, c3) =
        find_amount_value(boxes, &["价税合计", "合计金额", "小写", "合计"])
            .ok_or_else(|| missing("total_amount"))?;

    // 税额：优先行内定位，表格列版式作为兜底
    // 列定位器会跳过宽度 > 80px 的合并表头（如「税率/征收率税 额」）
    let tax = find_value(boxes, &["税额", "税  额"], looks_like_amount)
        .or_else(|| find_in_column(boxes, &["税额", "税 额", "税  额"], looks_like_amount, 20.0));
    let rate = find_value(boxes, &["税率"], looks_like_rate);

    // 购销方名称：先试表格列版式（左右分栏 + 「名称：」），
    // 失败降级到行内版式（完整标签「购买方名称」「销售方名称」）
    let (buyer_col, seller_col) = find_buyer_seller_names(boxes);
    let buyer = buyer_col.or_else(|| find_value(boxes, &["购买方名称", "购买方"], any_text));
    let seller = seller_col.or_else(|| find_value(boxes, &["销售方名称", "销售方"], any_text));

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
    fn extracts_amount_from_chinese_uppercase_mixed_box() {
        // 同行右侧框 "壹拾伍圆整 ¥15.00"：中文大写 + 阿拉伯金额混合
        let boxes = vec![
            tb("发票号码", 400.0, 20.0, 0.97),
            tb("24312000000012345678", 520.0, 20.0, 0.96),
            tb("开票日期", 400.0, 40.0, 0.95),
            tb("2026-06-08", 520.0, 40.0, 0.94),
            tb("价税合计（大写）", 50.0, 200.0, 0.96),
            tb("壹拾伍圆整 ¥15.00", 250.0, 200.0, 0.93),
        ];
        let invoice =
            locate_vat_fields(&boxes, Path::new("test.pdf"), ParseLevel::L1).unwrap();
        assert_eq!(invoice.total_amount, Decimal::from_str("15.00").unwrap());
    }

    #[test]
    fn extracts_amount_from_next_line() {
        // 标签行无值，金额框在下方一行（y 差 15px < NEXT_LINE_TOLERANCE 30px）
        let boxes = vec![
            tb("发票号码", 400.0, 20.0, 0.97),
            tb("24312000000012345678", 520.0, 20.0, 0.96),
            tb("开票日期", 400.0, 40.0, 0.95),
            tb("2026-06-08", 520.0, 40.0, 0.94),
            // label: height=20 → bottom=220；value y=225 在 [215, 250] 范围内
            tb("（小写）", 50.0, 200.0, 0.96),
            tb("¥15.00", 120.0, 225.0, 0.93),
        ];
        let invoice =
            locate_vat_fields(&boxes, Path::new("test.pdf"), ParseLevel::L1).unwrap();
        assert_eq!(invoice.total_amount, Decimal::from_str("15.00").unwrap());
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
