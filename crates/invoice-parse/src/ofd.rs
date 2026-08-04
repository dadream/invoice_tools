use crate::manifest::TagHints;
use crate::model::{ParseError, ParsedInvoice, TicketType};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

/// 列出 OFD（ZIP）内所有条目名，用于探查结构。
pub fn list_entries(ofd_bytes: &[u8]) -> Result<Vec<String>, ParseError> {
    let mut archive = open_zip(ofd_bytes, Path::new(""))?;
    let mut names = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| ParseError::MalformedFormat {
            path: PathBuf::new(),
            format: "OFD",
            detail: format!("读取第 {i} 个条目失败: {e}"),
        })?;
        names.push(entry.name().to_string());
    }
    Ok(names)
}

/// 从 OFD 中取出内嵌的发票 XML。
/// 策略：优先找路径含 "invoice"/"发票" 的 .xml；
/// 退化为选择除版式文件（OFD.xml/Document.xml/DocumentRes.xml 等）之外
/// 体积最大的 .xml —— 内嵌发票数据通常远大于结构描述文件。
pub fn extract_invoice_xml(ofd_bytes: &[u8], path: &Path) -> Result<Vec<u8>, ParseError> {
    let mut archive = open_zip(ofd_bytes, path)?;

    // 收集所有非版式的 .xml 条目：(索引, 名称, 体积)
    let mut candidates: Vec<(usize, String, u64)> = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format: "OFD",
            detail: format!("读取第 {i} 个条目失败: {e}"),
        })?;
        let name = entry.name().to_string();
        if name.to_lowercase().ends_with(".xml") && !is_layout_file(&name) {
            candidates.push((i, name, entry.size()));
        }
    }

    if candidates.is_empty() {
        return Err(ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format: "OFD",
            detail: "找不到内嵌的发票 XML（容器内只有版式文件）".to_string(),
        });
    }

    // 优先：路径提到 invoice / 发票
    let chosen = candidates
        .iter()
        .find(|(_, name, _)| {
            let lower = name.to_lowercase();
            lower.contains("invoice") || name.contains("发票")
        })
        // 退化：体积最大的
        .or_else(|| candidates.iter().max_by_key(|(_, _, size)| *size))
        .map(|(i, _, _)| *i)
        .expect("candidates 非空");

    let mut entry = archive.by_index(chosen).map_err(|e| ParseError::MalformedFormat {
        path: path.to_path_buf(),
        format: "OFD",
        detail: format!("打开内嵌 XML 失败: {e}"),
    })?;

    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf).map_err(|e| ParseError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(buf)
}

pub fn parse_invoice_ofd(
    ofd_bytes: &[u8],
    path: &Path,
    hints: &TagHints,
    ticket_type: TicketType,
) -> Result<ParsedInvoice, ParseError> {
    let xml_bytes = extract_invoice_xml(ofd_bytes, path)?;
    crate::xml::parse_invoice_xml(&xml_bytes, path, hints, ticket_type)
}

fn open_zip(
    bytes: &[u8],
    path: &Path,
) -> Result<zip::ZipArchive<Cursor<Vec<u8>>>, ParseError> {
    zip::ZipArchive::new(Cursor::new(bytes.to_vec())).map_err(|e| ParseError::MalformedFormat {
        path: path.to_path_buf(),
        format: "OFD",
        detail: format!("不是有效的 ZIP 容器: {e}"),
    })
}

/// 这些是 OFD 的版式结构文件，不含发票业务数据。
const LAYOUT_FILES: &[&str] = &[
    "OFD.xml",
    "Document.xml",
    "DocumentRes.xml",
    "PublicRes.xml",
    "Annotations.xml",
    "Signatures.xml",
    "Signature.xml",
    "Attachments.xml",
];

fn is_layout_file(entry_name: &str) -> bool {
    let file_name = entry_name.rsplit('/').next().unwrap_or(entry_name);
    LAYOUT_FILES.iter().any(|f| f.eq_ignore_ascii_case(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    /// 构造一个最小 OFD：一个版式文件 + 一个内嵌发票 XML
    fn build_ofd(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            for (name, content) in entries {
                zip.start_file(*name, SimpleFileOptions::default()).unwrap();
                zip.write_all(content).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    const INVOICE_XML: &[u8] = br#"<Invoice>
        <Fphm>24312000000012345678</Fphm><Kprq>2026-07-03</Kprq>
        <Jshj>553.00</Jshj><Se>50.73</Se>
    </Invoice>"#;

    #[test]
    fn lists_all_zip_entries() {
        let ofd = build_ofd(&[("OFD.xml", b"<OFD/>"), ("Doc_0/invoice.xml", INVOICE_XML)]);
        let entries = list_entries(&ofd).unwrap();
        assert!(entries.contains(&"OFD.xml".to_string()));
        assert!(entries.contains(&"Doc_0/invoice.xml".to_string()));
    }

    #[test]
    fn picks_xml_whose_path_mentions_invoice() {
        let ofd = build_ofd(&[
            ("OFD.xml", b"<OFD/>"),
            ("Doc_0/Document.xml", b"<Document/>"),
            ("Doc_0/Attachs/invoice.xml", INVOICE_XML),
        ]);
        let xml = extract_invoice_xml(&ofd, Path::new("x.ofd")).unwrap();
        assert_eq!(xml, INVOICE_XML);
    }

    #[test]
    fn falls_back_to_largest_non_layout_xml() {
        let ofd = build_ofd(&[
            ("OFD.xml", b"<OFD/>"),
            ("Doc_0/Document.xml", b"<Document/>"),
            ("Doc_0/Attachs/data_001.xml", INVOICE_XML),
        ]);
        let xml = extract_invoice_xml(&ofd, Path::new("x.ofd")).unwrap();
        assert_eq!(xml, INVOICE_XML);
    }

    #[test]
    fn layout_only_ofd_errors_clearly() {
        let ofd = build_ofd(&[("OFD.xml", b"<OFD/>"), ("Doc_0/Document.xml", b"<Document/>")]);
        let err = extract_invoice_xml(&ofd, Path::new("x.ofd")).unwrap_err();
        assert!(err.to_string().contains("找不到"), "错误应说明未找到内嵌 XML");
    }

    #[test]
    fn non_zip_input_errors() {
        let err = list_entries(b"this is not a zip").unwrap_err();
        assert!(matches!(err, ParseError::MalformedFormat { .. }));
    }

    #[test]
    fn end_to_end_ofd_parse_yields_fields() {
        let ofd = build_ofd(&[("OFD.xml", b"<OFD/>"), ("Doc_0/Attachs/invoice.xml", INVOICE_XML)]);
        let hints = TagHints {
            invoice_number: vec!["Fphm".into()],
            issue_date: vec!["Kprq".into()],
            total_amount: vec!["Jshj".into()],
            tax_amount: vec!["Se".into()],
            tax_rate: vec![],
            buyer_name: vec![],
            seller_name: vec![],
        };
        let invoice =
            parse_invoice_ofd(&ofd, Path::new("x.ofd"), &hints, TicketType::Hotel).unwrap();
        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.ticket_type, TicketType::Hotel);
    }
}
