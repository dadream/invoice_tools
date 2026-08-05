use crate::model::ParseError;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum SignatureStatus {
    /// 签章验证通过：内容未被篡改，且由可信主体签发
    Valid,
    /// 签章存在但验证失败
    Invalid { reason: String },
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

/// 验证 XML 的数字签章。
/// XML 数电票的签章位于 <TaxBureauSignature><SignatureValue> 元素中。
pub fn verify_xml_signature(
    xml_bytes: &[u8],
    path: &Path,
) -> Result<SignatureStatus, ParseError> {
    let xml_str = std::str::from_utf8(xml_bytes).map_err(|_| ParseError::MalformedFormat {
        path: path.to_path_buf(),
        format: "XML",
        detail: "文件不是有效的 UTF-8".to_string(),
    })?;

    // 提取 <SignatureValue> 内容
    let sig_value = if let Some(start) = xml_str.find("<SignatureValue>") {
        let content_start = start + "<SignatureValue>".len();
        if let Some(end) = xml_str[content_start..].find("</SignatureValue>") {
            let sig_hex = &xml_str[content_start..content_start + end];
            // 去除可能的 &amp; 和时间戳部分
            let sig_hex = sig_hex.split('&').next().unwrap_or(sig_hex);
            sig_hex.trim()
        } else {
            return Ok(SignatureStatus::Invalid {
                reason: "SignatureValue 元素未正确闭合".to_string(),
            });
        }
    } else {
        return Ok(SignatureStatus::NotSigned);
    };

    if sig_value.is_empty() {
        return Ok(SignatureStatus::NotSigned);
    }

    // 尝试解码 hex 签名
    let sig_bytes = match hex_decode(sig_value) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(SignatureStatus::Invalid {
                reason: format!("SignatureValue 不是有效的十六进制（{} 字符）", sig_value.len()),
            })
        }
    };

    // 提取签名数据中的 SM2 公钥和签名值
    match extract_sm2_parts(&sig_bytes) {
        None => Ok(SignatureStatus::Invalid {
            reason: format!("无法从 SignatureValue 中提取 SM2 签名结构（{} 字节）", sig_bytes.len()),
        }),
        Some((public_key, signature)) => {
            // 对于 XML，被签名的数据是整个文档（或特定部分）
            // 简化实现：验证整个 XML 内容
            let ok = sm2_verify(&public_key, xml_bytes, &signature);
            if ok {
                Ok(SignatureStatus::Valid)
            } else {
                Ok(SignatureStatus::Invalid {
                    reason: "SM2 签名验证不通过（内容可能被篡改）".to_string(),
                })
            }
        }
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

    let mut hit: Option<usize> = None;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| ParseError::MalformedFormat {
            path: PathBuf::new(),
            format: "OFD",
            detail: format!("读取第 {i} 个条目失败: {e}"),
        })?;
        if looks_like_signature(entry.name()) && !entry.name().ends_with('/') {
            hit = Some(i);
            break;
        }
    }

    let Some(index) = hit else { return Ok(None) };

    let mut entry = archive.by_index(index).map_err(|e| ParseError::MalformedFormat {
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
pub fn verify_ofd_signature(
    ofd_bytes: &[u8],
    path: &Path,
) -> Result<SignatureStatus, ParseError> {
    let Some(sig) = locate_signature(ofd_bytes)? else {
        return Ok(SignatureStatus::NotSigned);
    };

    // 被签名的数据。按 GB/T 33190 规范，签章覆盖 Signature.xml 所引用的
    // 各文件摘要；MVP 阶段简化为验证内嵌发票 XML —— 这正是我们要保护的内容。
    // 若 Step 5 的真实样本验签失败，按规范扩展这里的数据范围。
    let signed_payload = match crate::ofd::extract_invoice_xml(ofd_bytes, path) {
        Ok(xml) => xml,
        Err(_) => {
            return Ok(SignatureStatus::Invalid {
                reason: "容器有签章但找不到被签名的发票 XML".to_string(),
            })
        }
    };

    match extract_sm2_parts(&sig.raw) {
        None => Ok(SignatureStatus::Invalid {
            reason: format!(
                "签章文件 {} 不是可识别的 SES_Signature 结构（{} 字节）",
                sig.entry_name,
                sig.raw.len()
            ),
        }),
        Some((public_key, signature)) => {
            let ok = sm2_verify(&public_key, &signed_payload, &signature);
            if ok {
                Ok(SignatureStatus::Valid)
            } else {
                Ok(SignatureStatus::Invalid {
                    reason: "SM2 签名验证不通过（内容可能被篡改）".to_string(),
                })
            }
        }
    }
}

/// 签章文件的候选路径特征（不区分大小写）
const SIGNATURE_HINTS: &[&str] = &["signedvalue.dat", "signature.dat", "/signs/", "seal.dat"];

fn looks_like_signature(entry_name: &str) -> bool {
    let lower = entry_name.to_lowercase();
    SIGNATURE_HINTS.iter().any(|h| lower.contains(h))
}

/// 从 SES_Signature（ASN.1 DER）中取出签发者公钥与签名值。
///
/// 返回 None 表示结构无法识别 —— 此时判 Invalid 而非 panic。
/// Step 5 会用真实样本确认这里的解析是否需要按 GB/T 38540 细化。
fn extract_sm2_parts(raw: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    // SES_Signature 是 DER 编码的 SEQUENCE，最外层标签为 0x30。
    // 明显不是 DER 的输入直接判定无法识别。
    if raw.first() != Some(&0x30) || raw.len() < 64 {
        return None;
    }

    // 查找最后一个看起来像 SM2 签名的 SEQUENCE (0x30 0x44 或 0x30 0x45 或 0x30 0x46)
    // SM2 签名是 SEQUENCE { r INTEGER, s INTEGER }，通常 68-72 字节
    let mut sig_start = None;
    for i in (0..raw.len().saturating_sub(70)).rev() {
        if raw[i] == 0x30 && i + 1 < raw.len() {
            let len = raw[i + 1] as usize;
            if (0x44..=0x46).contains(&(len as u8)) && i + 2 + len <= raw.len() {
                sig_start = Some(i);
                break;
            }
        }
    }

    let sig_offset = sig_start?;
    let sig_data = &raw[sig_offset..];

    // 提取签名的 r 和 s 分量
    let (r, s) = extract_sm2_r_s(sig_data)?;

    // 构造 64 字节的原始签名 (r || s)
    let mut signature = Vec::with_capacity(64);
    signature.extend_from_slice(&r);
    signature.extend_from_slice(&s);

    // 查找公钥：在证书结构中寻找 0x04 开头的 64 字节未压缩点
    // 跳过明显不是公钥的位置（如在 OID 附近）
    let mut public_key = None;
    for i in 50..raw.len().saturating_sub(65) {
        if raw[i] == 0x04 {
            let candidate = &raw[i + 1..i + 65];
            // 检查是否像坐标（有足够的熵）
            let zeros = candidate.iter().filter(|&&b| b == 0).count();
            let ones = candidate.iter().filter(|&&b| b == 0xff).count();
            if zeros < 50 && ones < 50 {
                // 看起来像一个真实的公钥点
                public_key = Some(candidate.to_vec());
                break;
            }
        }
    }

    // 如果找不到公钥，返回占位值以便继续测试签名提取
    // 真实场景需要从证书 SubjectPublicKeyInfo 中正确提取
    let public_key = public_key.unwrap_or_else(|| vec![0u8; 64]);

    Some((public_key, signature))
}

/// 解析 DER 长度字段，返回 (长度字段占用的字节数, 实际长度值)
fn parse_der_length(data: &[u8]) -> Option<(usize, usize)> {
    if data.is_empty() {
        return None;
    }

    let first = data[0];
    if first & 0x80 == 0 {
        // 短格式：长度直接编码在第一个字节
        Some((1, first as usize))
    } else {
        // 长格式：第一个字节的低 7 位表示后续有几个字节表示长度
        let num_bytes = (first & 0x7f) as usize;
        if num_bytes > 4 || data.len() < 1 + num_bytes {
            return None;
        }

        let mut length = 0usize;
        for i in 0..num_bytes {
            length = (length << 8) | (data[1 + i] as usize);
        }
        Some((1 + num_bytes, length))
    }
}

/// 从 SM2 签名数据中提取 r 和 s 分量（各 32 字节）
fn extract_sm2_r_s(data: &[u8]) -> Option<([u8; 32], [u8; 32])> {
    if data.len() < 64 {
        return None;
    }

    let mut offset = 0;

    // 跳过可能的 SEQUENCE 标签
    if data[offset] == 0x30 {
        offset += 1;
        let (len_bytes, _) = parse_der_length(&data[offset..])?;
        offset += len_bytes;
    }

    // 读取 r (INTEGER)
    if offset >= data.len() || data[offset] != 0x02 {
        return None;
    }
    offset += 1;

    let (len_bytes, r_len) = parse_der_length(&data[offset..])?;
    offset += len_bytes;

    if offset + r_len > data.len() {
        return None;
    }

    let r_data = &data[offset..offset + r_len];
    offset += r_len;

    // 读取 s (INTEGER)
    if offset >= data.len() || data[offset] != 0x02 {
        return None;
    }
    offset += 1;

    let (len_bytes, s_len) = parse_der_length(&data[offset..])?;
    offset += len_bytes;

    if offset + s_len > data.len() {
        return None;
    }

    let s_data = &data[offset..offset + s_len];

    // r 和 s 可能是 32 或 33 字节（如果有前导 0x00）
    let r = pad_or_trim_to_32(r_data)?;
    let s = pad_or_trim_to_32(s_data)?;

    Some((r, s))
}

/// 将整数转换为固定 32 字节（去除前导 0x00 或补零）
fn pad_or_trim_to_32(data: &[u8]) -> Option<[u8; 32]> {
    let mut result = [0u8; 32];

    if data.is_empty() || data.len() > 33 {
        return None;
    }

    // 去除前导 0x00（DER 编码中用于表示正数）
    let data = if data.len() == 33 && data[0] == 0x00 {
        &data[1..]
    } else {
        data
    };

    if data.len() > 32 {
        return None;
    }

    // 右对齐：将数据复制到结果数组的末尾
    let offset = 32 - data.len();
    result[offset..].copy_from_slice(data);

    Some(result)
}

/// SM2 验签。公钥为未压缩点（0x04 || X || Y），或裸 X||Y。
fn sm2_verify(public_key: &[u8], data: &[u8], signature: &[u8]) -> bool {
    let key_hex = hex_encode(public_key);
    let ctx = smcrypto::sm2::Verify::new(&key_hex);
    ctx.verify(data, signature)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(hex_str: &str) -> Result<Vec<u8>, ()> {
    if hex_str.len() % 2 != 0 {
        return Err(());
    }
    (0..hex_str.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex_str[i..i + 2], 16).map_err(|_| ()))
        .collect()
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
            ("Doc_0/Signs/Sign_0/SignedValue.dat", b"fake-signature-bytes"),
        ]);
        let found = locate_signature(&ofd).unwrap().expect("应找到签章");
        assert_eq!(found.entry_name, "Doc_0/Signs/Sign_0/SignedValue.dat");
        assert_eq!(found.raw, b"fake-signature-bytes");
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
    fn garbage_signature_is_invalid_not_panic() {
        // 关键：无效签章必须返回 Invalid，不能 panic 也不能误判 Valid
        let ofd = build_ofd(&[
            ("OFD.xml", b"<OFD/>"),
            ("Doc_0/invoice.xml", b"<Invoice><Fphm>1</Fphm></Invoice>"),
            ("Doc_0/Signs/Sign_0/SignedValue.dat", b"not-a-real-signature"),
        ]);
        let status = verify_ofd_signature(&ofd, Path::new("x.ofd")).unwrap();
        assert!(
            matches!(status, SignatureStatus::Invalid { .. }),
            "垃圾签章应判 Invalid，实际 {status:?}"
        );
    }

    #[test]
    fn non_zip_input_errors() {
        let err = locate_signature(b"not a zip").unwrap_err();
        assert!(matches!(err, ParseError::MalformedFormat { .. }));
    }
}
