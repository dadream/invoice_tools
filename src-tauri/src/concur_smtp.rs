//! Concur receipt delivery over the user's existing mailbox.
//!
//! This module never discovers recipients and never sends automatically. The command layer
//! must first persist a reviewed plan, require an explicit click, and pass the session-only
//! authorization code. SMTP failures after delivery starts are deliberately reported as
//! `OutcomeUnknown` so callers do not retry and accidentally duplicate receipts.

use std::time::Duration;

use lettre::message::{header::ContentType, Attachment, Mailbox, Message, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{SmtpTransport, Transport};

pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 5;
pub const MAX_ATTACHMENT_BYTES: usize = 15 * 1024 * 1024;
pub const MAX_MESSAGE_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
const SMTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmtpTlsMode {
    Wrapper,
    RequiredStartTls,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SmtpProvider {
    host: &'static str,
    port: u16,
    tls_mode: SmtpTlsMode,
}

#[derive(Debug, Clone)]
pub struct ReceiptAttachment {
    pub name: String,
    pub mime_type: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ReceiptMessage {
    pub sender_email: String,
    pub recipient_email: String,
    pub message_id: String,
    pub is_trial: bool,
    pub attachments: Vec<ReceiptAttachment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryErrorKind {
    BeforeSend,
    OutcomeUnknown,
}

#[derive(Debug, thiserror::Error)]
#[error("{safe_message}")]
pub struct DeliveryError {
    pub kind: DeliveryErrorKind,
    safe_message: &'static str,
}

trait ReceiptTransport {
    fn deliver(&self, message: &Message) -> Result<(), ()>;
}

impl ReceiptTransport for SmtpTransport {
    fn deliver(&self, message: &Message) -> Result<(), ()> {
        self.send(message).map(|_| ()).map_err(|_| ())
    }
}

impl DeliveryError {
    fn before_send(message: &'static str) -> Self {
        Self {
            kind: DeliveryErrorKind::BeforeSend,
            safe_message: message,
        }
    }

    fn outcome_unknown() -> Self {
        Self {
            kind: DeliveryErrorKind::OutcomeUnknown,
            safe_message: "SMTP 连接在发送阶段失败，是否送达未知；请先到 Concur 核对，勿直接重试",
        }
    }
}

/// The internal Alpha is built with this unset, which makes every send command fail closed
/// before reading an attachment or opening a socket.
pub fn is_send_enabled() -> bool {
    matches!(option_env!("INVOICE_ENABLE_CONCUR_SEND"), Some("1"))
}

pub fn send_receipt_message(
    request: &ReceiptMessage,
    authorization_code: &str,
) -> Result<String, DeliveryError> {
    if !is_send_enabled() {
        return Err(DeliveryError::before_send(
            "当前构建未启用 Concur 真实发送；内部 Alpha 不会连接 SMTP",
        ));
    }
    if authorization_code.is_empty() {
        return Err(DeliveryError::before_send("当前会话没有邮箱授权码"));
    }

    let provider = provider_for_email(&request.sender_email)?;
    let message = build_message(request)?;
    let builder = match provider.tls_mode {
        SmtpTlsMode::Wrapper => SmtpTransport::relay(provider.host),
        SmtpTlsMode::RequiredStartTls => SmtpTransport::starttls_relay(provider.host),
    }
    .map_err(|_| DeliveryError::before_send("无法建立安全 SMTP 配置"))?;
    let transport = builder
        .port(provider.port)
        .credentials(Credentials::new(
            request.sender_email.clone(),
            authorization_code.to_string(),
        ))
        .timeout(Some(SMTP_TIMEOUT))
        .build();

    // Once `send` starts, a network error can occur after the relay accepted DATA. Treat every
    // error as ambiguous; the user must resolve it against Concur before retrying.
    deliver_with_transport(&transport, &message, &request.message_id)
}

fn deliver_with_transport(
    transport: &impl ReceiptTransport,
    message: &Message,
    message_id: &str,
) -> Result<String, DeliveryError> {
    transport
        .deliver(message)
        .map_err(|_| DeliveryError::outcome_unknown())?;
    Ok(message_id.to_string())
}

fn build_message(request: &ReceiptMessage) -> Result<Message, DeliveryError> {
    validate_request(request)?;
    let from: Mailbox = request
        .sender_email
        .parse()
        .map_err(|_| DeliveryError::before_send("发件邮箱格式不正确"))?;
    let to: Mailbox = request
        .recipient_email
        .parse()
        .map_err(|_| DeliveryError::before_send("Concur 收件地址格式不正确"))?;
    let subject = if request.is_trial {
        "Invoice Assistant - Concur trial receipt"
    } else {
        "Invoice Assistant - Concur receipts"
    };
    let body = format!(
        "Sent after explicit user confirmation. This message contains {} reviewed receipt attachment(s).",
        request.attachments.len()
    );
    let mut multipart = MultiPart::mixed().singlepart(SinglePart::plain(body));
    for attachment in &request.attachments {
        let content_type = ContentType::parse(attachment.mime_type)
            .map_err(|_| DeliveryError::before_send("收据 MIME 类型不受支持"))?;
        multipart = multipart.singlepart(
            Attachment::new(attachment.name.clone()).body(attachment.bytes.clone(), content_type),
        );
    }

    Message::builder()
        .from(from)
        .to(to)
        .subject(subject)
        .message_id(Some(request.message_id.clone()))
        .multipart(multipart)
        .map_err(|_| DeliveryError::before_send("无法构造收据邮件"))
}

fn validate_request(request: &ReceiptMessage) -> Result<(), DeliveryError> {
    if request.attachments.is_empty() || request.attachments.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(DeliveryError::before_send(
            "一封邮件必须包含 1 至 5 个收据附件",
        ));
    }
    if request.message_id.len() > 998
        || !request.message_id.starts_with('<')
        || !request.message_id.ends_with('>')
        || request.message_id.chars().any(char::is_control)
    {
        return Err(DeliveryError::before_send("邮件幂等标识格式不正确"));
    }
    let mut total_bytes = 0usize;
    for attachment in &request.attachments {
        if attachment.bytes.is_empty() || attachment.bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(DeliveryError::before_send(
                "单个收据必须大于 0 字节且不超过 15 MiB",
            ));
        }
        total_bytes = total_bytes
            .checked_add(attachment.bytes.len())
            .ok_or_else(|| DeliveryError::before_send("收据附件大小溢出"))?;
        if attachment.name.is_empty()
            || attachment.name.chars().count() > 180
            || attachment.name.chars().any(char::is_control)
            || attachment.name.contains(['/', '\\'])
            || !matches!(
                attachment.mime_type,
                "application/pdf" | "image/png" | "image/jpeg" | "image/tiff"
            )
        {
            return Err(DeliveryError::before_send("收据附件名称或格式不受支持"));
        }
    }
    if total_bytes > MAX_MESSAGE_ATTACHMENT_BYTES {
        return Err(DeliveryError::before_send(
            "单封邮件的收据附件合计不能超过 20 MiB",
        ));
    }
    Ok(())
}

fn provider_for_email(email: &str) -> Result<SmtpProvider, DeliveryError> {
    let (_, domain) = email
        .trim()
        .rsplit_once('@')
        .ok_or_else(|| DeliveryError::before_send("发件邮箱格式不正确"))?;
    let provider = match domain.to_ascii_lowercase().as_str() {
        "qq.com" | "vip.qq.com" | "foxmail.com" => SmtpProvider {
            host: "smtp.qq.com",
            port: 465,
            tls_mode: SmtpTlsMode::Wrapper,
        },
        "163.com" => SmtpProvider {
            host: "smtp.163.com",
            port: 465,
            tls_mode: SmtpTlsMode::Wrapper,
        },
        "126.com" => SmtpProvider {
            host: "smtp.126.com",
            port: 465,
            tls_mode: SmtpTlsMode::Wrapper,
        },
        "gmail.com" => SmtpProvider {
            host: "smtp.gmail.com",
            port: 465,
            tls_mode: SmtpTlsMode::Wrapper,
        },
        "outlook.com" | "hotmail.com" => SmtpProvider {
            host: "smtp.office365.com",
            port: 587,
            tls_mode: SmtpTlsMode::RequiredStartTls,
        },
        _ => {
            return Err(DeliveryError::before_send(
                "当前邮箱服务商尚未配置安全 SMTP 参数",
            ));
        }
    };
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeTransport {
        succeeds: bool,
    }

    impl ReceiptTransport for FakeTransport {
        fn deliver(&self, _message: &Message) -> Result<(), ()> {
            self.succeeds.then_some(()).ok_or(())
        }
    }

    fn attachment(name: &str) -> ReceiptAttachment {
        ReceiptAttachment {
            name: name.to_string(),
            mime_type: "application/pdf",
            bytes: b"synthetic-pdf-placeholder".to_vec(),
        }
    }

    fn request() -> ReceiptMessage {
        ReceiptMessage {
            sender_email: "sender@example.test".to_string(),
            recipient_email: "receipts@concur.example".to_string(),
            message_id: "<invoice-assistant-test@local.invalid>".to_string(),
            is_trial: true,
            attachments: vec![attachment("2026-07-15_other_100.00_123456.pdf")],
        }
    }

    #[test]
    fn providers_require_encrypted_submission() {
        let qq = provider_for_email("test-user@qq.com").unwrap();
        assert_eq!(qq.host, "smtp.qq.com");
        assert_eq!(qq.port, 465);
        assert_eq!(qq.tls_mode, SmtpTlsMode::Wrapper);
        let outlook = provider_for_email("test-user@outlook.com").unwrap();
        assert_eq!(outlook.port, 587);
        assert_eq!(outlook.tls_mode, SmtpTlsMode::RequiredStartTls);
        assert!(provider_for_email("test-user@example.test").is_err());
    }

    #[test]
    fn message_contains_only_reviewed_attachment_metadata() {
        let message = build_message(&request()).unwrap();
        let formatted = String::from_utf8(message.formatted()).unwrap();
        assert!(formatted.contains("Concur trial receipt"));
        assert!(formatted.contains("2026-07-15_other_100.00_123456.pdf"));
        assert!(formatted.contains("application/pdf"));
        assert!(!formatted.contains("authorization-code"));
    }

    #[test]
    fn message_limits_are_enforced_before_network() {
        let mut too_many = request();
        too_many.attachments = (0..6)
            .map(|index| attachment(&format!("receipt-{index}.pdf")))
            .collect();
        assert_eq!(
            build_message(&too_many).unwrap_err().kind,
            DeliveryErrorKind::BeforeSend
        );

        let mut unsupported = request();
        unsupported.attachments[0].mime_type = "application/xml";
        assert!(build_message(&unsupported).is_err());
    }

    #[test]
    fn simulated_smtp_success_returns_stable_message_id() {
        let request = request();
        let message = build_message(&request).unwrap();
        let result = deliver_with_transport(
            &FakeTransport { succeeds: true },
            &message,
            &request.message_id,
        )
        .unwrap();
        assert_eq!(result, request.message_id);
    }

    #[test]
    fn simulated_smtp_transport_failure_is_outcome_unknown() {
        let request = request();
        let message = build_message(&request).unwrap();
        let error = deliver_with_transport(
            &FakeTransport { succeeds: false },
            &message,
            &request.message_id,
        )
        .unwrap_err();
        assert_eq!(error.kind, DeliveryErrorKind::OutcomeUnknown);
    }

    #[test]
    fn disabled_build_fails_closed_before_provider_or_network() {
        if !is_send_enabled() {
            let error = send_receipt_message(&request(), "authorization-code").unwrap_err();
            assert_eq!(error.kind, DeliveryErrorKind::BeforeSend);
            assert!(error.to_string().contains("未启用"));
        }
    }
}
