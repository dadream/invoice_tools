use crate::extract::{ExtractedEmail, RawAttachment};
use quick_xml::events::Event;
use std::io::Cursor;

const MAX_STRUCTURE_ENTRIES: usize = 10_000;
const MAX_STRUCTURE_TOTAL_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentStructure {
    Valid { detected_mime: &'static str },
    Invalid { reason: &'static str },
    Unsupported,
}

/// 与解析验证计划 manifest.toml 的 format 字段取值一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    Xml,
    Ofd,
    PdfRail,
    PdfFlight,
    PdfVat,
    Image,
}

impl SampleFormat {
    /// 写入 manifest.toml 的字符串值
    pub fn as_manifest_str(&self) -> &'static str {
        match self {
            SampleFormat::Xml => "xml",
            SampleFormat::Ofd => "ofd",
            SampleFormat::PdfRail => "pdf-rail",
            SampleFormat::PdfFlight => "pdf-flight",
            SampleFormat::PdfVat => "pdf-vat",
            SampleFormat::Image => "image",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            SampleFormat::Xml => "xml",
            SampleFormat::Ofd => "ofd",
            SampleFormat::PdfRail | SampleFormat::PdfFlight | SampleFormat::PdfVat => "pdf",
            SampleFormat::Image => "jpg",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchReason {
    SenderWhitelist,
    AttachmentFeature,
    SupportedDocumentContent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub format: SampleFormat,
    pub platform: String,
    pub reason: MatchReason,
}

/// 发件人域名白名单。产品方案要求这份列表可云端更新，
/// MVP 阶段先内置 —— 本采集工具只需覆盖常见平台。
const SENDER_WHITELIST: &[(&str, &str)] = &[
    ("12306.cn", "12306"),
    ("rails.com.cn", "12306"),
    ("rail.sina.com.cn", "12306"),
    ("ctrip.com", "ctrip"),
    ("trip.com", "ctrip"),
    ("fliggy.com", "fliggy"),
    ("taobao.com", "fliggy"),
    ("ly.com", "tongcheng"),
    ("didiglobal.com", "didi"),
    ("xiaojukeji.com", "didi"),
    ("amap.com", "amap"),
    ("meituan.com", "meituan"),
    ("dianping.com", "meituan"),
    ("huazhu.com", "huazhu"),
    ("jinjiang.com", "jinjiang"),
    ("marriott.com", "marriott"),
    ("hilton.com", "hilton"),
    ("csair.com", "csair"),
    ("ceair.com", "ceair"),
    ("airchina.com", "airchina"),
    ("juneyaoair.com", "juneyao"),
    ("chinatax.gov.cn", "tax"),
    ("tax.gov.cn", "tax"),
];

/// 附件名/主题里出现这些词，视为发票相关
const INVOICE_KEYWORDS: &[&str] = &[
    "发票",
    "行程单",
    "结算单",
    "invoice",
    "fapiao",
    "电子客票",
    "报销凭证",
];

/// 发件人域名 → 平台标识。用于文件命名和统计。
pub fn platform_of_sender(from: &str) -> Option<&'static str> {
    let lower = from.to_lowercase();

    // Find the '@' symbol and extract the domain portion
    let domain_start = lower.find('@')?;
    let email_domain = &lower[domain_start + 1..];

    SENDER_WHITELIST
        .iter()
        .find(|(domain, _)| email_domain.ends_with(domain))
        .map(|(_, platform)| *platform)
}

fn has_invoice_keyword(text: &str) -> bool {
    let lower = text.to_lowercase();
    INVOICE_KEYWORDS
        .iter()
        .any(|kw| lower.contains(&kw.to_lowercase()))
}

/// 按扩展名和平台推断格式。
/// PDF 需要进一步区分铁路/航空/增值税 —— 用平台和关键词判断。
fn infer_format(filename: &str, platform: Option<&str>, subject: &str) -> Option<SampleFormat> {
    let lower = filename.to_lowercase();

    if lower.ends_with(".xml") {
        return Some(SampleFormat::Xml);
    }
    if lower.ends_with(".ofd") {
        return Some(SampleFormat::Ofd);
    }
    if lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".png")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
    {
        return Some(SampleFormat::Image);
    }
    if !lower.ends_with(".pdf") {
        return None;
    }

    // PDF 细分。平台优先，其次看文件名和主题里的关键词。
    let haystack = format!("{filename} {subject}").to_lowercase();
    match platform {
        Some("12306") => return Some(SampleFormat::PdfRail),
        Some("csair") | Some("ceair") | Some("airchina") | Some("juneyao") => {
            return Some(SampleFormat::PdfFlight)
        }
        _ => {}
    }
    if haystack.contains("客票") || haystack.contains("火车") || haystack.contains("铁路") {
        Some(SampleFormat::PdfRail)
    } else if haystack.contains("航空") || haystack.contains("机票") || haystack.contains("行程单")
    {
        // Ride-hailing platforms never issue flight itineraries
        if platform == Some("didi") || platform == Some("meituan") || platform == Some("amap") {
            return Some(SampleFormat::PdfVat);
        }
        Some(SampleFormat::PdfFlight)
    } else {
        Some(SampleFormat::PdfVat)
    }
}

fn is_image_extension(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".png")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
        || lower.ends_with(".gif")
        || lower.ends_with(".tif")
        || lower.ends_with(".tiff")
}

fn validate_pdf(data: &[u8]) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lopdf::Document::load_mem(data)
            .map(|document| !document.get_pages().is_empty())
            .unwrap_or(false)
    }))
    .unwrap_or(false)
}

fn validate_ofd(data: &[u8]) -> bool {
    let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(data)) else {
        return false;
    };
    if archive.is_empty() || archive.len() > MAX_STRUCTURE_ENTRIES {
        return false;
    }
    let mut has_root = false;
    let mut total = 0u64;
    for index in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(index) else {
            return false;
        };
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry.enclosed_name() else {
            return false;
        };
        has_root |= name
            .to_string_lossy()
            .replace('\\', "/")
            .eq_ignore_ascii_case("OFD.xml");
        total = total.saturating_add(entry.size());
        if total > MAX_STRUCTURE_TOTAL_BYTES {
            return false;
        }
        let mut sink = std::io::sink();
        if std::io::copy(&mut entry, &mut sink).is_err() {
            return false;
        }
    }
    has_root
}

fn validate_xml(data: &[u8]) -> bool {
    let mut reader = quick_xml::Reader::from_reader(data);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut has_element = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(_) | Event::Empty(_)) => has_element = true,
            Ok(Event::Eof) => return has_element,
            Ok(_) => {}
            Err(_) => return false,
        }
        buffer.clear();
    }
}

fn detected_image_mime(data: &[u8]) -> Option<&'static str> {
    let format = image::guess_format(data).ok()?;
    let (width, height) = image::ImageReader::with_format(Cursor::new(data), format)
        .into_dimensions()
        .ok()?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 40_000_000 {
        return None;
    }
    match format {
        image::ImageFormat::Png => Some("image/png"),
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        image::ImageFormat::WebP => Some("image/webp"),
        image::ImageFormat::Bmp => Some("image/bmp"),
        image::ImageFormat::Gif => Some("image/gif"),
        image::ImageFormat::Tiff => Some("image/tiff"),
        _ => None,
    }
}

/// 收集阶段只做文件可打开性检查，不读取票号、金额等业务字段。
pub fn validate_attachment_structure(att: &RawAttachment) -> AttachmentStructure {
    let extension = std::path::Path::new(&att.filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "pdf" => {
            if validate_pdf(&att.data) {
                AttachmentStructure::Valid {
                    detected_mime: "application/pdf",
                }
            } else {
                AttachmentStructure::Invalid {
                    reason: "pdf_structure_invalid",
                }
            }
        }
        "ofd" => {
            if validate_ofd(&att.data) {
                AttachmentStructure::Valid {
                    detected_mime: "application/ofd",
                }
            } else {
                AttachmentStructure::Invalid {
                    reason: "ofd_structure_invalid",
                }
            }
        }
        "xml" => {
            if validate_xml(&att.data) {
                AttachmentStructure::Valid {
                    detected_mime: "application/xml",
                }
            } else {
                AttachmentStructure::Invalid {
                    reason: "xml_structure_invalid",
                }
            }
        }
        _ if is_image_extension(&att.filename) => detected_image_mime(&att.data)
            .map(|detected_mime| AttachmentStructure::Valid { detected_mime })
            .unwrap_or(AttachmentStructure::Invalid {
                reason: "image_structure_invalid",
            }),
        _ => AttachmentStructure::Unsupported,
    }
}

fn has_supported_document_content(att: &RawAttachment) -> bool {
    let lower = att.filename.to_lowercase();
    if lower.ends_with(".pdf") {
        return att
            .data
            .windows(5)
            .take(1024)
            .any(|window| window == b"%PDF-");
    }
    if lower.ends_with(".ofd") {
        return matches!(
            att.data.get(..4),
            Some(b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08")
        );
    }
    if lower.ends_with(".xml") {
        let prefix = att.data.get(..att.data.len().min(1024)).unwrap_or_default();
        let prefix = prefix.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(prefix);
        return prefix
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace())
            == Some(b'<');
    }
    false
}

/// 判断一个附件是否为发票，并推断其格式。
pub fn classify_attachment(email: &ExtractedEmail, att: &RawAttachment) -> Option<Classification> {
    // Reject small images (likely decorations, not scanned invoices)
    if is_image_extension(&att.filename) && att.data.len() < 50_000 {
        return None;
    }

    let platform = platform_of_sender(&email.from);

    // 第 1 级：发件人在白名单 —— 该发件人的具名附件直接采信
    if let Some(p) = platform {
        let format = infer_format(&att.filename, Some(p), &email.subject)?;
        return Some(Classification {
            format,
            platform: p.to_string(),
            reason: MatchReason::SenderWhitelist,
        });
    }

    // 第 2 级：附件特征 —— 文件名或主题含发票关键词
    if has_invoice_keyword(&att.filename) || has_invoice_keyword(&email.subject) {
        let format = infer_format(&att.filename, None, &email.subject)?;
        return Some(Classification {
            format,
            platform: "unknown".to_string(),
            reason: MatchReason::AttachmentFeature,
        });
    }

    // 真实邮箱中存在大量由用户转发、企业邮件网关改写，或使用随机文件名的发票。
    // 对内容魔数确认的 PDF/OFD/XML 保持高召回；无效结构由后续独立结构校验和解析层拒绝。
    // 图片仍要求白名单/关键词和大小门槛，避免把签名图、二维码与 Logo 全部送入 OCR。
    if has_supported_document_content(att) {
        let format = infer_format(&att.filename, None, &email.subject)?;
        return Some(Classification {
            format,
            platform: "unknown".to_string(),
            reason: MatchReason::SupportedDocumentContent,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email_from(sender: &str, subject: &str) -> ExtractedEmail {
        ExtractedEmail {
            message_id: Some("id@x".into()),
            subject: subject.into(),
            from: sender.into(),
            attachments: vec![],
            invoice_link_hint: false,
            invoice_notice_hint: false,
            attachment_qr_link_hint: false,
        }
    }

    fn att(filename: &str, content_type: &str) -> RawAttachment {
        RawAttachment {
            filename: filename.into(),
            content_type: content_type.into(),
            data: b"%PDF-1.4".to_vec(),
        }
    }

    fn att_with_size(filename: &str, content_type: &str, size: usize) -> RawAttachment {
        RawAttachment {
            filename: filename.into(),
            content_type: content_type.into(),
            data: vec![0u8; size],
        }
    }

    #[test]
    fn recognizes_12306_as_rail_platform() {
        assert_eq!(platform_of_sender("noreply@12306.cn"), Some("12306"));
    }

    #[test]
    fn recognizes_ctrip_variants() {
        assert_eq!(platform_of_sender("invoice@ctrip.com"), Some("ctrip"));
        assert_eq!(platform_of_sender("fapiao@trip.com"), Some("ctrip"));
    }

    #[test]
    fn unknown_sender_has_no_platform() {
        assert_eq!(platform_of_sender("someone@random.org"), None);
    }

    #[test]
    fn rail_pdf_from_12306_is_classified_as_rail() {
        let email = email_from("noreply@12306.cn", "您的电子发票");
        let c = classify_attachment(&email, &att("电子客票.pdf", "application/pdf")).unwrap();
        assert_eq!(c.format, SampleFormat::PdfRail);
        assert_eq!(c.platform, "12306");
        assert_eq!(c.reason, MatchReason::SenderWhitelist);
    }

    #[test]
    fn xml_attachment_is_classified_as_xml_regardless_of_sender() {
        let email = email_from("unknown@nowhere.com", "发票");
        let c = classify_attachment(&email, &att("发票.xml", "application/xml")).unwrap();
        assert_eq!(c.format, SampleFormat::Xml);
        assert_eq!(c.reason, MatchReason::AttachmentFeature);
    }

    #[test]
    fn ofd_attachment_is_classified_as_ofd() {
        let email = email_from("unknown@nowhere.com", "发票");
        let c = classify_attachment(&email, &att("发票.ofd", "application/octet-stream")).unwrap();
        assert_eq!(c.format, SampleFormat::Ofd);
    }

    #[test]
    fn flight_itinerary_detected_by_filename_keyword() {
        let email = email_from("noreply@csair.com", "行程单");
        let c = classify_attachment(
            &email,
            &att("航空运输电子客票行程单.pdf", "application/pdf"),
        )
        .unwrap();
        assert_eq!(c.format, SampleFormat::PdfFlight);
    }

    #[test]
    fn generic_invoice_pdf_falls_back_to_vat() {
        let email = email_from("billing@hotel.com", "发票");
        let c =
            classify_attachment(&email, &att("增值税电子普通发票.pdf", "application/pdf")).unwrap();
        assert_eq!(c.format, SampleFormat::PdfVat);
    }

    #[test]
    fn image_attachment_is_classified_as_image() {
        let email = email_from("me@qq.com", "发票照片");
        // Use a large enough image (>50KB) to pass the size filter
        let c =
            classify_attachment(&email, &att_with_size("发票.jpg", "image/jpeg", 60_000)).unwrap();
        assert_eq!(c.format, SampleFormat::Image);
    }

    #[test]
    fn magic_verified_pdf_from_unknown_sender_is_kept_for_high_recall() {
        let email = email_from("colleague@corp.com", "周报");
        assert_eq!(
            classify_attachment(&email, &att("weekly-report.pdf", "application/pdf"))
                .unwrap()
                .reason,
            MatchReason::SupportedDocumentContent
        );
    }

    #[test]
    fn unsupported_unrelated_attachment_is_rejected() {
        let email = email_from("colleague@corp.com", "周报");
        assert!(classify_attachment(&email, &att("weekly-report.txt", "text/plain")).is_none());
    }

    #[test]
    fn image_from_unknown_sender_without_invoice_keyword_is_rejected() {
        // 避免把随便一张图片当发票
        let email = email_from("friend@qq.com", "旅游照片");
        assert!(classify_attachment(&email, &att("IMG_1234.jpg", "image/jpeg")).is_none());
    }

    #[test]
    fn accepts_magic_verified_pdf_without_sender_or_filename_keywords() {
        let email = email_from("forwarded@qq.com", "document");
        let attachment = att("4f9a8c.pdf", "application/octet-stream");
        let classification = classify_attachment(&email, &attachment).unwrap();
        assert_eq!(classification.format, SampleFormat::PdfVat);
        assert_eq!(classification.reason, MatchReason::SupportedDocumentContent);
    }

    #[test]
    fn rejects_fake_pdf_without_pdf_magic() {
        let email = email_from("forwarded@qq.com", "document");
        let mut attachment = att("4f9a8c.pdf", "application/pdf");
        attachment.data = b"not really a PDF".to_vec();
        assert!(classify_attachment(&email, &attachment).is_none());
    }

    #[test]
    fn structure_validation_rejects_truncated_pdf_even_with_pdf_magic() {
        let attachment = att("invoice.pdf", "application/pdf");
        assert_eq!(
            validate_attachment_structure(&attachment),
            AttachmentStructure::Invalid {
                reason: "pdf_structure_invalid"
            }
        );
    }

    #[test]
    fn structure_validation_accepts_well_formed_xml_and_corrects_mime() {
        let mut attachment = att("invoice.xml", "text/plain");
        attachment.data =
            b"<?xml version=\"1.0\"?><Invoice><Total>10.00</Total></Invoice>".to_vec();
        assert_eq!(
            validate_attachment_structure(&attachment),
            AttachmentStructure::Valid {
                detected_mime: "application/xml"
            }
        );
    }

    #[test]
    fn accepts_bom_prefixed_xml_without_keywords() {
        let email = email_from("forwarded@qq.com", "document");
        let mut attachment = att("4f9a8c.xml", "application/octet-stream");
        attachment.data = b"\xef\xbb\xbf  <Invoice/>".to_vec();
        assert_eq!(
            classify_attachment(&email, &attachment).unwrap().reason,
            MatchReason::SupportedDocumentContent
        );
    }

    #[test]
    #[ignore = "requires an explicitly authorized private capture outside the Git repository"]
    fn reclassifies_private_capture_without_logging_message_fields() {
        let capture_root = std::env::var_os("INVOICE_REAL_ALL_CAPTURE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("INVOICE_REAL_ALL_CAPTURE_ROOT must be set");
        let capture_root = std::fs::canonicalize(capture_root)
            .expect("private all-attachments capture root must exist");
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("invoice-collect must have a repository root")
            .canonicalize()
            .expect("repository root must be accessible");
        assert!(
            !capture_root.starts_with(repo_root),
            "private capture must stay outside the Git repository"
        );

        let mut emails = std::fs::read_dir(capture_root.join("emails"))
            .expect("private email directory must be readable")
            .map(|entry| entry.expect("private email entry must be readable").path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("eml"))
            .collect::<Vec<_>>();
        emails.sort();
        let mut rows =
            vec!["sha256\tpredicted_invoice\tpredicted_format\tmatch_reason".to_string()];
        let candidates_root = capture_root
            .join("reclassified-parse")
            .join("fixtures")
            .join("samples");
        std::fs::create_dir_all(&candidates_root)
            .expect("private reclassified candidate directory must be creatable");
        let mut saved_candidates = std::collections::HashSet::<String>::new();
        let mut positives = 0usize;
        let mut negatives = 0usize;
        for path in emails {
            let raw = std::fs::read(path).expect("private email must be readable");
            let email = crate::extract::extract_email(&raw).expect("private email must parse");
            for attachment in &email.attachments {
                for item in crate::extract::extract_zip_if_needed(attachment) {
                    let hash = crate::dedupe::sha256_hex(&item.data);
                    let classification = classify_attachment(&email, &item);
                    let (predicted, format, reason) = match classification {
                        Some(value) => {
                            positives += 1;
                            if saved_candidates.insert(hash.clone()) {
                                let candidate_path = candidates_root
                                    .join(format!("candidate-{hash}.{}", value.format.extension()));
                                if candidate_path.exists() {
                                    assert_eq!(
                                        std::fs::read(&candidate_path)
                                            .expect("existing private candidate must be readable"),
                                        item.data,
                                        "existing private candidate content changed"
                                    );
                                } else {
                                    std::fs::write(&candidate_path, &item.data)
                                        .expect("private candidate must remain outside Git");
                                }
                            }
                            (
                                true,
                                value.format.as_manifest_str(),
                                match value.reason {
                                    MatchReason::SenderWhitelist => "sender_whitelist",
                                    MatchReason::AttachmentFeature => "attachment_feature",
                                    MatchReason::SupportedDocumentContent => {
                                        "supported_document_content"
                                    }
                                },
                            )
                        }
                        None => {
                            negatives += 1;
                            (false, "", "")
                        }
                    };
                    rows.push(format!("{hash}\t{predicted}\t{format}\t{reason}"));
                }
            }
        }
        assert!(
            rows.len() > 1,
            "private capture contains no logical attachments"
        );
        std::fs::write(
            capture_root.join("reclassified.private.tsv"),
            rows.join("\n"),
        )
        .expect("private reclassification must remain in private capture root");
        println!("verification=private-offline-reclassification-v1");
        println!("logical_attachments={}", positives + negatives);
        println!("classifier_positive={positives}");
        println!("classifier_negative={negatives}");
        println!("unique_candidate_files={}", saved_candidates.len());
        println!("private_fields_logged=false");
    }

    #[test]
    #[ignore = "requires an explicitly authorized private capture outside the Git repository"]
    fn validates_private_link_only_hints_offline_without_logging_message_fields() {
        let capture_root = std::env::var_os("INVOICE_REAL_ALL_CAPTURE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("INVOICE_REAL_ALL_CAPTURE_ROOT must be set");
        let capture_root = std::fs::canonicalize(capture_root)
            .expect("private all-attachments capture root must exist");
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("invoice-collect must have a repository root")
            .canonicalize()
            .expect("repository root must be accessible");
        assert!(
            !capture_root.starts_with(&repo_root),
            "private capture must stay outside the Git repository"
        );

        let truth_path = capture_root.join("email-ground-truth-final.private.tsv");
        let truth = std::fs::read_to_string(&truth_path)
            .expect("private email truth table must be readable");
        let mut lines = truth.lines();
        let headers = lines
            .next()
            .expect("private email truth table must contain headers")
            .split('\t')
            .collect::<Vec<_>>();
        let email_file_index = headers
            .iter()
            .position(|header| *header == "email_file")
            .expect("truth table must contain email_file");
        let human_label_index = headers
            .iter()
            .position(|header| *header == "human_label")
            .expect("truth table must contain human_label");

        let emails_root = capture_root.join("emails");
        let canonical_emails_root = emails_root
            .canonicalize()
            .expect("private email directory must be accessible");
        let mut label_counts = std::collections::BTreeMap::<String, usize>::new();
        let mut email_count = 0usize;
        let mut link_only_actual = 0usize;
        let mut link_only_detected = 0usize;
        let mut other_labels_false_positive = 0usize;

        for line in lines.filter(|line| !line.trim().is_empty()) {
            let columns = line.split('\t').collect::<Vec<_>>();
            assert_eq!(
                columns.len(),
                headers.len(),
                "private truth table row has an invalid column count"
            );
            let email_file = columns[email_file_index];
            let human_label = columns[human_label_index];
            assert!(
                !email_file.contains(['/', '\\']) && email_file.ends_with(".eml"),
                "private email filename must be a safe basename"
            );
            let email_path = emails_root
                .join(email_file)
                .canonicalize()
                .expect("private email file must be accessible");
            assert!(
                email_path.starts_with(&canonical_emails_root),
                "private email path escaped its authorized root"
            );

            let raw = std::fs::read(email_path).expect("private email must be readable");
            let email = crate::extract::extract_email(&raw).expect("private email must parse");
            let has_accepted_candidate = email.attachments.iter().any(|attachment| {
                crate::extract::extract_zip_if_needed(attachment)
                    .iter()
                    .any(|item| classify_attachment(&email, item).is_some())
            });
            let predicted_link_only = email.invoice_link_hint && !has_accepted_candidate;

            email_count += 1;
            *label_counts.entry(human_label.to_string()).or_default() += 1;
            if human_label == "invoice_link_only" {
                link_only_actual += 1;
                if predicted_link_only {
                    link_only_detected += 1;
                }
            } else if predicted_link_only {
                other_labels_false_positive += 1;
            }
        }

        println!("verification=private-link-hint-v1");
        println!("emails={email_count}");
        println!("labels={label_counts:?}");
        println!("invoice_link_only_actual={link_only_actual}");
        println!("invoice_link_only_detected={link_only_detected}");
        println!("other_labels_false_positive={other_labels_false_positive}");
        println!("automatic_url_access=false");
        println!("network_requests=0");
        println!("private_fields_logged=false");

        assert_eq!(email_count, 69, "private truth table email count changed");
        assert_eq!(link_only_actual, 3, "link-only truth count changed");
        assert_eq!(
            link_only_detected, link_only_actual,
            "not every human-labelled link-only email was detected"
        );
        assert_eq!(
            other_labels_false_positive, 0,
            "a non-link-only email was incorrectly flagged"
        );
    }
}
