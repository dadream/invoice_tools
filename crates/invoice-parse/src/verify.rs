use crate::model::ParseError;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum SignatureStatus {
    /// 签章验证通过：内容未被篡改，且由可信主体签发
    Valid,
    /// 签章存在但验证失败
    Invalid { reason: String },
    /// 已发现数字签章，但当前版本尚不支持该签章规范的完整密码学验证。
    ///
    /// 该状态不能解释为文件被篡改，也不能阻断报销审核。
    Unsupported { reason: String },
    /// 容器内没有签章文件（如纯版式 OFD、非数电票）
    NotSigned,
}

/// 从 OFD 容器中提取的签章原始数据。
#[derive(Debug, Clone)]
pub struct SignatureData {
    /// 签章文件在容器内的路径
    pub entry_name: String,
    /// 签章文件原始字节（含 SES_Signature 结构）
    pub raw: Vec<u8>,
}

/// 检查 XML 数电票的数字签章能力。
///
/// XMLDSig/税务签章需要按声明的 Transform、CanonicalizationMethod、
/// Reference 摘要以及证书链完成验证。旧实现直接对整个 XML 做 SM2 验签，
/// 会把正常文件误判为被篡改，因此在完整验证器落地前必须诚实返回 Unsupported。
pub fn verify_xml_signature(xml_bytes: &[u8], path: &Path) -> Result<SignatureStatus, ParseError> {
    let mut reader = quick_xml::Reader::from_reader(xml_bytes);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut saw_element = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(quick_xml::events::Event::Start(element))
            | Ok(quick_xml::events::Event::Empty(element)) => {
                saw_element = true;
                if element.local_name().as_ref() == b"SignatureValue" {
                    return Ok(SignatureStatus::Unsupported {
                        reason: "XML 数字签章需要按 XMLDSig 规范完成引用摘要、规范化与证书链验证"
                            .to_string(),
                    });
                }
            }
            Ok(quick_xml::events::Event::Eof) if saw_element => {
                return Ok(SignatureStatus::NotSigned)
            }
            Ok(quick_xml::events::Event::Eof) => {
                return Err(ParseError::MalformedFormat {
                    path: path.to_path_buf(),
                    format: "XML",
                    detail: "文件中没有 XML 元素".to_string(),
                })
            }
            Ok(_) => {}
            Err(error) => {
                return Err(ParseError::MalformedFormat {
                    path: path.to_path_buf(),
                    format: "XML",
                    detail: format!("签章结构解析失败: {error}"),
                })
            }
        }
        buffer.clear();
    }
}

/// 在 OFD 容器中定位签章文件。
/// 数电票的签章通常位于 `Doc_0/Signs/Sign_0/SignedValue.dat`
/// 或 `Doc_0/Signs/Signatures.xml` 指向的文件。
pub fn locate_signature(ofd_bytes: &[u8]) -> Result<Option<SignatureData>, ParseError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(ofd_bytes.to_vec())).map_err(|e| {
        ParseError::MalformedFormat {
            path: PathBuf::new(),
            format: "OFD",
            detail: format!("不是有效的 ZIP 容器: {e}"),
        }
    })?;

    let mut hit: Option<(u8, usize)> = None;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| ParseError::MalformedFormat {
                path: PathBuf::new(),
                format: "OFD",
                detail: format!("读取第 {i} 个条目失败: {e}"),
            })?;
        if let Some(priority) = signature_entry_priority(entry.name()) {
            if hit.map_or(true, |(current, _)| priority < current) {
                hit = Some((priority, i));
            }
        }
    }

    let Some((_, index)) = hit else {
        return Ok(None);
    };

    let mut entry = archive
        .by_index(index)
        .map_err(|e| ParseError::MalformedFormat {
            path: PathBuf::new(),
            format: "OFD",
            detail: format!("打开签章文件失败: {e}"),
        })?;
    let entry_name = entry.name().to_string();
    let mut raw = Vec::new();
    entry.read_to_end(&mut raw).map_err(|e| ParseError::Io {
        path: PathBuf::from(&entry_name),
        source: e,
    })?;

    Ok(Some(SignatureData { entry_name, raw }))
}

/// 验证 OFD 的数字签章。
pub fn verify_ofd_signature(ofd_bytes: &[u8], _path: &Path) -> Result<SignatureStatus, ParseError> {
    let Some(sig) = locate_signature(ofd_bytes)? else {
        return Ok(SignatureStatus::NotSigned);
    };

    if sig.raw.is_empty() {
        return Ok(SignatureStatus::Invalid {
            reason: format!("签章文件 {} 为空", sig.entry_name),
        });
    }

    // OFD 的 SignedValue.dat 是 SES_Signature。完整验签必须先按 Signature.xml
    // 校验每个 Reference 的摘要，再按 GB/T 38540 解析签章、证书和算法参数。
    // 旧实现以“内嵌发票 XML”作为签名原文，这不符合规范并会稳定地产生误报。
    Ok(SignatureStatus::Unsupported {
        reason: format!(
            "已找到签章文件 {}，当前版本尚未完成 OFD/SES 引用摘要与证书链验证",
            sig.entry_name
        ),
    })
}

/// 返回真实签章值文件的候选优先级（数值越小越优先）。
///
/// `Signatures.xml` 和 `Signature.xml` 只是索引/描述文件，不能当作签章值。
fn signature_entry_priority(entry_name: &str) -> Option<u8> {
    let lower = entry_name.to_lowercase();
    let file_name = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    if file_name == "signedvalue.dat" {
        Some(0)
    } else if file_name == "signature.dat" {
        Some(1)
    } else if file_name == "seal.dat" {
        Some(2)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

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

    #[test]
    fn finds_signature_by_path_hint() {
        let ofd = build_ofd(&[
            ("OFD.xml", b"<OFD/>"),
            (
                "Doc_0/Signs/Sign_0/SignedValue.dat",
                b"fake-signature-bytes",
            ),
        ]);
        let found = locate_signature(&ofd).unwrap().expect("应找到签章");
        assert_eq!(found.entry_name, "Doc_0/Signs/Sign_0/SignedValue.dat");
        assert_eq!(found.raw, b"fake-signature-bytes");
    }

    #[test]
    fn descriptor_xml_is_not_mistaken_for_signature_value() {
        let descriptor = br#"<?xml version="1.0"?><ofd:Signatures xmlns:ofd="urn:ofd"><ofd:Signature BaseLoc="Sign_0/Signature.xml"/></ofd:Signatures>"#;
        let signed_value = b"real-signed-value";
        let ofd = build_ofd(&[
            ("OFD.xml", b"<OFD/>"),
            ("Doc_0/Signs/Signatures.xml", descriptor),
            ("Doc_0/Signs/Sign_0/Signature.xml", b"<Signature/>"),
            ("Doc_0/Signs/Sign_0/SignedValue.dat", signed_value),
        ]);
        let found = locate_signature(&ofd).unwrap().expect("应找到签章值");
        assert_eq!(found.entry_name, "Doc_0/Signs/Sign_0/SignedValue.dat");
        assert_eq!(found.raw, signed_value);
    }

    #[test]
    fn unsigned_container_returns_none() {
        let ofd = build_ofd(&[("OFD.xml", b"<OFD/>"), ("Doc_0/Document.xml", b"<Doc/>")]);
        assert!(locate_signature(&ofd).unwrap().is_none());
    }

    #[test]
    fn unsigned_container_reports_not_signed() {
        let ofd = build_ofd(&[("OFD.xml", b"<OFD/>")]);
        let status = verify_ofd_signature(&ofd, Path::new("x.ofd")).unwrap();
        assert_eq!(status, SignatureStatus::NotSigned);
    }

    #[test]
    fn unimplemented_ses_signature_is_unsupported_not_invalid() {
        // 关键：在尚未完成 SES 规范验签前，不能把无法解析的真实格式误判为篡改。
        let ofd = build_ofd(&[
            ("OFD.xml", b"<OFD/>"),
            ("Doc_0/invoice.xml", b"<Invoice><Fphm>1</Fphm></Invoice>"),
            (
                "Doc_0/Signs/Sign_0/SignedValue.dat",
                b"not-a-real-signature",
            ),
        ]);
        let status = verify_ofd_signature(&ofd, Path::new("x.ofd")).unwrap();
        assert!(
            matches!(status, SignatureStatus::Unsupported { .. }),
            "未支持的签章应判 Unsupported，实际 {status:?}"
        );
    }

    #[test]
    fn signed_xml_is_unsupported_instead_of_false_invalid() {
        let xml =
            br#"<Invoice xmlns:ds="urn:ds"><ds:SignatureValue>abc</ds:SignatureValue></Invoice>"#;
        let status = verify_xml_signature(xml, Path::new("x.xml")).unwrap();
        assert!(matches!(status, SignatureStatus::Unsupported { .. }));
    }

    #[test]
    fn unsigned_xml_reports_not_signed() {
        let status = verify_xml_signature(b"<Invoice/>", Path::new("x.xml")).unwrap();
        assert_eq!(status, SignatureStatus::NotSigned);
    }

    #[test]
    fn non_zip_input_errors() {
        let err = locate_signature(b"not a zip").unwrap_err();
        assert!(matches!(err, ParseError::MalformedFormat { .. }));
    }
}
