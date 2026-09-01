use crate::config::{DateRange, ImapConfig};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

const MAILBOX_OPEN_COMMAND: &str = "EXAMINE";
const RAW_BODY_FETCH_ITEMS: &str = "BODY.PEEK[]";
const FINGERPRINT_FETCH_ITEMS: &str = "(FLAGS RFC822.SIZE ENVELOPE)";

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
    if name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '.')
    {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('\\', r"\\").replace('"', r#"\""#))
    }
}

fn raw_command_is_allowed(command: &str) -> bool {
    command
        .split_ascii_whitespace()
        .next()
        .is_some_and(|verb| verb.eq_ignore_ascii_case("ID"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadOnlyMessageFingerprint {
    uid: u32,
    flags: Vec<String>,
    rfc822_size: Option<u32>,
    message_id_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadOnlyFingerprint {
    folder: String,
    search_query: String,
    uid_validity: Option<u32>,
    exists: u32,
    messages: Vec<ReadOnlyMessageFingerprint>,
}

pub struct Session {
    inner: imap::Session<Box<dyn imap::ImapConnection>>,
    fingerprint: Option<ReadOnlyFingerprint>,
}

impl Session {
    pub fn connect(cfg: &ImapConfig) -> Result<Self> {
        if let Some(warning) = cfg.warn_if_password_looks_wrong() {
            eprintln!("警告: {warning}");
        }

        let client = imap::ClientBuilder::new(&cfg.host, cfg.port)
            .connect()
            .with_context(|| format!("连接 {}:{} 失败", cfg.host, cfg.port))?;

        let mut inner = client
            .login(&cfg.username, &cfg.password)
            .map_err(|(e, _)| {
                anyhow::anyhow!(
                    "登录失败: {e}。若为 QQ 邮箱，请确认：\
                 (1) 已在 设置 → 账户 中开启 IMAP 服务；\
                 (2) 密码用的是 16 位授权码而非登录密码"
                )
            })?;

        // 腾讯/网易服务器要求：SELECT 之前先发 ID，否则报 Unsafe Login。
        // 所有原始命令必须先过白名单；其他操作只使用下面封装的只读类型化 API。
        // 发送失败不致命 —— 有些服务器不认 ID 命令，忽略即可。
        let id_command = id_command_payload();
        anyhow::ensure!(
            raw_command_is_allowed(&id_command),
            "IMAP 原始命令被只读策略阻止"
        );
        if let Err(e) = inner.run_command_and_check_ok(id_command) {
            eprintln!("提示: ID 命令未被接受（{e}），继续尝试");
        }

        Ok(Session {
            inner,
            fingerprint: None,
        })
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
        let mailbox = self
            .inner
            .examine(quote_folder(folder))
            .with_context(|| format!("{MAILBOX_OPEN_COMMAND} {folder} 失败"))?;
        anyhow::ensure!(mailbox.is_read_only, "IMAP 服务器未确认邮箱处于只读状态");

        let query = range.to_imap_search();
        let mut uids: Vec<u32> = self
            .inner
            .uid_search(&query)
            .with_context(|| format!("UID SEARCH {query} 失败"))?
            .into_iter()
            .collect();
        uids.sort_unstable();
        let messages = self.fingerprint_messages(&uids)?;
        self.fingerprint = Some(ReadOnlyFingerprint {
            folder: folder.to_string(),
            search_query: query,
            uid_validity: mailbox.uid_validity,
            exists: mailbox.exists,
            messages,
        });
        Ok(uids)
    }

    fn fingerprint_messages(&mut self, uids: &[u32]) -> Result<Vec<ReadOnlyMessageFingerprint>> {
        let mut fingerprint = Vec::new();
        for chunk in uids.chunks(100) {
            if chunk.is_empty() {
                continue;
            }
            let set = chunk
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetches = self
                .inner
                .uid_fetch(&set, FINGERPRINT_FETCH_ITEMS)
                .with_context(|| "读取邮件只读指纹失败")?;
            for fetch in fetches.iter() {
                let Some(uid) = fetch.uid else {
                    continue;
                };
                let mut flags = fetch
                    .flags()
                    .iter()
                    .map(ToString::to_string)
                    .filter(|flag| !flag.eq_ignore_ascii_case(r"\Recent"))
                    .collect::<Vec<_>>();
                flags.sort_unstable();
                let message_id_sha256 = fetch
                    .envelope()
                    .and_then(|envelope| envelope.message_id.as_deref())
                    .map(sha256_bytes)
                    .unwrap_or_else(|| sha256_bytes(b""));
                fingerprint.push(ReadOnlyMessageFingerprint {
                    uid,
                    flags,
                    rfc822_size: fetch.size,
                    message_id_sha256,
                });
            }
        }
        fingerprint.sort_unstable_by_key(|message| message.uid);
        anyhow::ensure!(
            fingerprint.len() == uids.len(),
            "邮箱只读指纹不完整：预期 {} 封，实际 {} 封",
            uids.len(),
            fingerprint.len()
        );
        Ok(fingerprint)
    }

    /// 再次读取目标范围的 UID、FLAGS、大小和 Message-ID；任何变化都按只读验证失败处理。
    pub fn verify_read_only_unchanged(&mut self, folder: &str) -> Result<String> {
        let before = self.fingerprint.clone().context("尚未建立邮箱只读指纹")?;
        anyhow::ensure!(before.folder == folder, "只读指纹文件夹不匹配");
        let mailbox = self
            .inner
            .examine(quote_folder(folder))
            .with_context(|| format!("{MAILBOX_OPEN_COMMAND} {folder} 复核失败"))?;
        anyhow::ensure!(
            mailbox.is_read_only,
            "IMAP 服务器未确认复核会话处于只读状态"
        );
        let mut uids = self
            .inner
            .uid_search(&before.search_query)
            .with_context(|| format!("UID SEARCH {} 复核失败", before.search_query))?
            .into_iter()
            .collect::<Vec<_>>();
        uids.sort_unstable();
        let after = ReadOnlyFingerprint {
            folder: folder.to_string(),
            search_query: before.search_query.clone(),
            uid_validity: mailbox.uid_validity,
            exists: mailbox.exists,
            messages: self.fingerprint_messages(&uids)?,
        };
        anyhow::ensure!(
            before == after,
            "邮箱 UIDVALIDITY、计数、UID、FLAGS、大小或 Message-ID 在读取前后发生变化；已停止流程，请检查服务器或其他客户端"
        );
        Ok(fingerprint_sha256(&before))
    }

    pub fn fetch_summaries(&mut self, uids: &[u32]) -> Result<Vec<MessageSummary>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }

        // 分批获取，避免一次请求过多导致服务器断连
        let batch_size = 10;
        let mut out = Vec::new();

        for (batch_idx, chunk) in uids.chunks(batch_size).enumerate() {
            let set = chunk
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            // QQ 邮箱服务器对 BODYSTRUCTURE 支持不稳定，容易断连，
            // 这里只获取基本信息，附件判断移到 fetch_raw 时进行
            let fetches = self
                .inner
                .uid_fetch(&set, "(INTERNALDATE ENVELOPE)")
                .with_context(|| {
                    format!(
                        "UID FETCH 概要失败（批次 {}, UIDs: {}）",
                        batch_idx + 1,
                        set
                    )
                })?;

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
                        let mailbox = a
                            .mailbox
                            .as_deref()
                            .map(decode_header_bytes)
                            .unwrap_or_default();
                        let host = a
                            .host
                            .as_deref()
                            .map(decode_header_bytes)
                            .unwrap_or_default();
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
            .uid_fetch(uid.to_string(), RAW_BODY_FETCH_ITEMS)
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
fn fingerprint_sha256(fingerprint: &ReadOnlyFingerprint) -> String {
    let mut hasher = Sha256::new();
    hasher.update(fingerprint.folder.as_bytes());
    hasher.update([0]);
    hasher.update(fingerprint.search_query.as_bytes());
    hasher.update(fingerprint.uid_validity.unwrap_or_default().to_be_bytes());
    hasher.update(fingerprint.exists.to_be_bytes());
    for message in &fingerprint.messages {
        hasher.update(message.uid.to_be_bytes());
        hasher.update(message.rfc822_size.unwrap_or_default().to_be_bytes());
        hasher.update(message.message_id_sha256.as_bytes());
        for flag in &message.flags {
            hasher.update([0]);
            hasher.update(flag.as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

fn sha256_bytes(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

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

    #[test]
    fn raw_command_policy_only_allows_id() {
        assert!(raw_command_is_allowed(&id_command_payload()));
        for command in [
            "STORE 1 +FLAGS (Seen)",
            "EXPUNGE",
            "APPEND INBOX",
            "DELETE INBOX",
            "MOVE 1 Archive",
        ] {
            assert!(!raw_command_is_allowed(command), "不应允许 {command}");
        }
    }

    #[test]
    fn client_source_has_no_mutating_typed_calls_or_non_peek_body_fetch() {
        let source = include_str!("imap_client.rs");
        for verb in [
            "select", "store", "expunge", "append", "delete", "copy", "mv",
        ] {
            let token = format!(".{verb}(");
            assert!(!source.contains(&token), "发现禁止的 IMAP 调用 {token}");
        }
        let mutating_fetch = ["BODY", "[]"].concat();
        let quoted = format!("\"{mutating_fetch}\"");
        assert!(!source.contains(&quoted));
        assert_eq!(MAILBOX_OPEN_COMMAND, "EXAMINE");
        assert_eq!(RAW_BODY_FETCH_ITEMS, "BODY.PEEK[]");
        assert_eq!(FINGERPRINT_FETCH_ITEMS, "(FLAGS RFC822.SIZE ENVELOPE)");
    }

    #[test]
    fn fingerprint_hash_covers_mailbox_and_message_invariants() {
        let baseline = ReadOnlyFingerprint {
            folder: "INBOX".to_string(),
            search_query: "SINCE 01-Jun-2026 BEFORE 01-Jul-2026".to_string(),
            uid_validity: Some(42),
            exists: 7,
            messages: vec![ReadOnlyMessageFingerprint {
                uid: 3,
                flags: vec!["\\Seen".to_string()],
                rfc822_size: Some(1024),
                message_id_sha256: sha256_bytes(b"<synthetic@example.invalid>"),
            }],
        };

        let baseline_hash = fingerprint_sha256(&baseline);
        for changed in [
            ReadOnlyFingerprint {
                exists: 8,
                ..baseline.clone()
            },
            ReadOnlyFingerprint {
                uid_validity: Some(43),
                ..baseline.clone()
            },
            ReadOnlyFingerprint {
                messages: vec![ReadOnlyMessageFingerprint {
                    rfc822_size: Some(1025),
                    ..baseline.messages[0].clone()
                }],
                ..baseline.clone()
            },
            ReadOnlyFingerprint {
                messages: vec![ReadOnlyMessageFingerprint {
                    message_id_sha256: sha256_bytes(b"<other@example.invalid>"),
                    ..baseline.messages[0].clone()
                }],
                ..baseline.clone()
            },
        ] {
            assert_ne!(baseline_hash, fingerprint_sha256(&changed));
        }
    }
}
