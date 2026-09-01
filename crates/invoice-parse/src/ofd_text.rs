//! 从 OFD 版式 XML 中直接提取文本与坐标。
//!
//! OFD 是 ZIP 容器，`Doc_0/Pages/Page_*/Content.xml` 里的
//! `<ofd:TextObject Boundary="x y w h">` 包着 `<ofd:TextCode>` 文本。
//! 这是文件自带的确定性文本 + 精确坐标，因此定级 L1 而不是 L2：
//! 不需要渲染、不需要 OCR、不需要 java。

use crate::model::{ParseError, ParseLevel, ParsedInvoice};
use crate::ocr::{locate_vat_fields, merge_line_fragments, TextBox};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::{Cursor, Read};
use std::path::Path;

/// 毫米转像素（96 DPI）。
///
/// OFD Boundary 单位是毫米，行间距只有 3–6mm，
/// 而 ocr::SAME_LINE_TOLERANCE 是按像素定的 15.0。
/// 不换算的话所有框会被判成同一行。
pub const MM_TO_PX: f32 = 3.7795;

/// 同一行内两个碎片框之间允许的最大水平间隙（像素）。
/// 美团票把一个词拆成 4 个 TextObject，间隙接近 0；
/// 不同字段之间的间隙远大于此值。
const FRAGMENT_MAX_GAP: f32 = 6.0;

pub fn extract_text_boxes(bytes: &[u8], path: &Path) -> Result<Vec<TextBox>, ParseError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes.to_vec())).map_err(|e| {
        ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format: "OFD",
            detail: format!("不是有效的 ZIP 容器: {e}"),
        }
    })?;

    // 收集所有 Content.xml（可能多页 + 模板层）
    let content_names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with("content.xml") && (l.contains("/pages/") || l.contains("/tpls/"))
        })
        .collect();

    if content_names.is_empty() {
        return Err(ParseError::MissingField {
            path: path.to_path_buf(),
            field: "Content.xml".to_string(),
        });
    }

    let mut boxes = Vec::new();
    for name in content_names {
        let mut raw = Vec::new();
        zip.by_name(&name)
            .map_err(|e| ParseError::MalformedFormat {
                path: path.to_path_buf(),
                format: "OFD",
                detail: format!("读取 {name} 失败: {e}"),
            })?
            .read_to_end(&mut raw)
            .map_err(|e| ParseError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
        boxes.extend(parse_content_xml(&raw, path)?);
    }
    Ok(boxes)
}

/// 解析单个 Content.xml。
///
/// 必须用真正的 XML 解析器：某些开票平台在 TextObject 与 TextCode 之间
/// 夹了 CGTransform / FillColor 等子元素，正则匹配不到。
fn parse_content_xml(xml: &[u8], path: &Path) -> Result<Vec<TextBox>, ParseError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut out = Vec::new();
    let mut buf = Vec::new();
    // 当前 TextObject 的 Boundary（毫米）
    let mut pending: Option<(f32, f32, f32, f32)> = None;
    let mut in_text_code = false;
    let mut text = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = local_tag(e.name().as_ref());
                if name == "TextObject" {
                    pending = e
                        .attributes()
                        .flatten()
                        .find(|a| local_tag(a.key.as_ref()) == "Boundary")
                        .and_then(|a| parse_boundary(&a.value));
                    text.clear();
                } else if name == "TextCode" {
                    in_text_code = true;
                }
            }
            Ok(Event::Text(e)) if in_text_code => {
                if let Ok(s) = e.unescape() {
                    text.push_str(&s);
                }
            }
            // 数电票常把真实字段放在 CDATA 中。只处理 Event::Text 会留下标签、
            // 丢掉发票号码/日期/金额，最终把一张有效 OFD 错送到“配套材料待处理”。
            Ok(Event::CData(e)) if in_text_code => {
                text.push_str(&String::from_utf8_lossy(e.as_ref()));
            }
            Ok(Event::End(e)) => {
                let name = local_tag(e.name().as_ref());
                if name == "TextCode" {
                    in_text_code = false;
                } else if name == "TextObject" {
                    if let Some((x, y, w, h)) = pending.take() {
                        let t = text.trim();
                        if !t.is_empty() {
                            out.push(TextBox {
                                text: t.to_string(),
                                x: x * MM_TO_PX,
                                y: y * MM_TO_PX,
                                width: w * MM_TO_PX,
                                height: h * MM_TO_PX,
                                // 文件自带文本，不是识别结果
                                confidence: 1.0,
                            });
                        }
                    }
                    text.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(ParseError::MalformedFormat {
                    path: path.to_path_buf(),
                    format: "OFD",
                    detail: format!("版式 XML 解析失败: {e}"),
                })
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// 去掉命名空间前缀：`ofd:TextObject` → `TextObject`
fn local_tag(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    s.rsplit(':').next().unwrap_or(&s).to_string()
}

/// `Boundary="x y w h"`，单位毫米。
fn parse_boundary(raw: &[u8]) -> Option<(f32, f32, f32, f32)> {
    let s = String::from_utf8_lossy(raw);
    let v: Vec<f32> = s
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect();
    if v.len() == 4 {
        Some((v[0], v[1], v[2], v[3]))
    } else {
        None
    }
}

/// OFD 版式文本 → ParsedInvoice（L1）。
pub fn parse_invoice_ofd_text(bytes: &[u8], path: &Path) -> Result<ParsedInvoice, ParseError> {
    let boxes = extract_text_boxes(bytes, path)?;
    let merged = merge_line_fragments(boxes, FRAGMENT_MAX_GAP);
    locate_vat_fields(&merged, path, ParseLevel::L1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    /// 构造一个最小 OFD：ZIP 里只放一个 Content.xml。
    fn make_ofd(content_xml: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("Doc_0/Pages/Page_0/Content.xml", opts)
                .unwrap();
            zip.write_all(content_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    /// 标签与值各自一个 TextObject，同一行，值在右侧。
    const CLEAN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016"><ofd:Content><ofd:Layer>
<ofd:TextObject ID="14" Boundary="154.5 10.9 20 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">发票号码：</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="15" Boundary="176.0 10.9 30 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">26132000001954318426</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="16" Boundary="154.5 16.9 20 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">开票日期：</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="17" Boundary="176.0 16.9 30 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">2026年06月22日</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="18" Boundary="20.0 80.0 24 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">价税合计</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="19" Boundary="50.0 80.0 20 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">￥47.40</ofd:TextCode></ofd:TextObject>
</ofd:Layer></ofd:Content></ofd:Page>"#;

    /// 美团版式：TextCode 前夹着 CGTransform / FillColor 子元素，
    /// 且一个词被拆成多个 TextObject。正则做法在这里必然失败。
    const FRAGMENTED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016"><ofd:Content><ofd:Layer>
<ofd:TextObject ID="116" Boundary="154.530 10.701 3.300 4.874" Size="3.300" CTM="1.0 0.0 0.0 1.0 0.0 0.0"><ofd:CGTransform CodePosition="0" GlyphCount="1" CodeCount="1"><ofd:Glyphs>1</ofd:Glyphs></ofd:CGTransform><ofd:FillColor Value="0 0 0"/><ofd:TextCode X="0" Y="3.3">发</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="117" Boundary="157.830 10.701 3.300 4.874" Size="3.300"><ofd:FillColor Value="0 0 0"/><ofd:TextCode X="0" Y="3.3">票号</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="118" Boundary="161.130 10.701 3.300 4.874" Size="3.300"><ofd:TextCode X="0" Y="3.3">码：</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="119" Boundary="165.000 10.701 30.000 4.874" Size="3.300"><ofd:TextCode X="0" Y="3.3">26112000002208097411</ofd:TextCode></ofd:TextObject>
</ofd:Layer></ofd:Content></ofd:Page>"#;

    #[test]
    fn extracts_text_with_coordinates() {
        let boxes = extract_text_boxes(&make_ofd(CLEAN), Path::new("t.ofd")).unwrap();
        assert!(
            boxes.len() >= 6,
            "应提取到至少 6 个文本框，实际 {}",
            boxes.len()
        );
        assert!(
            boxes
                .iter()
                .any(|b| b.text.contains("26132000001954318426")),
            "应含发票号码，实际: {:?}",
            boxes.iter().map(|b| &b.text).collect::<Vec<_>>()
        );
    }

    /// 坐标必须换算成像素，否则同行判定失效。
    #[test]
    fn coordinates_are_scaled_to_pixels() {
        let boxes = extract_text_boxes(&make_ofd(CLEAN), Path::new("t.ofd")).unwrap();
        let num = boxes.iter().find(|b| b.text.contains("2613")).unwrap();
        // Boundary x=176.0 mm → 176.0 * 3.7795 ≈ 665
        assert!(
            (num.x - 176.0 * MM_TO_PX).abs() < 1.0,
            "x 应换算为像素 ≈{:.1}，实际 {:.1}",
            176.0 * MM_TO_PX,
            num.x
        );
    }

    /// 结构化置信度恒为 1.0——这是文件自带文本，不是识别猜测。
    #[test]
    fn confidence_is_one_for_structured_text() {
        let boxes = extract_text_boxes(&make_ofd(CLEAN), Path::new("t.ofd")).unwrap();
        assert!(boxes.iter().all(|b| b.confidence == 1.0));
    }

    /// 标签与值在同一行、值在右侧 —— 字段定位器应该能配上。
    #[test]
    fn clean_layout_locates_all_three_required_fields() {
        let inv = parse_invoice_ofd_text(&make_ofd(CLEAN), Path::new("t.ofd")).unwrap();
        assert_eq!(inv.invoice_number, "26132000001954318426");
        assert_eq!(inv.issue_date.to_string(), "2026-06-22");
        assert_eq!(
            inv.total_amount,
            rust_decimal::Decimal::from_str_exact("47.40").unwrap()
        );
    }

    /// OFD 版式文本是文件自带的确定性文本，定级 L1，不是 L2。
    #[test]
    fn parse_level_is_l1_not_l2() {
        let inv = parse_invoice_ofd_text(&make_ofd(CLEAN), Path::new("t.ofd")).unwrap();
        assert_eq!(inv.parse_level, ParseLevel::L1);
        assert_eq!(inv.confidence, 1.0);
    }

    /// 被拆成 发/票号/码： 的碎片必须先合并，否则找不到 "发票号码" 标签。
    #[test]
    fn fragmented_layout_merges_and_locates() {
        let inv = parse_invoice_ofd_text(&make_ofd(FRAGMENTED), Path::new("t.ofd"));
        match inv {
            Ok(i) => assert_eq!(i.invoice_number, "26112000002208097411"),
            Err(e) => {
                // 该测试样本只有发票号码，缺日期和金额，
                // 因此允许因缺字段失败，但绝不能是找不到 invoice_number
                let msg = e.to_string();
                assert!(
                    !msg.contains("invoice_number"),
                    "碎片合并失败，没能识别出发票号码标签: {msg}"
                );
            }
        }
    }

    #[test]
    fn merged_text_reassembles_label() {
        let boxes = extract_text_boxes(&make_ofd(FRAGMENTED), Path::new("t.ofd")).unwrap();
        let merged = merge_line_fragments(boxes, FRAGMENT_MAX_GAP);
        assert!(
            merged.iter().any(|b| b.text.contains("发票号码")),
            "合并后应出现完整标签 发票号码，实际: {:?}",
            merged.iter().map(|b| &b.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn corrupt_zip_returns_malformed_error() {
        let err = extract_text_boxes(b"not a zip at all", Path::new("bad.ofd")).unwrap_err();
        assert!(
            matches!(err, ParseError::MalformedFormat { .. }),
            "应返回 MalformedFormat，实际 {err:?}"
        );
    }

    #[test]
    fn zip_without_content_xml_reports_missing_field() {
        let empty = make_ofd_named("Doc_0/Document.xml", "<a/>");
        let err = extract_text_boxes(&empty, Path::new("e.ofd")).unwrap_err();
        assert!(
            matches!(err, ParseError::MissingField { .. }),
            "实际 {err:?}"
        );
    }

    fn make_ofd_named(name: &str, body: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    /// 真实样本回归：这 6 个 OFD 必须能提取到文本框。
    /// 02-unknown-f6f7c6b1.ofd 的 ZIP 中央目录损坏，不在其列。
    #[test]
    fn real_samples_yield_text_boxes() {
        let names = [
            "11-meituan-34ee412d.ofd",
            "28-unknown-36c9093e.ofd",
            "33-unknown-1f1e61a4.ofd",
            "40-meituan-12f8065e.ofd",
            "45-unknown-3ed9ed77.ofd",
            "48-unknown-cb25d50d.ofd",
            "63-unknown-19d988e1.ofd",
        ];
        for n in names {
            let p = PathBuf::from("../../fixtures/samples").join(n);
            let Ok(bytes) = std::fs::read(&p) else {
                continue;
            };
            let boxes =
                extract_text_boxes(&bytes, &p).unwrap_or_else(|e| panic!("{n} 提取失败: {e}"));
            assert!(!boxes.is_empty(), "{n} 应提取到文本框");
        }
    }

    #[test]
    fn extracts_values_stored_in_cdata() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016"><ofd:Content><ofd:Layer>
<ofd:TextObject Boundary="170.5 11.5 27 5"><ofd:TextCode><![CDATA[26312000003409200316]]></ofd:TextCode></ofd:TextObject>
</ofd:Layer></ofd:Content></ofd:Page>"#;
        let boxes = extract_text_boxes(&make_ofd(content), Path::new("cdata.ofd")).unwrap();
        assert!(boxes.iter().any(|item| item.text == "26312000003409200316"));
    }
}
