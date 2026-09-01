//! PDF 内嵌结构化发票数据解析。
//!
//! 部分铁路电子客票 PDF 的可见文本使用特殊字体编码，通用 PDF 文本提取会失败，
//! 但文件同时通过 PDF EmbeddedFiles 名称树携带标准 XBRL/XML。本模块只读取该名称树
//! 中声明的附件，并对 PDF、名称树、附件数、压缩输入与解压输出分别设置上限。

use crate::field_extractor;
use crate::model::{ParseError, ParseLevel, ParsedInvoice, TicketType, TransportDocumentKind};
use crate::xml::{collect_leaf_elements, parse_amount, parse_date, parse_tax_rate, LeafElement};
use chrono::NaiveTime;
use flate2::read::ZlibDecoder;
use lopdf::{Document, Object, ObjectId, Stream};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

pub const MAX_PDF_BYTES: usize = 25 * 1024 * 1024;
const MAX_PDF_OBJECTS: usize = 100_000;
const MAX_NAME_TREE_NODES: usize = 256;
const MAX_EMBEDDED_STREAMS: usize = 32;
const MAX_EMBEDDED_COMPRESSED_BYTES: usize = 1024 * 1024;
const MAX_EMBEDDED_XML_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_EMBEDDED_XML_BYTES: usize = 4 * 1024 * 1024;

fn malformed(path: &Path, detail: impl Into<String>) -> ParseError {
    ParseError::MalformedFormat {
        path: path.to_path_buf(),
        format: "PDF",
        detail: detail.into(),
    }
}

fn missing(path: &Path, field: &str) -> ParseError {
    ParseError::MissingField {
        path: path.to_path_buf(),
        field: field.to_string(),
    }
}

#[derive(Default)]
struct EmbeddedState {
    visited_nodes: HashSet<ObjectId>,
    visited_streams: HashSet<ObjectId>,
    payload_hashes: HashSet<[u8; 32]>,
    name_tree_nodes: usize,
    embedded_streams: usize,
    total_xml_bytes: usize,
    payloads: Vec<Vec<u8>>,
}

/// 优先从 PDF 的 EmbeddedFiles 名称树读取铁路电子客票 XBRL/XML。
///
/// 只有同时包含铁路电子客票号码、开票日期和票价三个标准标签的 XML 才会命中。
/// 重复挂载会按内容哈希去重；若多个有效载荷的结构化字段相互冲突，则拒绝自动选择。
pub fn parse_embedded_rail_invoice(
    pdf_bytes: &[u8],
    path: &Path,
) -> Result<ParsedInvoice, ParseError> {
    if pdf_bytes.is_empty() || pdf_bytes.len() > MAX_PDF_BYTES {
        return Err(malformed(path, "文件大小超出解析上限"));
    }

    let document =
        Document::load_mem(pdf_bytes).map_err(|_| malformed(path, "PDF 对象结构无法读取"))?;
    if document.objects.len() > MAX_PDF_OBJECTS {
        return Err(malformed(path, "PDF 对象数量超出解析上限"));
    }

    let catalog = document
        .trailer
        .get_deref(b"Root", &document)
        .and_then(Object::as_dict)
        .map_err(|_| malformed(path, "PDF 缺少文档目录"))?;
    let names = catalog
        .get_deref(b"Names", &document)
        .and_then(Object::as_dict)
        .map_err(|_| missing(path, "embedded_invoice_xml"))?;
    let embedded_files = names
        .get(b"EmbeddedFiles")
        .map_err(|_| missing(path, "embedded_invoice_xml"))?;

    let mut state = EmbeddedState::default();
    walk_name_tree(&document, embedded_files, path, &mut state)?;

    let mut parsed = Vec::new();
    let mut first_candidate_error = None;
    for payload in &state.payloads {
        match parse_rail_xml(payload, path) {
            Ok(Some(invoice)) => parsed.push(invoice),
            Ok(None) => {}
            Err(error) => {
                first_candidate_error.get_or_insert(error);
            }
        }
    }

    let Some(first) = parsed.first().cloned() else {
        return Err(first_candidate_error.unwrap_or_else(|| missing(path, "embedded_invoice_xml")));
    };
    if parsed.iter().skip(1).any(|other| other != &first) {
        return Err(malformed(path, "内嵌结构化发票数据存在冲突"));
    }
    Ok(first)
}

fn walk_name_tree(
    document: &Document,
    node: &Object,
    path: &Path,
    state: &mut EmbeddedState,
) -> Result<(), ParseError> {
    let (node_id, node) = document
        .dereference(node)
        .map_err(|_| malformed(path, "EmbeddedFiles 名称树引用无效"))?;
    if let Some(node_id) = node_id {
        if !state.visited_nodes.insert(node_id) {
            return Ok(());
        }
    }
    state.name_tree_nodes += 1;
    if state.name_tree_nodes > MAX_NAME_TREE_NODES {
        return Err(malformed(path, "EmbeddedFiles 名称树超出节点上限"));
    }

    let dictionary = match node.as_dict() {
        Ok(dictionary) => dictionary,
        Err(_) => return Ok(()),
    };

    if let Ok(names) = dictionary
        .get_deref(b"Names", document)
        .and_then(Object::as_array)
    {
        if names.len() % 2 != 0 {
            return Err(malformed(path, "EmbeddedFiles 名称数组长度无效"));
        }
        for pair in names.chunks_exact(2) {
            collect_file_spec(document, &pair[1], path, state)?;
        }
    }

    if let Ok(kids) = dictionary
        .get_deref(b"Kids", document)
        .and_then(Object::as_array)
    {
        for child in kids {
            walk_name_tree(document, child, path, state)?;
        }
    }
    Ok(())
}

fn collect_file_spec(
    document: &Document,
    file_spec: &Object,
    path: &Path,
    state: &mut EmbeddedState,
) -> Result<(), ParseError> {
    let file_spec = match document
        .dereference(file_spec)
        .ok()
        .and_then(|(_, value)| value.as_dict().ok())
    {
        Some(file_spec) => file_spec,
        None => return Ok(()),
    };
    let embedded = match file_spec
        .get_deref(b"EF", document)
        .and_then(Object::as_dict)
    {
        Ok(embedded) => embedded,
        Err(_) => return Ok(()),
    };

    // PDF 规范允许 F 与 UF 同时引用同一个流，必须先按对象 ID、再按内容去重。
    for key in [b"UF".as_slice(), b"F".as_slice()] {
        let Ok(stream_object) = embedded.get(key) else {
            continue;
        };
        let Ok((stream_id, stream_object)) = document.dereference(stream_object) else {
            continue;
        };
        if let Some(stream_id) = stream_id {
            if !state.visited_streams.insert(stream_id) {
                continue;
            }
        }
        let Ok(stream) = stream_object.as_stream() else {
            continue;
        };

        state.embedded_streams += 1;
        if state.embedded_streams > MAX_EMBEDDED_STREAMS {
            return Err(malformed(path, "内嵌附件数量超出解析上限"));
        }
        let Some(payload) = decode_embedded_stream(document, stream, path)? else {
            continue;
        };
        if !looks_like_xml(&payload) {
            continue;
        }
        state.total_xml_bytes = state.total_xml_bytes.saturating_add(payload.len());
        if state.total_xml_bytes > MAX_TOTAL_EMBEDDED_XML_BYTES {
            return Err(malformed(path, "内嵌 XML 总大小超出解析上限"));
        }
        let digest: [u8; 32] = Sha256::digest(&payload).into();
        if state.payload_hashes.insert(digest) {
            state.payloads.push(payload);
        }
    }
    Ok(())
}

fn decode_embedded_stream(
    document: &Document,
    stream: &Stream,
    path: &Path,
) -> Result<Option<Vec<u8>>, ParseError> {
    if stream.content.len() > MAX_EMBEDDED_COMPRESSED_BYTES {
        return Err(malformed(path, "内嵌附件压缩数据超出解析上限"));
    }

    let filter = stream
        .dict
        .get(b"Filter")
        .ok()
        .and_then(|value| document.dereference(value).ok().map(|(_, value)| value));
    match filter {
        None => Ok(Some(stream.content.clone())),
        Some(Object::Name(name)) if name.as_slice() == b"FlateDecode" => {
            decode_flate_limited(&stream.content, path).map(Some)
        }
        Some(Object::Array(filters))
            if filters.len() == 1
                && matches!(&filters[0], Object::Name(name) if name.as_slice() == b"FlateDecode") =>
        {
            decode_flate_limited(&stream.content, path).map(Some)
        }
        // 非 XML 附件可能使用其他 PDF filter；当前解析器不展开它们。
        Some(_) => Ok(None),
    }
}

fn decode_flate_limited(bytes: &[u8], path: &Path) -> Result<Vec<u8>, ParseError> {
    let decoder = ZlibDecoder::new(bytes);
    let mut output = Vec::new();
    decoder
        .take((MAX_EMBEDDED_XML_BYTES + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|_| malformed(path, "内嵌附件解压失败"))?;
    if output.len() > MAX_EMBEDDED_XML_BYTES {
        return Err(malformed(path, "内嵌 XML 解压后超出大小上限"));
    }
    Ok(output)
}

fn looks_like_xml(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        == Some(b'<')
}

fn unique_leaf_value(leaves: &[LeafElement], tag: &str) -> Result<Option<String>, ()> {
    let mut values = leaves
        .iter()
        .filter(|leaf| leaf.tag == tag)
        .map(|leaf| leaf.text.trim())
        .filter(|value| !value.is_empty());
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.any(|value| value != first) {
        return Err(());
    }
    Ok(Some(first.to_string()))
}

fn optional_value(leaves: &[LeafElement], tags: &[&str]) -> Option<String> {
    tags.iter()
        .find_map(|tag| unique_leaf_value(leaves, tag).ok().flatten())
}

fn required_value(
    leaves: &[LeafElement],
    tag: &str,
    field: &str,
    path: &Path,
) -> Result<String, ParseError> {
    unique_leaf_value(leaves, tag)
        .map_err(|_| malformed(path, format!("结构化字段 {field} 存在冲突")))?
        .ok_or_else(|| missing(path, field))
}

fn parse_rail_xml(bytes: &[u8], path: &Path) -> Result<Option<ParsedInvoice>, ParseError> {
    let leaves = match collect_leaf_elements(bytes) {
        Ok(leaves) => leaves,
        Err(_) => return Ok(None),
    };
    if !leaves
        .iter()
        .any(|leaf| leaf.tag == "ElectronicInvoiceRailwayETicketNumber")
    {
        return Ok(None);
    }

    let invoice_number = required_value(
        &leaves,
        "ElectronicInvoiceRailwayETicketNumber",
        "invoice_number",
        path,
    )?;
    if !(8..=24).contains(&invoice_number.len())
        || !invoice_number
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return Err(malformed(path, "结构化发票号码格式无效"));
    }
    let issue_date_raw = required_value(&leaves, "DateOfIssue", "issue_date", path)?;
    let issue_date =
        parse_date(&issue_date_raw).map_err(|_| malformed(path, "结构化开票日期格式无效"))?;
    let fare_raw = required_value(&leaves, "Fare", "total_amount", path)?;
    let total_amount = parse_amount(&fare_raw, "total_amount")
        .map_err(|_| malformed(path, "结构化票价格式无效"))?;

    let tax_amount = optional_value(&leaves, &["TaxAmount"])
        .map(|raw| parse_amount(&raw, "tax_amount"))
        .transpose()
        .map_err(|_| malformed(path, "结构化税额格式无效"))?;
    let tax_rate = optional_value(&leaves, &["TaxRate"])
        .map(|raw| parse_tax_rate(&raw))
        .transpose()
        .map_err(|_| malformed(path, "结构化税率格式无效"))?;
    let buyer_name = optional_value(&leaves, &["BuyerName", "PurchaserName", "NameOfPurchaser"]);
    let seller_name = optional_value(
        &leaves,
        &[
            "SellerName",
            "NameOfSeller",
            "RailwayTransportEnterpriseName",
        ],
    );
    let transport_document_kind = optional_value(&leaves, &["TypeOfBusiness"])
        .map(|value| transport_document_kind(&value))
        .unwrap_or_default();

    let departure_station = optional_value(&leaves, &["DepartureStation"]);
    let destination_station = optional_value(&leaves, &["DestinationStation"]);
    let route = departure_station.as_ref().and_then(|departure| {
        destination_station
            .as_ref()
            .map(|destination| format!("{departure}→{destination}"))
    });
    let city = route
        .as_deref()
        .and_then(|route| field_extractor::extract_city(&TicketType::Rail, route));
    let departure_time = optional_value(&leaves, &["TravelDate"])
        .and_then(|raw| parse_date(&raw).ok())
        .and_then(|travel_date| {
            optional_value(&leaves, &["DepartureTime"])
                .and_then(|raw| parse_time(&raw))
                .map(|time| travel_date.and_time(time))
        });

    Ok(Some(ParsedInvoice {
        invoice_number,
        issue_date,
        total_amount,
        tax_amount,
        tax_rate,
        buyer_name,
        seller_name,
        ticket_type: TicketType::Rail,
        transport_document_kind,
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        city,
        travel_route: route,
        departure_time,
        checkin_date: None,
        source_path: path.to_path_buf(),
    }))
}

fn transport_document_kind(value: &str) -> TransportDocumentKind {
    let normalized = value.trim();
    if normalized == "退" || normalized.contains("退票") {
        TransportDocumentKind::Refund
    } else if normalized == "改" || normalized.contains("改签") {
        TransportDocumentKind::Change
    } else if normalized == "售" || normalized.contains("售票") {
        TransportDocumentKind::Sale
    } else {
        TransportDocumentKind::Unknown
    }
}

fn parse_time(raw: &str) -> Option<NaiveTime> {
    ["%H:%M:%S", "%H:%M", "%H%M%S", "%H%M"]
        .iter()
        .find_map(|format| NaiveTime::parse_from_str(raw.trim(), format).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use lopdf::{dictionary, Object, Stream};
    use rust_decimal::Decimal;
    use std::io::Write;
    use std::str::FromStr;

    const RAIL_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
        <xbrli:xbrl xmlns:xbrli="urn:xbrl" xmlns:rail="urn:rail">
          <rail:ElectronicInvoiceRailwayETicketNumber>24312000000012345678</rail:ElectronicInvoiceRailwayETicketNumber>
          <rail:DateOfIssue>2026-07-03</rail:DateOfIssue>
          <rail:TypeOfBusiness>售</rail:TypeOfBusiness>
          <rail:TravelDate>2026-07-05</rail:TravelDate>
          <rail:DepartureTime>08:09</rail:DepartureTime>
          <rail:DepartureStation>北京南</rail:DepartureStation>
          <rail:DestinationStation>上海虹桥</rail:DestinationStation>
          <rail:Fare>553.00</rail:Fare>
          <rail:TaxAmount>45.66</rail:TaxAmount>
          <rail:TaxRate>9%</rail:TaxRate>
        </xbrli:xbrl>"#;

    fn embedded_pdf(payloads: &[(&[u8], bool)]) -> Vec<u8> {
        let mut document = Document::with_version("1.7");
        let pages_id = document.new_object_id();
        let mut names = Vec::new();

        for (index, (payload, compressed)) in payloads.iter().enumerate() {
            let mut stream = if *compressed {
                let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(payload).unwrap();
                let compressed = encoder.finish().unwrap();
                Stream::new(
                    dictionary! {
                        "Type" => "EmbeddedFile",
                        "Filter" => "FlateDecode",
                    },
                    compressed,
                )
            } else {
                Stream::new(dictionary! { "Type" => "EmbeddedFile" }, payload.to_vec())
            };
            stream
                .dict
                .set("Subtype", Object::Name(b"text/xml".to_vec()));
            let stream_id = document.add_object(stream);
            let file_spec_id = document.add_object(dictionary! {
                "Type" => "Filespec",
                "F" => Object::string_literal(format!("invoice-{index}.xml")),
                "EF" => dictionary! { "F" => stream_id, "UF" => stream_id },
            });
            names.push(Object::string_literal(format!("invoice-{index}.xml")));
            names.push(file_spec_id.into());
        }

        let embedded_files_id = document.add_object(dictionary! { "Names" => names });
        let names_id = document.add_object(dictionary! { "EmbeddedFiles" => embedded_files_id });
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        });
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
            "Names" => names_id,
        });
        document.trailer.set("Root", catalog_id);
        let mut output = Vec::new();
        document.save_to(&mut output).unwrap();
        output
    }

    #[test]
    fn parses_uncompressed_embedded_rail_xbrl() {
        let invoice = parse_embedded_rail_invoice(
            &embedded_pdf(&[(RAIL_XML.as_bytes(), false)]),
            Path::new("rail.pdf"),
        )
        .unwrap();

        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
        assert_eq!(invoice.total_amount, Decimal::from_str("553.00").unwrap());
        assert_eq!(
            invoice.tax_amount,
            Some(Decimal::from_str("45.66").unwrap())
        );
        assert_eq!(invoice.tax_rate, Some(Decimal::from_str("0.09").unwrap()));
        assert_eq!(invoice.ticket_type, TicketType::Rail);
        assert_eq!(invoice.transport_document_kind, TransportDocumentKind::Sale);
        assert_eq!(invoice.parse_level, ParseLevel::L0);
        assert_eq!(invoice.city.as_deref(), Some("北京"));
        assert_eq!(invoice.travel_route.as_deref(), Some("北京南→上海虹桥"));
        assert_eq!(
            invoice.departure_time.unwrap().to_string(),
            "2026-07-05 08:09:00"
        );
    }

    #[test]
    fn reads_refund_business_type_from_embedded_rail_xbrl() {
        let refund_xml = RAIL_XML.replace(">售<", ">退<");
        let invoice = parse_embedded_rail_invoice(
            &embedded_pdf(&[(refund_xml.as_bytes(), false)]),
            Path::new("refund.pdf"),
        )
        .unwrap();

        assert_eq!(
            invoice.transport_document_kind,
            TransportDocumentKind::Refund
        );
        assert_eq!(invoice.travel_route.as_deref(), Some("北京南→上海虹桥"));
    }

    #[test]
    fn parses_flate_embedded_xml_and_deduplicates_references() {
        let invoice = parse_embedded_rail_invoice(
            &embedded_pdf(&[(RAIL_XML.as_bytes(), true), (RAIL_XML.as_bytes(), false)]),
            Path::new("rail.pdf"),
        )
        .unwrap();
        assert_eq!(invoice.total_amount, Decimal::from_str("553.00").unwrap());
    }

    #[test]
    fn ignores_non_invoice_xml_attachment() {
        let error = parse_embedded_rail_invoice(
            &embedded_pdf(&[(b"<notes><amount>1.00</amount></notes>", false)]),
            Path::new("notes.pdf"),
        )
        .unwrap_err();
        assert!(matches!(error, ParseError::MissingField { .. }));
    }

    #[test]
    fn rejects_conflicting_embedded_invoices() {
        let conflicting = RAIL_XML.replace("553.00", "554.00");
        let error = parse_embedded_rail_invoice(
            &embedded_pdf(&[
                (RAIL_XML.as_bytes(), false),
                (conflicting.as_bytes(), false),
            ]),
            Path::new("conflict.pdf"),
        )
        .unwrap_err();
        assert!(matches!(error, ParseError::MalformedFormat { .. }));
    }

    #[test]
    fn rejects_decompression_bomb_at_output_limit() {
        let oversized = vec![b' '; MAX_EMBEDDED_XML_BYTES + 1];
        let error = parse_embedded_rail_invoice(
            &embedded_pdf(&[(oversized.as_slice(), true)]),
            Path::new("oversized.pdf"),
        )
        .unwrap_err();
        assert!(matches!(error, ParseError::MalformedFormat { .. }));
    }

    /// 私有真实样本只允许通过显式环境变量启用；测试仅输出聚合计数，
    /// 不输出文件名、路径或任何结构化字段值。
    #[test]
    #[ignore = "requires an explicitly authorized private PDF sample directory"]
    fn private_pdf_set_matches_expected_embedded_rail_count() {
        let root = std::env::var_os("INVOICE_REAL_EMBEDDED_PDF_ROOT")
            .expect("INVOICE_REAL_EMBEDDED_PDF_ROOT is required");
        let expected: usize = std::env::var("INVOICE_REAL_EMBEDDED_EXPECTED")
            .expect("INVOICE_REAL_EMBEDDED_EXPECTED is required")
            .parse()
            .expect("INVOICE_REAL_EMBEDDED_EXPECTED must be an integer");

        let mut pdf_files = 0usize;
        let mut parsed = 0usize;
        let mut complete_travel = 0usize;
        for entry in std::fs::read_dir(root).expect("private PDF directory must be readable") {
            let path = entry
                .expect("private directory entry must be readable")
                .path();
            if !path.is_file()
                || !path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
            {
                continue;
            }
            pdf_files += 1;
            let bytes = std::fs::read(&path).expect("private PDF must be readable");
            if let Ok(invoice) =
                parse_embedded_rail_invoice(&bytes, Path::new("private-sample.pdf"))
            {
                parsed += 1;
                if invoice.travel_route.is_some()
                    && invoice.departure_time.is_some()
                    && invoice.city.is_some()
                {
                    complete_travel += 1;
                }
            }
        }

        println!("private_pdf_files={pdf_files}");
        println!("embedded_rail_parsed={parsed}");
        println!("complete_rail_travel_fields={complete_travel}");
        println!("private_values_logged=false");
        assert_eq!(parsed, expected);
        assert_eq!(complete_travel, expected);
    }
}
