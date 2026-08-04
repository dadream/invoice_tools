use crate::extract::{ExtractedEmail, RawAttachment};

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
    "发票", "行程单", "结算单", "invoice", "fapiao", "电子客票", "报销凭证",
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
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") || lower.ends_with(".png") {
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
        if platform == Some("didi") || platform == Some("meituan")
            || platform == Some("amap") {
            return Some(SampleFormat::PdfVat);
        }
        Some(SampleFormat::PdfFlight)
    } else {
        Some(SampleFormat::PdfVat)
    }
}

fn is_image_extension(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    lower.ends_with(".jpg") || lower.ends_with(".jpeg")
        || lower.ends_with(".png") || lower.ends_with(".gif")
}

/// 判断一个附件是否为发票，并推断其格式。
pub fn classify_attachment(
    email: &ExtractedEmail,
    att: &RawAttachment,
) -> Option<Classification> {
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
        let c = classify_attachment(&email, &att("航空运输电子客票行程单.pdf", "application/pdf"))
            .unwrap();
        assert_eq!(c.format, SampleFormat::PdfFlight);
    }

    #[test]
    fn generic_invoice_pdf_falls_back_to_vat() {
        let email = email_from("billing@hotel.com", "发票");
        let c = classify_attachment(&email, &att("增值税电子普通发票.pdf", "application/pdf"))
            .unwrap();
        assert_eq!(c.format, SampleFormat::PdfVat);
    }

    #[test]
    fn image_attachment_is_classified_as_image() {
        let email = email_from("me@qq.com", "发票照片");
        // Use a large enough image (>50KB) to pass the size filter
        let c = classify_attachment(&email, &att_with_size("发票.jpg", "image/jpeg", 60_000)).unwrap();
        assert_eq!(c.format, SampleFormat::Image);
    }

    #[test]
    fn unrelated_attachment_from_unknown_sender_is_rejected() {
        let email = email_from("colleague@corp.com", "周报");
        assert!(classify_attachment(&email, &att("weekly-report.pdf", "application/pdf")).is_none());
    }

    #[test]
    fn image_from_unknown_sender_without_invoice_keyword_is_rejected() {
        // 避免把随便一张图片当发票
        let email = email_from("friend@qq.com", "旅游照片");
        assert!(classify_attachment(&email, &att("IMG_1234.jpg", "image/jpeg")).is_none());
    }
}
