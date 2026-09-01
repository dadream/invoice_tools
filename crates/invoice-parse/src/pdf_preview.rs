//! Passive PDF text-layout fallback for the original viewer.
//!
//! Windows page rasterization remains the preferred preview.  This module is
//! used only when that renderer cannot open a PDF which still has a readable
//! text layer.

use euclid::Transform2D;
use pdf_extract::{MediaBox, OutputDev, OutputError};
use serde::Serialize;
use std::path::Path;

use crate::model::ParseError;

type Transform = Transform2D<f64, pdf_extract::Space, pdf_extract::Space>;
const MAX_TEXTS_PER_PAGE: usize = 30_000;
const MAX_TEXT_CHARS: usize = 4_096;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PdfPreviewText {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PdfPreviewPage {
    pub page: u32,
    pub width: f32,
    pub height: f32,
    pub texts: Vec<PdfPreviewText>,
}

struct Collector {
    pages: Vec<PdfPreviewPage>,
    current_page: Option<PdfPreviewPage>,
    flip_ctm: Transform,
    current_word: String,
    word_start_x: f64,
    word_start_y: f64,
    word_height: f64,
    word_width: f64,
    first_char: bool,
}

impl Collector {
    fn new() -> Self {
        Self {
            pages: Vec::new(),
            current_page: None,
            flip_ctm: Transform::identity(),
            current_word: String::new(),
            word_start_x: 0.0,
            word_start_y: 0.0,
            word_height: 0.0,
            word_width: 0.0,
            first_char: true,
        }
    }

    fn finish_word(&mut self) {
        let value = self.current_word.trim();
        if !value.is_empty() {
            if let Some(page) = self.current_page.as_mut() {
                if page.texts.len() < MAX_TEXTS_PER_PAGE {
                    page.texts.push(PdfPreviewText {
                        text: value.chars().take(MAX_TEXT_CHARS).collect(),
                        x: finite(self.word_start_x),
                        y: finite(self.word_start_y),
                        width: finite(self.word_width),
                        height: finite(self.word_height),
                    });
                }
            }
        }
        self.current_word.clear();
        self.first_char = true;
        self.word_height = 0.0;
        self.word_width = 0.0;
    }

    fn finish_page(&mut self) {
        self.finish_word();
        if let Some(page) = self.current_page.take() {
            self.pages.push(page);
        }
    }
}

impl OutputDev for Collector {
    fn begin_page(
        &mut self,
        page_num: u32,
        media_box: &MediaBox,
        _art_box: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), OutputError> {
        self.finish_page();
        let width = (media_box.urx - media_box.llx).abs();
        let height = (media_box.ury - media_box.lly).abs();
        self.flip_ctm = Transform::row_major(1.0, 0.0, 0.0, -1.0, 0.0, height);
        self.current_page = Some(PdfPreviewPage {
            page: page_num,
            width: finite(width).max(1.0),
            height: finite(height).max(1.0),
            texts: Vec::new(),
        });
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), OutputError> {
        self.finish_page();
        Ok(())
    }

    fn output_character(
        &mut self,
        trm: &Transform,
        width: f64,
        _spacing: f64,
        font_size: f64,
        character: &str,
    ) -> Result<(), OutputError> {
        let position = trm.post_transform(&self.flip_ctm);
        let transformed = trm.transform_vector(euclid::vec2(font_size, font_size));
        let height = (transformed.x.abs() * transformed.y.abs()).sqrt().max(1.0);
        if self.first_char {
            self.word_start_x = position.m31;
            self.word_start_y = position.m32;
            self.word_height = height;
            self.first_char = false;
        } else {
            self.word_height = self.word_height.max(height);
        }
        if self.current_word.chars().count() < MAX_TEXT_CHARS {
            self.current_word.push_str(character);
        }
        self.word_width = (position.m31 + width.abs() * height - self.word_start_x).abs();
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
        self.finish_word();
        Ok(())
    }
}

pub fn render_text_preview_page(
    bytes: &[u8],
    path: &Path,
    requested_page: u32,
) -> Result<PdfPreviewPage, ParseError> {
    if requested_page == 0 {
        return Err(malformed(path, "PDF 页码必须从 1 开始"));
    }
    let result = std::panic::catch_unwind(|| {
        let document = pdf_extract::Document::load_mem(bytes)
            .map_err(|_| malformed(path, "PDF 文本版式无法读取"))?;
        let mut collector = Collector::new();
        pdf_extract::output_doc_page(&document, &mut collector, requested_page)
            .map_err(|_| malformed(path, "PDF 文本版式提取失败"))?;
        collector.finish_page();
        let page = collector
            .pages
            .into_iter()
            .next()
            .ok_or_else(|| malformed(path, "PDF 页码超出范围"))?;
        if page.texts.is_empty() {
            return Err(malformed(path, "PDF 页面没有可显示的文本层"));
        }
        Ok(page)
    });
    match result {
        Ok(value) => value,
        Err(_) => Err(malformed(path, "PDF 文本版式处理异常")),
    }
}

fn malformed(path: &Path, detail: &str) -> ParseError {
    ParseError::MalformedFormat {
        path: path.to_path_buf(),
        format: "PDF",
        detail: detail.to_string(),
    }
}

fn finite(value: f64) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 20_000.0) as f32
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};
    use std::io::Cursor;

    fn text_pdf() -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 16.into()]),
                Operation::new("Td", vec![40.into(), 760.into()]),
                Operation::new("Tj", vec![Object::string_literal("Invoice 126.00")]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id =
            document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id =
            document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        document.trailer.set("Root", catalog_id);
        let mut output = Cursor::new(Vec::new());
        document.save_to(&mut output).unwrap();
        output.into_inner()
    }

    #[test]
    fn rejects_invalid_pdf_without_panicking() {
        assert!(render_text_preview_page(b"not pdf", Path::new("bad.pdf"), 1).is_err());
    }

    #[test]
    fn rejects_zero_page() {
        assert!(render_text_preview_page(b"%PDF", Path::new("bad.pdf"), 0).is_err());
    }

    #[test]
    fn extracts_only_the_requested_one_based_page() {
        let page = render_text_preview_page(&text_pdf(), Path::new("invoice.pdf"), 1).unwrap();
        assert_eq!(page.page, 1);
        assert_eq!(page.width, 595.0);
        assert!(page.texts.iter().any(|item| item.text.contains("126.00")));
    }
}
