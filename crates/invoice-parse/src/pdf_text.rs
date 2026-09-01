//! 从 PDF 中提取带坐标的文本框。
//!
//! 使用 pdf_extract::OutputDev trait 自定义输出器，
//! 收集每个文本块的位置与内容，供字段定位算法使用。

use crate::model::{ParseError, ParseLevel, ParsedInvoice};
use crate::ocr::{locate_vat_fields, merge_line_fragments, TextBox};
use euclid::Transform2D;
use pdf_extract::{MediaBox, OutputDev, OutputError};
use std::path::Path;

// Use pdf_extract's Transform type directly
type Transform = Transform2D<f64, pdf_extract::Space, pdf_extract::Space>;

/// 自定义 PDF 输出器，收集文本框。
struct TextBoxCollector {
    boxes: Vec<TextBox>,
    flip_ctm: Transform,
    current_word: String,
    word_start_x: f64,
    word_start_y: f64,
    word_max_height: f64,
    word_max_width: f64,
    first_char_in_word: bool,
}

impl TextBoxCollector {
    fn new() -> Self {
        Self {
            boxes: Vec::new(),
            flip_ctm: Transform::identity(),
            current_word: String::new(),
            word_start_x: 0.0,
            word_start_y: 0.0,
            word_max_height: 0.0,
            word_max_width: 0.0,
            first_char_in_word: true,
        }
    }

    fn finish_word(&mut self) {
        if !self.current_word.is_empty() {
            self.boxes.push(TextBox {
                text: std::mem::take(&mut self.current_word),
                x: self.word_start_x as f32,
                y: self.word_start_y as f32,
                width: self.word_max_width as f32,
                height: self.word_max_height as f32,
                confidence: 1.0, // PDF text layer is deterministic
            });
        }
        self.first_char_in_word = true;
        self.word_max_height = 0.0;
        self.word_max_width = 0.0;
    }
}

impl OutputDev for TextBoxCollector {
    fn begin_page(
        &mut self,
        _page_num: u32,
        media_box: &MediaBox,
        _art_box: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), OutputError> {
        // Flip Y coordinate so top-left is origin (matching OCR convention)
        self.flip_ctm =
            Transform::row_major(1.0, 0.0, 0.0, -1.0, 0.0, media_box.ury - media_box.lly);
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), OutputError> {
        self.finish_word();
        Ok(())
    }

    fn output_character(
        &mut self,
        trm: &Transform,
        width: f64,
        _spacing: f64,
        font_size: f64,
        char: &str,
    ) -> Result<(), OutputError> {
        let position = trm.post_transform(&self.flip_ctm);
        let (x, y) = (position.m31, position.m32);

        // Calculate transformed font size
        let transformed_font_size_vec = trm.transform_vector(euclid::vec2(font_size, font_size));
        let transformed_font_size =
            (transformed_font_size_vec.x * transformed_font_size_vec.y).sqrt();

        if self.first_char_in_word {
            self.word_start_x = x;
            self.word_start_y = y;
            self.word_max_height = transformed_font_size;
            self.first_char_in_word = false;
        } else {
            // Extend bounding box
            self.word_max_height = self.word_max_height.max(transformed_font_size);
        }

        self.current_word.push_str(char);
        self.word_max_width = (x + width * transformed_font_size) - self.word_start_x;

        Ok(())
    }

    fn begin_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_word(&mut self) -> Result<(), OutputError> {
        self.finish_word();
        Ok(())
    }

    fn end_line(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
}

/// 从 PDF 字节流中提取文本框（带坐标）。
///
/// 使用 catch_unwind 包裹 pdf_extract 调用，防止 PDF 解析库的 panic 传播。
pub fn extract_text_boxes(pdf_bytes: &[u8], path: &Path) -> Result<Vec<TextBox>, ParseError> {
    // pdf_extract 在遇到某些畸形 PDF 时会 panic，用 catch_unwind 包裹
    let result = std::panic::catch_unwind(|| {
        let doc = pdf_extract::Document::load_mem(pdf_bytes).map_err(|e| {
            ParseError::MalformedFormat {
                path: path.to_path_buf(),
                format: "PDF",
                detail: format!("无法加载 PDF 文档: {e}"),
            }
        })?;

        let mut collector = TextBoxCollector::new();
        pdf_extract::output_doc(&doc, &mut collector).map_err(|e| ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format: "PDF",
            detail: format!("提取文本框失败: {e}"),
        })?;

        Ok(collector.boxes)
    });

    match result {
        Ok(r) => r,
        Err(panic_info) => {
            let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                s.clone()
            } else {
                "未知 panic".to_string()
            };
            Err(ParseError::MalformedFormat {
                path: path.to_path_buf(),
                format: "PDF",
                detail: format!("PDF 解析库 panic: {msg}"),
            })
        }
    }
}

/// 从 PDF 文本框解析增值税发票（L1）。
///
/// 此函数复用 ocr::locate_vat_fields 的字段定位算法，
/// 但输入是 PDF 自带的文本层（L1），而不是 OCR 识别结果（L2）。
pub fn parse_vat_invoice_from_boxes(
    pdf_bytes: &[u8],
    path: &Path,
) -> Result<ParsedInvoice, ParseError> {
    let boxes = extract_text_boxes(pdf_bytes, path)?;

    if boxes.is_empty() {
        return Err(ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format: "PDF",
            detail: "PDF 无可提取的文本层（可能是纯扫描件）".to_string(),
        });
    }

    // 合并同行碎片（某些 PDF 把一个词拆成多个字符框）
    // 增大间隙阈值以应对字符间距较大的 PDF
    let merged = merge_line_fragments(boxes, 12.0);

    locate_vat_fields(&merged, path, ParseLevel::L1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_boxes_from_real_pdf() {
        // Use a known sample with text layer
        let sample_path = std::path::PathBuf::from("fixtures/samples/05-unknown-b4511bc3.pdf");
        if !sample_path.exists() {
            eprintln!("跳过测试：样本不存在");
            return;
        }

        let bytes = std::fs::read(&sample_path).unwrap();
        let boxes = extract_text_boxes(&bytes, &sample_path).unwrap();

        // Should extract some text boxes
        assert!(!boxes.is_empty(), "应提取到文本框");

        // Should find invoice number in boxes
        let has_invoice_number = boxes
            .iter()
            .any(|b| b.text.contains("26112000002267104336"));
        assert!(has_invoice_number, "应包含发票号码");
    }

    #[test]
    fn panic_in_pdf_extract_becomes_error() {
        // Empty bytes should cause pdf_extract to fail
        let result = extract_text_boxes(b"not a pdf", Path::new("test.pdf"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::MalformedFormat { .. }));
    }

    #[test]
    fn empty_pdf_returns_error() {
        // Minimal PDF with no text
        let minimal_pdf = b"%PDF-1.4\n%%EOF";
        let result = parse_vat_invoice_from_boxes(minimal_pdf, Path::new("empty.pdf"));
        assert!(result.is_err());
    }
}
