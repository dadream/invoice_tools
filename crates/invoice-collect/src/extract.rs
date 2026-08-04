use anyhow::{Context, Result};
use mail_parser::MimeHeaders;

#[derive(Debug, Clone, PartialEq)]
pub struct RawAttachment {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedEmail {
    pub message_id: Option<String>,
    pub subject: String,
    pub from: String,
    pub attachments: Vec<RawAttachment>,
}

/// 解析一封原始邮件，取出头字段与所有具名附件。
///
/// 无 filename 的部件（正文、内联图片）一律跳过 —— 发票必然是具名附件。
pub fn extract_email(raw: &[u8]) -> Result<ExtractedEmail> {
    let message = mail_parser::MessageParser::default()
        .parse(raw)
        .context("邮件 MIME 结构无法解析")?;

    let message_id = message.message_id().map(|id| id.trim_matches(['<', '>']).to_string());

    let subject = message
        .subject()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("(无主题)")
        .to_string();

    let from = message
        .from()
        .and_then(|addrs| addrs.first())
        .and_then(|a| a.address())
        .unwrap_or("(未知发件人)")
        .to_string();

    let mut attachments = Vec::new();
    for part in message.attachments() {
        // 无 filename 的部件不是发票（正文、内联图片），跳过
        let Some(filename) = part.attachment_name() else {
            continue;
        };
        let filename = filename.trim();
        if filename.is_empty() {
            continue;
        }

        let content_type = part
            .content_type()
            .map(|ct| match ct.subtype() {
                Some(sub) => format!("{}/{}", ct.ctype(), sub),
                None => ct.ctype().to_string(),
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());

        attachments.push(RawAttachment {
            filename: filename.to_string(),
            content_type,
            data: part.contents().to_vec(),
        });
    }

    Ok(ExtractedEmail {
        message_id,
        subject,
        from,
        attachments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "%PDF-1.4" 的 base64，用作假附件内容
    const FAKE_PDF_B64: &str = "JVBERi0xLjQ=";
    const FAKE_PDF_BYTES: &[u8] = b"%PDF-1.4";

    fn eml_with_filename(filename_param: &str) -> Vec<u8> {
        format!(
            "From: noreply@12306.cn\r\n\
             To: test-user@qq.com\r\n\
             Subject: 电子发票\r\n\
             Message-ID: <abc123@12306.cn>\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"BOUND\"\r\n\
             \r\n\
             --BOUND\r\n\
             Content-Type: text/plain; charset=UTF-8\r\n\
             \r\n\
             您的发票已开出\r\n\
             --BOUND\r\n\
             Content-Type: application/pdf\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; {filename_param}\r\n\
             \r\n\
             {FAKE_PDF_B64}\r\n\
             --BOUND--\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn extracts_ascii_filename_and_decoded_body() {
        let eml = eml_with_filename(r#"filename="invoice.pdf""#);
        let email = extract_email(&eml).unwrap();

        assert_eq!(email.attachments.len(), 1);
        assert_eq!(email.attachments[0].filename, "invoice.pdf");
        assert_eq!(email.attachments[0].data, FAKE_PDF_BYTES);
    }

    #[test]
    fn decodes_rfc2047_utf8_filename() {
        let eml = eml_with_filename(r#"filename="=?UTF-8?B?5Y+R56Wo?=.pdf""#);
        let email = extract_email(&eml).unwrap();
        assert!(
            email.attachments[0].filename.contains("发票"),
            "实际文件名: {}",
            email.attachments[0].filename
        );
    }

    #[test]
    fn decodes_rfc2047_gb18030_filename() {
        let eml = eml_with_filename(r#"filename="=?GB18030?B?t6LGsQ==?=.pdf""#);
        let email = extract_email(&eml).unwrap();
        assert!(
            email.attachments[0].filename.contains("发票"),
            "实际文件名: {}",
            email.attachments[0].filename
        );
    }

    #[test]
    fn decodes_rfc2231_filename() {
        let eml = eml_with_filename("filename*=UTF-8''%E5%8F%91%E7%A5%A8.pdf");
        let email = extract_email(&eml).unwrap();
        assert!(
            email.attachments[0].filename.contains("发票"),
            "实际文件名: {}",
            email.attachments[0].filename
        );
    }

    #[test]
    fn extracts_message_id_and_sender() {
        let eml = eml_with_filename(r#"filename="a.pdf""#);
        let email = extract_email(&eml).unwrap();
        assert_eq!(email.message_id.as_deref(), Some("abc123@12306.cn"));
        assert_eq!(email.from, "noreply@12306.cn");
        assert_eq!(email.subject, "电子发票");
    }

    #[test]
    fn skips_parts_without_filename() {
        // 只有正文，没有具名附件
        let eml = b"From: a@b.com\r\n\
                    Subject: hi\r\n\
                    Content-Type: text/plain\r\n\
                    \r\n\
                    just text\r\n";
        let email = extract_email(eml).unwrap();
        assert!(email.attachments.is_empty());
    }

    #[test]
    fn extracts_multiple_attachments_in_one_email() {
        let eml = format!(
            "From: a@b.com\r\nSubject: two\r\n\
             Content-Type: multipart/mixed; boundary=\"B\"\r\n\r\n\
             --B\r\nContent-Type: application/pdf\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; filename=\"one.pdf\"\r\n\r\n\
             {FAKE_PDF_B64}\r\n\
             --B\r\nContent-Type: application/xml\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; filename=\"two.xml\"\r\n\r\n\
             {FAKE_PDF_B64}\r\n\
             --B--\r\n"
        )
        .into_bytes();

        let email = extract_email(&eml).unwrap();
        assert_eq!(email.attachments.len(), 2);
        assert_eq!(email.attachments[0].filename, "one.pdf");
        assert_eq!(email.attachments[1].filename, "two.xml");
    }

    #[test]
    fn missing_subject_falls_back_to_placeholder() {
        let eml = b"From: a@b.com\r\nContent-Type: text/plain\r\n\r\nx\r\n";
        let email = extract_email(eml).unwrap();
        assert_eq!(email.subject, "(无主题)");
    }
}
