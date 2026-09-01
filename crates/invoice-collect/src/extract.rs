use anyhow::{Context, Result};
use base64::Engine;
use mail_parser::{Encoding, Message, MessagePart, MimeHeaders, PartType};
use std::io::{Cursor, Read};
use url::Url;
const MAX_ZIP_ENTRIES: usize = 100;
const MAX_ZIP_ENTRY_BYTES: u64 = 25 * 1024 * 1024;
const MAX_ZIP_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
/// 正文仅用于生成“可能通过链接交付发票”的提示和本地审核快照。
const MAX_LINK_HINT_SCAN_CHARS: usize = 256 * 1024;
const MAX_REVIEW_BODY_CHARS: usize = 100 * 1024;
const MAX_REVIEW_LINKS: usize = 20;
const MAX_REVIEW_URL_CHARS: usize = 4096;

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
    /// 正文包含 HTTP(S) 链接和明确的发票下载/查看语义。
    pub invoice_link_hint: bool,
    /// 主题或正文包含明确的开票完成/发票已发送语义。
    /// 仅保留布尔值，用于没有可取得文件时形成“需用户确认”台账状态。
    pub invoice_notice_hint: bool,
    /// 具名图片附件中包含可安全打开的 HTTP(S) 二维码。
    /// 这里只保留布尔值，完整地址只进入可信后端审核快照。
    pub attachment_qr_link_hint: bool,
}

/// 收集阶段从原始邮件生成的安全纯文本正文与下载链接。
#[derive(Clone, PartialEq)]
pub struct EmailReviewContent {
    pub sender_name: Option<String>,
    pub sender_address: Option<String>,
    pub body_text: String,
    pub body_truncated: bool,
    pub links: Vec<EmailReviewLink>,
}

/// `url` 只供可信后端持久化并在用户点击后打开；Tauri DTO 不会把它返回给 WebView。
#[derive(Clone, PartialEq)]
pub struct EmailReviewLink {
    pub label: String,
    pub host: String,
    pub url: String,
}

/// 对单个图片附件的二维码分析。只返回经过 URL 安全校验的 HTTP(S) 地址；
/// `qr_dominant` 用于区分“带二维码的账单”与“以二维码为主体的下载指引图”。
#[derive(Clone, PartialEq)]
pub struct QrAttachmentAnalysis {
    pub detected: bool,
    pub qr_dominant: bool,
    pub links: Vec<EmailReviewLink>,
}

fn strip_html_markup(html: &str) -> String {
    let mut visible = String::with_capacity(html.len().min(MAX_LINK_HINT_SCAN_CHARS));
    let mut inside_tag = false;

    for character in html.chars().take(MAX_LINK_HINT_SCAN_CHARS) {
        match character {
            '<' => {
                inside_tag = true;
                visible.push(' ');
            }
            '>' => {
                inside_tag = false;
                visible.push(' ');
            }
            _ if !inside_tag => visible.push(character),
            _ => {}
        }
    }

    visible
}

fn has_invoice_action(text: &str) -> bool {
    let lower = text.to_lowercase();
    let compact: String = lower
        .chars()
        .filter(|character| !character.is_whitespace() && !character.is_ascii_punctuation())
        .collect();

    const ENGLISH_ACTIONS: &[&str] = &[
        "download invoice",
        "view invoice",
        "retrieve invoice",
        "get invoice",
        "invoice download",
    ];
    const CHINESE_ACTIONS: &[&str] = &[
        "下载发票",
        "查看发票",
        "获取发票",
        "领取发票",
        "发票下载",
        "发票查看",
    ];

    ENGLISH_ACTIONS.iter().any(|action| lower.contains(action))
        || CHINESE_ACTIONS
            .iter()
            .any(|action| compact.contains(action))
}

fn has_invoice_notice(text: &str) -> bool {
    let lower = text.to_lowercase();
    let compact: String = lower
        .chars()
        .filter(|character| !character.is_whitespace() && !character.is_ascii_punctuation())
        .collect();
    const CHINESE_NOTICES: &[&str] = &[
        "开票成功",
        "发票已开具",
        "发票已发送",
        "电子发票通知",
        "发票申请成功",
        "发票开具完成",
    ];
    const ENGLISH_NOTICES: &[&str] = &[
        "invoice issued",
        "invoice is ready",
        "invoice has been sent",
    ];
    CHINESE_NOTICES
        .iter()
        .any(|notice| compact.contains(notice))
        || ENGLISH_NOTICES.iter().any(|notice| lower.contains(notice))
}

fn has_review_link_action(text: &str) -> bool {
    if has_invoice_action(text) {
        return true;
    }
    let lower = text.to_lowercase();
    let compact: String = lower
        .chars()
        .filter(|character| !character.is_whitespace() && !character.is_ascii_punctuation())
        .collect();
    let has_document = ["发票", "票据", "行程单", "invoice", "receipt"]
        .iter()
        .any(|word| compact.contains(word));
    let has_action = [
        "下载", "查看", "获取", "领取", "打开", "查验", "验证", "download", "view", "retrieve",
        "get", "verify", "validate",
    ]
    .iter()
    .any(|word| compact.contains(word));
    has_document && has_action
}

fn clean_link_label(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let url_start = [lower.find("https://"), lower.find("http://")]
        .into_iter()
        .flatten()
        .min();
    let visible = url_start.map(|index| &value[..index]).unwrap_or(value);
    let compact = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    let label: String = compact.chars().take(120).collect();
    if label.is_empty() {
        "打开相关下载页面".to_string()
    } else {
        label
    }
}

/// 重新校验准备持久化或打开的邮件链接。
///
/// 只允许无用户信息的 HTTP(S) URL，并排除明显的广告、跟踪和退订地址。
pub fn validated_review_link(url: &str, label: &str) -> Option<EmailReviewLink> {
    let candidate = url
        .trim()
        .trim_matches(['<', '>', '"', '\'', '(', ')', '[', ']', '{', '}', ',', ';']);
    if candidate.chars().count() > MAX_REVIEW_URL_CHARS {
        return None;
    }
    let parsed = Url::parse(candidate).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    let host = parsed
        .host_str()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty()
        || ["ad.", "ads.", "track.", "tracking.", "pixel."]
            .iter()
            .any(|prefix| host.starts_with(prefix))
    {
        return None;
    }
    let rejection_text = format!(
        "{} {}",
        candidate.to_ascii_lowercase(),
        label.to_lowercase()
    );
    if [
        "unsubscribe",
        "optout",
        "opt-out",
        "tracking",
        "track/pixel",
        "退订",
        "取消订阅",
    ]
    .iter()
    .any(|term| rejection_text.contains(term))
    {
        return None;
    }
    Some(EmailReviewLink {
        label: clean_link_label(label),
        host,
        url: parsed.to_string(),
    })
}

fn web_url_tokens(text: &str) -> Vec<&str> {
    let lower = text.to_ascii_lowercase();
    let mut tokens = Vec::new();
    let mut offset = 0usize;
    while offset < lower.len() {
        let https = lower[offset..].find("https://");
        let http = lower[offset..].find("http://");
        let Some(found) = [https, http].into_iter().flatten().min() else {
            break;
        };
        let start = offset + found;
        let tail = &text[start..];
        let end = tail
            .char_indices()
            .find_map(|(index, character)| {
                (character.is_whitespace()
                    || matches!(character, '"' | '\'' | '<' | '>' | ')' | ']' | '}'))
                .then_some(index)
            })
            .unwrap_or(tail.len());
        if end > 0 {
            tokens.push(&tail[..end]);
        }
        offset = start.saturating_add(end.max(7));
        if offset >= text.len() || tokens.len() >= MAX_REVIEW_LINKS * 4 {
            break;
        }
    }
    tokens
}

fn html_anchor_links(html: &str) -> Vec<EmailReviewLink> {
    let lower = html.to_ascii_lowercase();
    let mut links = Vec::new();
    let mut offset = 0usize;
    while let Some(anchor_start_rel) = lower[offset..].find("<a") {
        let anchor_start = offset + anchor_start_rel;
        let Some(tag_end_rel) = lower[anchor_start..].find('>') else {
            break;
        };
        let tag_end = anchor_start + tag_end_rel;
        let tag = &html[anchor_start..=tag_end];
        let tag_lower = tag.to_ascii_lowercase();
        let Some(href_rel) = tag_lower.find("href") else {
            offset = tag_end + 1;
            continue;
        };
        let after_href = &tag[href_rel + 4..];
        let Some(equals_rel) = after_href.find('=') else {
            offset = tag_end + 1;
            continue;
        };
        let value = after_href[equals_rel + 1..].trim_start();
        let (href, _) = if let Some(rest) = value.strip_prefix('"') {
            match rest.find('"') {
                Some(end) => (&rest[..end], end + 2),
                None => {
                    offset = tag_end + 1;
                    continue;
                }
            }
        } else if let Some(rest) = value.strip_prefix('\'') {
            match rest.find('\'') {
                Some(end) => (&rest[..end], end + 2),
                None => {
                    offset = tag_end + 1;
                    continue;
                }
            }
        } else {
            let end = value
                .char_indices()
                .find_map(|(index, character)| {
                    (character.is_whitespace() || character == '>').then_some(index)
                })
                .unwrap_or(value.len());
            (&value[..end], end)
        };
        let close = lower[tag_end + 1..]
            .find("</a>")
            .map(|index| tag_end + 1 + index)
            .unwrap_or(tag_end + 1);
        let visible = strip_html_markup(&html[tag_end + 1..close]);
        // HTML 邮件常把官网、广告和跟踪地址放在无文字的图片锚点中。链接是否与
        // 发票相关必须由锚点自身或其局部正文证明，不能让邮件主题替每一个锚点背书。
        let mut context_start = anchor_start.saturating_sub(240);
        while !html.is_char_boundary(context_start) {
            context_start += 1;
        }
        let mut context_end = close.saturating_add(240).min(html.len());
        while !html.is_char_boundary(context_end) {
            context_end -= 1;
        }
        let local_context = strip_html_markup(&html[context_start..context_end]);
        if !visible.trim().is_empty()
            && (has_review_link_action(&visible) || has_review_link_action(&local_context))
        {
            if let Some(link) = validated_review_link(href, &visible) {
                links.push(link);
            }
        }
        offset = close.saturating_add(4);
        if links.len() >= MAX_REVIEW_LINKS {
            break;
        }
    }
    links
}

fn append_unique_link(links: &mut Vec<EmailReviewLink>, link: EmailReviewLink) {
    if links.len() < MAX_REVIEW_LINKS && !links.iter().any(|item| item.url == link.url) {
        links.push(link);
    }
}

fn decoded_attachment_bytes(message: &Message<'_>, part: &MessagePart<'_>) -> Result<Vec<u8>> {
    // mail-parser intentionally converts every text/* MIME part to Unicode. Some invoice
    // delivery services incorrectly label PDF/OFD attachments as text/plain; calling
    // `contents()` for those parts therefore changes arbitrary binary bytes. A named MIME
    // part is a file, so decode its transfer encoding directly from the original message.
    if matches!(part.body, PartType::Text(_) | PartType::Html(_)) {
        let encoded = message
            .raw_message()
            .get(part.offset_body..part.offset_end)
            .context("邮件附件原始字节范围无效")?;
        return match part.encoding {
            Encoding::Base64 => {
                let compact = encoded
                    .iter()
                    .copied()
                    .filter(|byte| !byte.is_ascii_whitespace())
                    .collect::<Vec<_>>();
                base64::engine::general_purpose::STANDARD
                    .decode(compact)
                    .context("邮件附件 Base64 解码失败")
            }
            Encoding::QuotedPrintable => {
                quoted_printable::decode(encoded, quoted_printable::ParseMode::Robust)
                    .context("邮件附件 quoted-printable 解码失败")
            }
            Encoding::None => Ok(encoded.to_vec()),
        };
    }
    Ok(part.contents().to_vec())
}

fn decoded_named_attachments(message: &Message<'_>) -> Result<Vec<RawAttachment>> {
    let mut attachments = Vec::new();
    for part in message.attachments() {
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
            data: decoded_attachment_bytes(message, part)?,
        });
    }
    Ok(attachments)
}

fn is_image_filename(filename: &str) -> bool {
    matches!(
        std::path::Path::new(filename)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "tif" | "tiff"
    )
}

fn scan_qr_variant(
    gray: image::GrayImage,
    detected: &mut bool,
    dominant_ratio: &mut f32,
    links: &mut Vec<EmailReviewLink>,
) {
    let width = gray.width().max(1) as f32;
    let height = gray.height().max(1) as f32;
    let mut prepared = rqrr::PreparedImage::prepare(gray);
    for grid in prepared.detect_grids() {
        *detected = true;
        let min_x = grid.bounds.iter().map(|point| point.x).min().unwrap_or(0);
        let max_x = grid.bounds.iter().map(|point| point.x).max().unwrap_or(0);
        let min_y = grid.bounds.iter().map(|point| point.y).min().unwrap_or(0);
        let max_y = grid.bounds.iter().map(|point| point.y).max().unwrap_or(0);
        let qr_width = max_x.saturating_sub(min_x).max(0) as f32;
        let qr_height = max_y.saturating_sub(min_y).max(0) as f32;
        *dominant_ratio = dominant_ratio.max((qr_width / width).max(qr_height / height));
        let Ok((_, value)) = grid.decode() else {
            continue;
        };
        if let Some(link) = validated_review_link(&value, "打开图片中提取的二维码地址")
        {
            append_unique_link(links, link);
        }
    }
}

/// 从一个本地图片附件提取二维码。最多处理 4000 万像素，并用原始灰度图、
/// 增强对比度图和等比缩小图多次识别，以覆盖带 Logo 的二维码和屏幕拍照摩尔纹。
pub fn analyze_qr_attachment(attachment: &RawAttachment) -> QrAttachmentAnalysis {
    let mut analysis = QrAttachmentAnalysis {
        detected: false,
        qr_dominant: false,
        links: Vec::new(),
    };
    if !is_image_filename(&attachment.filename) {
        return analysis;
    }
    let Ok(format) = image::guess_format(&attachment.data) else {
        return analysis;
    };
    let Ok((width, height)) =
        image::ImageReader::with_format(Cursor::new(&attachment.data), format).into_dimensions()
    else {
        return analysis;
    };
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 40_000_000 {
        return analysis;
    }
    let Ok(decoded) = image::load_from_memory_with_format(&attachment.data, format) else {
        return analysis;
    };
    let gray = decoded.to_luma8();
    let mut dominant_ratio = 0.0f32;
    scan_qr_variant(
        gray.clone(),
        &mut analysis.detected,
        &mut dominant_ratio,
        &mut analysis.links,
    );
    if analysis.links.is_empty() {
        scan_qr_variant(
            image::imageops::contrast(&gray, 35.0),
            &mut analysis.detected,
            &mut dominant_ratio,
            &mut analysis.links,
        );
    }
    if analysis.links.is_empty() && width.max(height) > 1_600 {
        let scale = 1_600.0 / width.max(height) as f32;
        let resized = image::imageops::resize(
            &gray,
            ((width as f32 * scale).round() as u32).max(1),
            ((height as f32 * scale).round() as u32).max(1),
            image::imageops::FilterType::Lanczos3,
        );
        scan_qr_variant(
            resized,
            &mut analysis.detected,
            &mut dominant_ratio,
            &mut analysis.links,
        );
    }
    analysis.qr_dominant = analysis.detected && dominant_ratio >= 0.30;
    analysis
}

fn image_qr_links(attachments: &[RawAttachment]) -> Vec<EmailReviewLink> {
    let mut links = Vec::new();
    for attachment in attachments {
        for link in analyze_qr_attachment(attachment).links {
            append_unique_link(&mut links, link);
        }
    }
    links
}

/// 为独立邮件审核页生成安全纯文本正文与发票相关 HTTP(S) 链接。
pub fn extract_review_content(raw: &[u8]) -> Result<EmailReviewContent> {
    let message = mail_parser::MessageParser::default()
        .parse(raw)
        .context("邮件 MIME 结构无法解析")?;
    let subject = message
        .subject()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("(无主题)");
    let sender = message.from().and_then(|addresses| addresses.first());
    let sender_name = sender
        .and_then(|address| address.name())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let sender_address = sender
        .and_then(|address| address.address())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);

    let mut body_parts = Vec::new();
    let mut links = Vec::new();
    for index in 0..message.text_body.len() {
        if let Some(body) = message.body_text(index) {
            let snippet: String = body.chars().take(MAX_LINK_HINT_SCAN_CHARS).collect();
            for line in snippet.lines() {
                let context = format!("{subject} {line}");
                if has_review_link_action(&context) {
                    for token in web_url_tokens(line) {
                        if let Some(link) = validated_review_link(token, line) {
                            append_unique_link(&mut links, link);
                        }
                    }
                }
            }
            body_parts.push(snippet);
        }
    }
    for index in 0..message.html_body.len() {
        if let Some(body) = message.body_html(index) {
            let snippet: String = body.chars().take(MAX_LINK_HINT_SCAN_CHARS).collect();
            for link in html_anchor_links(&snippet) {
                append_unique_link(&mut links, link);
            }
            if body_parts.is_empty() {
                body_parts.push(strip_html_markup(&snippet));
            }
        }
    }
    for link in image_qr_links(&decoded_named_attachments(&message)?) {
        append_unique_link(&mut links, link);
    }
    let body = body_parts.join("\n\n");
    let body_truncated = body.chars().count() > MAX_REVIEW_BODY_CHARS;
    let body_text = body.chars().take(MAX_REVIEW_BODY_CHARS).collect::<String>();

    Ok(EmailReviewContent {
        sender_name,
        sender_address,
        body_text: if body_text.trim().is_empty() {
            "（邮件正文为空）".to_string()
        } else {
            body_text
        },
        body_truncated,
        links,
    })
}

fn invoice_delivery_hints(message: &mail_parser::Message<'_>, subject: &str) -> (bool, bool) {
    let mut has_web_link = false;
    let mut has_action = has_invoice_action(subject);
    let mut has_notice = has_invoice_notice(subject);

    for index in 0..message.text_body.len() {
        if let Some(body) = message.body_text(index) {
            let snippet: String = body.chars().take(MAX_LINK_HINT_SCAN_CHARS).collect();
            let lower = snippet.to_ascii_lowercase();
            has_web_link |= lower.contains("https://") || lower.contains("http://");
            has_action |= has_invoice_action(&snippet);
            has_notice |= has_invoice_notice(&snippet);
        }
    }

    for index in 0..message.html_body.len() {
        if let Some(body) = message.body_html(index) {
            let snippet: String = body.chars().take(MAX_LINK_HINT_SCAN_CHARS).collect();
            let lower = snippet.to_ascii_lowercase();
            has_web_link |= lower.contains("https://") || lower.contains("http://");
            let visible = strip_html_markup(&snippet);
            has_action |= has_invoice_action(&visible);
            has_notice |= has_invoice_notice(&visible);
        }
    }

    (has_web_link && has_action, has_notice)
}

/// 解析一封原始邮件，取出头字段与所有具名附件。
///
/// 无 filename 的部件（正文、内联图片）一律跳过 —— 发票必然是具名附件。
pub fn extract_email(raw: &[u8]) -> Result<ExtractedEmail> {
    let message = mail_parser::MessageParser::default()
        .parse(raw)
        .context("邮件 MIME 结构无法解析")?;

    let message_id = message
        .message_id()
        .map(|id| id.trim_matches(['<', '>']).to_string());

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

    let (invoice_link_hint, invoice_notice_hint) = invoice_delivery_hints(&message, &subject);

    let attachments = decoded_named_attachments(&message)?;
    let attachment_qr_link_hint = !image_qr_links(&attachments).is_empty();

    Ok(ExtractedEmail {
        message_id,
        subject,
        from,
        attachments,
        invoice_link_hint,
        invoice_notice_hint,
        attachment_qr_link_hint,
    })
}

/// Extract ZIP archives if needed. Invalid or unsafe ZIP files return no attachments.
///
/// If the attachment is a ZIP file (based on extension and content type),
/// extract all files inside and return them as separate RawAttachments.
/// Otherwise, return the original attachment unchanged.
pub fn extract_zip_if_needed(att: &RawAttachment) -> Vec<RawAttachment> {
    let filename_lower = att.filename.to_lowercase();

    // Check if this is a ZIP file
    if !filename_lower.ends_with(".zip") {
        return vec![att.clone()];
    }

    // Check content type
    let ct_lower = att.content_type.to_lowercase();
    if ct_lower != "application/zip" && ct_lower != "application/octet-stream" {
        return vec![att.clone()];
    }

    // Try to extract the ZIP
    let cursor = std::io::Cursor::new(&att.data);
    let mut archive = match zip::ZipArchive::new(cursor) {
        Ok(a) => a,
        Err(_) => {
            // 不把损坏的 ZIP 当成普通发票继续处理。
            return Vec::new();
        }
    };

    // 拒绝异常条目数量，避免 ZIP 炸弹耗尽内存。
    if archive.len() > MAX_ZIP_ENTRIES {
        return Vec::new();
    }

    let mut extracted = Vec::new();
    let mut total_uncompressed = 0u64;

    for i in 0..archive.len() {
        let file = match archive.by_index(i) {
            Ok(f) => f,
            Err(_) => continue,
        };

        // Skip directories
        if file.is_dir() {
            continue;
        }

        let name = match file.enclosed_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => continue,
        };

        let entry_size = file.size();
        if entry_size > MAX_ZIP_ENTRY_BYTES
            || total_uncompressed.saturating_add(entry_size) > MAX_ZIP_TOTAL_BYTES
        {
            continue;
        }

        // 即使中央目录中的 size 不可信，读取也有硬上限。
        let mut contents = Vec::new();
        let mut limited = file.take(MAX_ZIP_ENTRY_BYTES + 1);
        if limited.read_to_end(&mut contents).is_err()
            || contents.len() as u64 > MAX_ZIP_ENTRY_BYTES
        {
            continue;
        }
        total_uncompressed = total_uncompressed.saturating_add(contents.len() as u64);

        // Infer content type from filename
        let content_type = if name.to_lowercase().ends_with(".pdf") {
            "application/pdf".to_string()
        } else if name.to_lowercase().ends_with(".ofd") {
            "application/ofd".to_string()
        } else if name.to_lowercase().ends_with(".xml") {
            "application/xml".to_string()
        } else {
            "application/octet-stream".to_string()
        };

        extracted.push(RawAttachment {
            filename: name,
            content_type,
            data: contents,
        });
    }

    extracted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

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
    fn preserves_binary_attachment_mislabeled_as_text_plain() {
        let original = b"%PDF-1.7\n\xff\x00\x80binary-payload\n%%EOF";
        let encoded = base64::engine::general_purpose::STANDARD.encode(original);
        let eml = format!(
            "From: billing@example.com\r\n\
             Subject: invoice\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=B\r\n\r\n\
             --B\r\n\
             Content-Type: text/plain\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; filename=invoice.pdf\r\n\r\n\
             {encoded}\r\n\
             --B--\r\n"
        );

        let email = extract_email(eml.as_bytes()).unwrap();
        assert_eq!(email.attachments.len(), 1);
        assert_eq!(email.attachments[0].data, original);
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
    fn detects_explicit_https_invoice_download_in_html() {
        let eml = b"From: billing@example.com\r\n\
                    Subject: Your invoice\r\n\
                    Content-Type: text/html; charset=UTF-8\r\n\
                    \r\n\
                    <html><a href=\"https://billing.example.com/i/123\">Download invoice</a></html>\r\n";

        let email = extract_email(eml).unwrap();
        assert!(email.invoice_link_hint);
        assert!(!format!("{email:?}").contains("billing.example.com/i/123"));
    }

    #[test]
    fn detects_chinese_action_split_by_html_markup() {
        let eml = "From: billing@example.com\r\n\
                   Subject: 电子票据\r\n\
                   Content-Type: text/html; charset=UTF-8\r\n\
                   \r\n\
                   <a href=\"https://billing.example.com/i/123\"><span>下载</span><b>发票</b></a>\r\n";

        assert!(extract_email(eml.as_bytes()).unwrap().invoice_link_hint);
    }

    #[test]
    fn detects_invoice_notice_without_download_link() {
        let eml = b"From: billing@example.com\r\n\
                    Subject: Electronic invoice notice\r\n\
                    Content-Type: text/plain; charset=UTF-8\r\n\
                    \r\n\
                    Your invoice is ready.\r\n";
        let email = extract_email(eml).unwrap();
        assert!(email.invoice_notice_hint);
        assert!(!email.invoice_link_hint);
    }

    #[test]
    fn unrelated_message_is_not_an_invoice_notice() {
        let eml =
            b"From: news@example.com\r\nSubject: Weekly report\r\n\r\nNo billing content.\r\n";
        let email = extract_email(eml).unwrap();
        assert!(!email.invoice_notice_hint);
    }

    #[test]
    fn rejects_unsafe_or_unrelated_body_links() {
        let cases: &[&[u8]] = &[
            b"From: a@b.com\r\nSubject: invoice\r\nContent-Type: text/plain\r\n\r\nUnsubscribe: https://example.com/unsubscribe\r\n",
            b"From: a@b.com\r\nSubject: invoice\r\nContent-Type: text/plain\r\n\r\nYour invoice is ready.\r\n",
            b"From: a@b.com\r\nSubject: software\r\nContent-Type: text/plain\r\n\r\nDownload package: https://example.com/app\r\n",
        ];

        for eml in cases {
            assert!(!extract_email(eml).unwrap().invoice_link_hint);
        }
    }

    #[test]
    fn visible_http_invoice_download_is_a_link_hint() {
        let eml = b"From: a@b.com\r\nSubject: invoice\r\nContent-Type: text/plain\r\n\r\nDownload invoice: http://delivery.example/i\r\n";
        assert!(extract_email(eml).unwrap().invoice_link_hint);
    }

    #[test]
    fn review_content_returns_plain_body_and_only_relevant_https_links() {
        let eml = b"From: Billing Team <billing@example.com>\r\n\
                    Subject: Electronic invoice ready\r\n\
                    Content-Type: text/html; charset=UTF-8\r\n\
                    \r\n\
                    <html><body><p>Your invoice is ready.</p>\
                    <a href=\"https://billing.example.com/i/123\">Download invoice</a>\
                    <a href=\"https://example.com/unsubscribe\">Unsubscribe</a>\
                    <img src=\"https://track.example.com/pixel\"></body></html>\r\n";

        let review = extract_review_content(eml).unwrap();
        assert_eq!(review.sender_name.as_deref(), Some("Billing Team"));
        assert_eq!(
            review.sender_address.as_deref(),
            Some("billing@example.com")
        );
        assert!(review.body_text.contains("Your invoice is ready."));
        assert!(!review.body_text.contains("<html>"));
        assert_eq!(review.links.len(), 1);
        assert_eq!(review.links[0].host, "billing.example.com");
        assert_eq!(review.links[0].label, "Download invoice");
    }

    #[test]
    fn review_content_accepts_visible_http_and_rejects_credentialed_links() {
        let eml = b"From: a@b.com\r\nSubject: Invoice download\r\n\r\n\
                    Download invoice: http://unsafe.example/i\r\n\
                    Download invoice: https://user:pass@example.com/i\r\n";
        let review = extract_review_content(eml).unwrap();
        assert_eq!(review.links.len(), 1);
        assert_eq!(review.links[0].url, "http://unsafe.example/i");
    }

    #[test]
    fn review_content_prefers_visible_invoice_url_and_rejects_footer_and_ad_links() {
        let eml = "From: billing@example.com\r\n\
                   Subject: 电子发票查看通知\r\n\
                   Content-Type: multipart/alternative; boundary=ALT\r\n\r\n\
                   --ALT\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n\
                   发票已开具，点击链接查看：http://u.baiwang.com/token\r\n\
                   --ALT\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n\
                   <p>发票已开具。</p>\
                   <a href=\"http://u.baiwang.com/token\">点击查看发票</a>\
                   <a href=\"https://www.baiwang.com\"><img src=\"logo.png\"></a>\
                   <a href=\"https://ad.efapiao.com/campaign\"><img src=\"ad.png\"></a>\
                   --ALT--\r\n";

        let review = extract_review_content(eml.as_bytes()).unwrap();
        assert_eq!(review.links.len(), 1);
        assert_eq!(review.links[0].host, "u.baiwang.com");
        assert_eq!(review.links[0].url, "http://u.baiwang.com/token");
    }

    #[test]
    fn review_content_accepts_official_ticket_verification_link() {
        let eml = "From: ticket@example.com\r\n\
                   Subject: 电子票据\r\n\
                   Content-Type: text/html; charset=UTF-8\r\n\r\n\
                   <p>请前往票据查验网站核对真伪。</p>\
                   <a href=\"https://ticket.example.com/verify\">票据查验网站</a>\r\n";

        let review = extract_review_content(eml.as_bytes()).unwrap();
        assert_eq!(review.links.len(), 1);
        assert_eq!(review.links[0].host, "ticket.example.com");
    }

    #[test]
    fn review_link_summary_never_exposes_the_full_plain_text_url() {
        let eml = b"From: a@b.com\r\nSubject: Invoice download\r\n\r\n\
                    Download invoice: https://billing.example.com/private/token-123\r\n\
                    Unsubscribe: https://billing.example.com/unsubscribe\r\n";
        let review = extract_review_content(eml).unwrap();
        assert_eq!(review.links.len(), 1);
        assert_eq!(review.links[0].host, "billing.example.com");
        assert!(!review.links[0].label.contains("https://"));
        assert!(!review.links[0].label.contains("token-123"));
    }

    #[test]
    fn review_content_truncates_large_bodies() {
        let body = "x".repeat(MAX_REVIEW_BODY_CHARS + 64);
        let eml = format!("From: a@b.com\r\nSubject: x\r\n\r\n{body}");
        let review = extract_review_content(eml.as_bytes()).unwrap();
        assert!(review.body_truncated);
        assert_eq!(review.body_text.chars().count(), MAX_REVIEW_BODY_CHARS);
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

    fn zip_with_entries(count: usize) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        for index in 0..count {
            writer
                .start_file(
                    format!("invoice-{index}.xml"),
                    zip::write::FileOptions::default(),
                )
                .unwrap();
            writer.write_all(b"<invoice/>").unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_supported_zip_within_limits() {
        let attachment = RawAttachment {
            filename: "invoices.zip".to_string(),
            content_type: "application/zip".to_string(),
            data: zip_with_entries(2),
        };

        let extracted = extract_zip_if_needed(&attachment);
        assert_eq!(extracted.len(), 2);
        assert!(extracted.iter().all(|item| item.filename.ends_with(".xml")));
    }

    #[test]
    fn rejects_zip_with_too_many_entries() {
        let attachment = RawAttachment {
            filename: "too-many.zip".to_string(),
            content_type: "application/zip".to_string(),
            data: zip_with_entries(MAX_ZIP_ENTRIES + 1),
        };

        assert!(extract_zip_if_needed(&attachment).is_empty());
    }

    #[test]
    fn missing_subject_falls_back_to_placeholder() {
        let eml = b"From: a@b.com\r\nContent-Type: text/plain\r\n\r\nx\r\n";
        let email = extract_email(eml).unwrap();
        assert_eq!(email.subject, "(无主题)");
    }
}
