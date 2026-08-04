use crate::config::{DateRange, ImapConfig};
use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct MessageSummary {
    pub uid: u32,
    pub subject: String,
    pub from: String,
    pub internal_date: String,
    pub has_attachments: bool,
}

/// 腾讯/网易的 IMAP 服务器在 SELECT 前需要客户端发 ID 命令，
/// 否则返回 `SELECT Unsafe Login`。这里构造符合 RFC 2971 的 ID 载荷。
pub(crate) fn id_command_payload() -> String {
    r#"ID ("name" "invoice-collect" "version" "0.1.0")"#.to_string()
}

/// IMAP 文件夹名含空格或非 ASCII 时必须加引号。
pub(crate) fn quote_folder(name: &str) -> String {
    if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '.') {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('\\', r"\\").replace('"', r#"\""#))
    }
}

pub struct Session {
    inner: imap::Session<Box<dyn imap::ImapConnection>>,
}

impl Session {
    pub fn connect(cfg: &ImapConfig) -> Result<Self> {
        if let Some(warning) = cfg.warn_if_password_looks_wrong() {
            eprintln!("警告: {warning}");
        }

        let client = imap::ClientBuilder::new(&cfg.host, cfg.port)
            .connect()
            .with_context(|| format!("连接 {}:{} 失败", cfg.host, cfg.port))?;

        let mut inner = client.login(&cfg.username, &cfg.password).map_err(|(e, _)| {
            anyhow::anyhow!(
                "登录失败: {e}。若为 QQ 邮箱，请确认：\
                 (1) 已在 设置 → 账户 中开启 IMAP 服务；\
                 (2) 密码用的是 16 位授权码而非登录密码"
            )
        })?;

        // 腾讯/网易服务器要求：SELECT 之前先发 ID，否则报 Unsafe Login。
        // 发送失败不致命 —— 有些服务器不认 ID 命令，忽略即可。
        if let Err(e) = inner.run_command_and_check_ok(&id_command_payload()) {
            eprintln!("提示: ID 命令未被接受（{e}），继续尝试");
        }

        Ok(Session { inner })
    }

    pub fn list_folders(&mut self) -> Result<Vec<String>> {
        let names = self
            .inner
            .list(Some(""), Some("*"))
            .context("LIST 命令失败")?
            .iter()
            .map(|n| n.name().to_string())
            .collect();
        Ok(names)
    }

    pub fn search_range(&mut self, folder: &str, range: &DateRange) -> Result<Vec<u32>> {
        self.inner
            .select(quote_folder(folder))
            .with_context(|| format!("SELECT {folder} 失败"))?;

        let query = range.to_imap_search();
        let mut uids: Vec<u32> = self
            .inner
            .uid_search(&query)
            .with_context(|| format!("UID SEARCH {query} 失败"))?
            .into_iter()
            .collect();
        uids.sort_unstable();
        Ok(uids)
    }

    pub fn fetch_summaries(&mut self, uids: &[u32]) -> Result<Vec<MessageSummary>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }

        // 分批获取，避免一次请求过多导致服务器断连
        let batch_size = 10;
        let mut out = Vec::new();

        for (batch_idx, chunk) in uids.chunks(batch_size).enumerate() {
            let set = chunk.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
            // QQ 邮箱服务器对 BODYSTRUCTURE 支持不稳定，容易断连，
            // 这里只获取基本信息，附件判断移到 fetch_raw 时进行
            let fetches = self
                .inner
                .uid_fetch(&set, "(INTERNALDATE ENVELOPE)")
                .with_context(|| format!("UID FETCH 概要失败（批次 {}, UIDs: {}）", batch_idx + 1, set))?;

            for f in fetches.iter() {
                let uid = f.uid.unwrap_or(0);
                let envelope = f.envelope();

                let subject = envelope
                    .and_then(|e| e.subject.as_ref())
                    .map(|s| decode_header_bytes(s))
                    .unwrap_or_else(|| "(无主题)".to_string());

                let from = envelope
                    .and_then(|e| e.from.as_ref())
                    .and_then(|addrs| addrs.first())
                    .map(|a| {
                        let mailbox = a.mailbox.as_deref().map(decode_header_bytes).unwrap_or_default();
                        let host = a.host.as_deref().map(decode_header_bytes).unwrap_or_default();
                        format!("{mailbox}@{host}")
                    })
                    .unwrap_or_else(|| "(未知发件人)".to_string());

                let internal_date = f
                    .internal_date()
                    .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "(无日期)".to_string());

                // 附件判断在探测阶段不可靠，标记为未知
                // 真正的附件提取在 Task 3 通过完整解析实现
                let has_attachments = false;

                out.push(MessageSummary {
                    uid,
                    subject,
                    from,
                    internal_date,
                    has_attachments,
                });
            }
        }
        Ok(out)
    }

    pub fn fetch_raw(&mut self, uid: u32) -> Result<Vec<u8>> {
        let fetches = self
            .inner
            .uid_fetch(uid.to_string(), "BODY[]")
            .with_context(|| format!("UID FETCH {uid} 正文失败"))?;

        let body = fetches
            .iter()
            .next()
            .and_then(|f| f.body())
            .with_context(|| format!("UID {uid} 无正文"))?;

        Ok(body.to_vec())
    }
}

/// IMAP ENVELOPE 里的头字段是 RFC 2047 编码的字节串。
/// 交给 mail-parser 解码，它同时处理 Base64/QP 和 GB18030 等字符集。
fn decode_header_bytes(raw: &[u8]) -> String {
    let synthetic = [b"Subject: ".as_slice(), raw, b"\r\n\r\n"].concat();
    mail_parser::MessageParser::default()
        .parse(&synthetic)
        .and_then(|m| m.subject().map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(raw).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_payload_follows_rfc2971_shape() {
        let payload = id_command_payload();
        assert!(payload.starts_with("ID ("));
        assert!(payload.contains("\"name\""));
        assert!(payload.ends_with(')'));
    }

    #[test]
    fn plain_folder_name_is_not_quoted() {
        assert_eq!(quote_folder("INBOX"), "INBOX");
        assert_eq!(quote_folder("INBOX/Sub"), "INBOX/Sub");
    }

    #[test]
    fn folder_with_space_is_quoted() {
        assert_eq!(quote_folder("Sent Messages"), "\"Sent Messages\"");
    }

    #[test]
    fn chinese_folder_name_is_quoted() {
        assert_eq!(quote_folder("发票"), "\"发票\"");
    }

    #[test]
    fn embedded_quote_is_escaped() {
        assert_eq!(quote_folder(r#"a"b"#), r#""a\"b""#);
    }
}
