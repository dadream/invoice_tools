//! Passive OFD layout preview.
//!
//! OFD is a ZIP container.  The preview path deliberately reads only page
//! geometry and text objects from XML and never executes embedded content.

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Serialize;
use std::io::{Cursor, Read};
use std::path::Path;

use crate::model::ParseError;

const DEFAULT_PAGE_WIDTH_MM: f32 = 210.0;
const DEFAULT_PAGE_HEIGHT_MM: f32 = 297.0;
const MAX_PAGES: usize = 100;
const MAX_TEXT_OBJECTS_PER_PAGE: usize = 20_000;
const MAX_TEXT_CHARS: usize = 4_096;
const MAX_XML_ENTRY_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OfdPreviewText {
    pub text: String,
    pub x_mm: f32,
    pub y_mm: f32,
    pub width_mm: f32,
    pub height_mm: f32,
    pub font_size_mm: f32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OfdPreviewPage {
    pub page: u32,
    pub width_mm: f32,
    pub height_mm: f32,
    pub texts: Vec<OfdPreviewText>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OfdPreviewDocument {
    pub pages: Vec<OfdPreviewPage>,
}

pub fn preview_page_count(bytes: &[u8], path: &Path) -> Result<u32, ParseError> {
    let mut archive = open_archive(bytes, path)?;
    let count = page_content_names(&mut archive)?.len();
    if count == 0 || count > MAX_PAGES {
        return Err(malformed(path, "OFD 页数为空或超过安全预览上限"));
    }
    u32::try_from(count).map_err(|_| malformed(path, "OFD 页数无法表示"))
}

pub fn render_preview_page(
    bytes: &[u8],
    path: &Path,
    requested_page: u32,
) -> Result<OfdPreviewPage, ParseError> {
    if requested_page == 0 {
        return Err(malformed(path, "OFD 页码必须从 1 开始"));
    }
    let mut archive = open_archive(bytes, path)?;
    let page_names = page_content_names(&mut archive)?;
    if page_names.is_empty() || page_names.len() > MAX_PAGES {
        return Err(malformed(path, "OFD 页数为空或超过安全预览上限"));
    }
    let index = usize::try_from(requested_page - 1).map_err(|_| malformed(path, "OFD 页码无效"))?;
    let name = page_names
        .get(index)
        .ok_or_else(|| malformed(path, "OFD 页码超出范围"))?;
    let physical_box =
        read_physical_box(&mut archive).unwrap_or((DEFAULT_PAGE_WIDTH_MM, DEFAULT_PAGE_HEIGHT_MM));
    let raw = read_entry(&mut archive, name, path)?;
    parse_page_xml(&raw, path, requested_page, physical_box)
}

fn open_archive(bytes: &[u8], path: &Path) -> Result<zip::ZipArchive<Cursor<Vec<u8>>>, ParseError> {
    zip::ZipArchive::new(Cursor::new(bytes.to_vec()))
        .map_err(|_| malformed(path, "不是有效的 OFD ZIP 容器"))
}

fn malformed(path: &Path, detail: &str) -> ParseError {
    ParseError::MalformedFormat {
        path: path.to_path_buf(),
        format: "OFD",
        detail: detail.to_string(),
    }
}

fn page_content_names(
    archive: &mut zip::ZipArchive<Cursor<Vec<u8>>>,
) -> Result<Vec<String>, ParseError> {
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|_| malformed(Path::new(""), "读取 OFD 条目失败"))?;
        let name = entry.name().replace('\\', "/");
        let lower = name.to_ascii_lowercase();
        if lower.ends_with("content.xml") && lower.contains("/pages/") {
            names.push(name);
        }
    }
    names.sort_by_key(|name| page_sort_key(name));
    names.dedup();
    Ok(names)
}

fn page_sort_key(name: &str) -> (u32, String) {
    let lower = name.to_ascii_lowercase();
    let marker = "/page_";
    let number = lower
        .find(marker)
        .and_then(|start| {
            let tail = &lower[start + marker.len()..];
            let digits = tail
                .chars()
                .take_while(|value| value.is_ascii_digit())
                .collect::<String>();
            digits.parse::<u32>().ok()
        })
        .unwrap_or(u32::MAX);
    (number, lower)
}

fn read_entry(
    archive: &mut zip::ZipArchive<Cursor<Vec<u8>>>,
    name: &str,
    path: &Path,
) -> Result<Vec<u8>, ParseError> {
    let mut entry = archive
        .by_name(name)
        .map_err(|_| malformed(path, "OFD 页面条目不存在"))?;
    if entry.size() > MAX_XML_ENTRY_BYTES {
        return Err(malformed(path, "OFD 页面版式超过安全预览上限"));
    }
    let mut raw = Vec::with_capacity(entry.size().min(4 * 1024 * 1024) as usize);
    entry
        .by_ref()
        .take(MAX_XML_ENTRY_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|error| ParseError::Io {
            path: path.to_path_buf(),
            source: error,
        })?;
    if raw.len() as u64 > MAX_XML_ENTRY_BYTES {
        return Err(malformed(path, "OFD 页面版式超过安全预览上限"));
    }
    Ok(raw)
}

fn read_physical_box(archive: &mut zip::ZipArchive<Cursor<Vec<u8>>>) -> Option<(f32, f32)> {
    let mut document_names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).ok()?;
        let name = entry.name().replace('\\', "/");
        if name.to_ascii_lowercase().ends_with("/document.xml") {
            document_names.push(name);
        }
    }
    for name in document_names {
        let raw = read_entry(archive, &name, Path::new("")).ok()?;
        let mut reader = Reader::from_reader(raw.as_slice());
        reader.config_mut().trim_text(true);
        let mut buffer = Vec::new();
        let mut in_physical_box = false;
        loop {
            match reader.read_event_into(&mut buffer).ok()? {
                Event::Start(event) => {
                    in_physical_box = local_tag(event.name().as_ref()) == "PhysicalBox";
                }
                Event::Text(event) if in_physical_box => {
                    let value = event.unescape().ok()?;
                    let numbers = value
                        .split_whitespace()
                        .filter_map(|part| part.parse::<f32>().ok())
                        .collect::<Vec<_>>();
                    if numbers.len() == 4 && numbers[2] > 0.0 && numbers[3] > 0.0 {
                        return Some((numbers[2].min(1_000.0), numbers[3].min(1_000.0)));
                    }
                }
                Event::End(event) if local_tag(event.name().as_ref()) == "PhysicalBox" => {
                    in_physical_box = false;
                }
                Event::Eof => break,
                _ => {}
            }
            buffer.clear();
        }
    }
    None
}

fn parse_page_xml(
    xml: &[u8],
    path: &Path,
    page: u32,
    physical_box: (f32, f32),
) -> Result<OfdPreviewPage, ParseError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut pending: Option<(f32, f32, f32, f32, f32)> = None;
    let mut in_text_code = false;
    let mut text = String::new();
    let mut texts = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = local_tag(event.name().as_ref());
                if name == "TextObject" {
                    let mut boundary = None;
                    let mut font_size = None;
                    for attribute in event.attributes().flatten() {
                        match local_tag(attribute.key.as_ref()).as_str() {
                            "Boundary" => boundary = parse_boundary(attribute.value.as_ref()),
                            "Size" => {
                                font_size = std::str::from_utf8(attribute.value.as_ref())
                                    .ok()
                                    .and_then(|value| value.parse::<f32>().ok())
                            }
                            _ => {}
                        }
                    }
                    pending = boundary.map(|(x, y, width, height)| {
                        (x, y, width, height, font_size.unwrap_or(height * 0.8))
                    });
                    text.clear();
                } else if name == "TextCode" {
                    in_text_code = true;
                }
            }
            Ok(Event::Text(event)) if in_text_code => {
                if let Ok(value) = event.unescape() {
                    let remaining = MAX_TEXT_CHARS.saturating_sub(text.chars().count());
                    text.extend(value.chars().take(remaining));
                }
            }
            Ok(Event::End(event)) => {
                let name = local_tag(event.name().as_ref());
                if name == "TextCode" {
                    in_text_code = false;
                } else if name == "TextObject" {
                    if let Some((x, y, width, height, font_size)) = pending.take() {
                        let value = text.trim();
                        if !value.is_empty() && texts.len() < MAX_TEXT_OBJECTS_PER_PAGE {
                            texts.push(OfdPreviewText {
                                text: value.to_string(),
                                x_mm: finite_nonnegative(x),
                                y_mm: finite_nonnegative(y),
                                width_mm: finite_nonnegative(width),
                                height_mm: finite_nonnegative(height),
                                font_size_mm: font_size.clamp(1.2, 24.0),
                            });
                        }
                    }
                    text.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return Err(malformed(path, "OFD 页面版式 XML 无法读取")),
            _ => {}
        }
        buffer.clear();
    }

    if texts.is_empty() {
        return Err(malformed(path, "OFD 页面没有可安全显示的文本对象"));
    }
    let inferred_width = texts
        .iter()
        .map(|item| item.x_mm + item.width_mm)
        .fold(0.0_f32, f32::max)
        + 8.0;
    let inferred_height = texts
        .iter()
        .map(|item| item.y_mm + item.height_mm)
        .fold(0.0_f32, f32::max)
        + 8.0;
    Ok(OfdPreviewPage {
        page,
        width_mm: physical_box.0.max(inferred_width).clamp(50.0, 1_000.0),
        height_mm: physical_box.1.max(inferred_height).clamp(50.0, 1_000.0),
        texts,
    })
}

fn finite_nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn local_tag(raw: &[u8]) -> String {
    let value = String::from_utf8_lossy(raw);
    value.rsplit(':').next().unwrap_or(&value).to_string()
}

fn parse_boundary(raw: &[u8]) -> Option<(f32, f32, f32, f32)> {
    let numbers = String::from_utf8_lossy(raw)
        .split_whitespace()
        .filter_map(|part| part.parse::<f32>().ok())
        .collect::<Vec<_>>();
    if numbers.len() == 4 && numbers.iter().all(|value| value.is_finite()) {
        Some((numbers[0], numbers[1], numbers[2], numbers[3]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_ofd(pages: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut bytes));
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            writer.start_file("Doc_0/Document.xml", options).unwrap();
            writer.write_all(br#"<ofd:Document xmlns:ofd="http://www.ofdspec.org/2016"><ofd:CommonData><ofd:PageArea><ofd:PhysicalBox>0 0 210 297</ofd:PhysicalBox></ofd:PageArea></ofd:CommonData></ofd:Document>"#).unwrap();
            for (name, content) in pages {
                writer.start_file(*name, options).unwrap();
                writer.write_all(content.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes
    }

    const PAGE: &str = r#"<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016"><ofd:Content><ofd:Layer><ofd:TextObject Boundary="10 20 60 6" Size="4"><ofd:TextCode>电子发票 126.00</ofd:TextCode></ofd:TextObject></ofd:Layer></ofd:Content></ofd:Page>"#;

    #[test]
    fn renders_passive_text_page() {
        let bytes = make_ofd(&[("Doc_0/Pages/Page_0/Content.xml", PAGE)]);
        let page = render_preview_page(&bytes, Path::new("invoice.ofd"), 1).unwrap();
        assert_eq!(page.page, 1);
        assert_eq!(page.width_mm, 210.0);
        assert!(page.texts[0].text.contains("126.00"));
    }

    #[test]
    fn sorts_page_numbers_naturally() {
        let bytes = make_ofd(&[
            ("Doc_0/Pages/Page_10/Content.xml", PAGE),
            ("Doc_0/Pages/Page_2/Content.xml", PAGE),
        ]);
        assert_eq!(
            preview_page_count(&bytes, Path::new("invoice.ofd")).unwrap(),
            2
        );
        assert!(render_preview_page(&bytes, Path::new("invoice.ofd"), 2).is_ok());
    }

    #[test]
    fn rejects_corrupt_or_out_of_range_input() {
        assert!(preview_page_count(b"not-ofd", Path::new("bad.ofd")).is_err());
        let bytes = make_ofd(&[("Doc_0/Pages/Page_0/Content.xml", PAGE)]);
        assert!(render_preview_page(&bytes, Path::new("invoice.ofd"), 0).is_err());
        assert!(render_preview_page(&bytes, Path::new("invoice.ofd"), 2).is_err());
    }
}
