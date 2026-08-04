# 邮箱发票采集器 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现产品方案 M1 的邮件采集能力，并用它产出解析验证计划所需的真实发票样本集与清单骨架。

**Architecture:** 一个 Rust CLI crate `invoice-collect`。通过 IMAP 连接邮箱，按日期范围检索邮件，用「发件人白名单 + 附件特征」两级确定性漏斗筛出发票附件，三重键去重后落盘，并生成 `fixtures/manifest.toml` 骨架供人工补填期望值。七个任务中只有两个需要联网，其余用内置的合成 `.eml` 夹具离线 TDD。

**Tech Stack:** Rust 2021 · `imap`（IMAP 协议）· `mail-parser`（MIME/RFC 2047/字符集解码）· `sha2`（附件指纹）· `chrono`（日期）· `toml` + `serde`（清单输出）

## Global Constraints

- Rust edition 2021，MSRV 1.75+
- **凭证只从环境变量 `INVOICE_IMAP_PASSWORD` 读取**。不写入任何文件、不进 git、不打印到日志或 stdout
- **任何真实凭证（密码、授权码、账号）都不得出现在本文档、代码、注释或测试里**。测试一律用明显是假的占位值
- 邮箱地址通过命令行参数传入，不硬编码
- QQ 邮箱的 IMAP 密码必须是 **16 位授权码**，不是账号登录密码。授权码在 设置 → 账户 → POP3/IMAP/SMTP 服务 中生成
- 采集到的发票文件属于个人财务数据，`fixtures/samples/` 必须 gitignore；仓库只提交清单结构与测试代码
- 目标检索范围：**2026-06-01 至 2026-06-30**（IMAP `SINCE 1-Jun-2026 BEFORE 1-Jul-2026`）
- 所有字段名、结构体名用英文；面向用户的输出文案用中文
- 每个任务结束时 commit，message 用英文，格式 `feat:` / `test:` / `chore:`

---

## 前置动作（执行 Task 2 之前必须完成）

1. **确认账号登录密码未在任何可留存的渠道暴露过**；若有，先轮换
2. **开启 IMAP 服务并生成授权码** —— QQ 邮箱 设置 → 账户 → POP3/IMAP/SMTP 服务 → 开启 IMAP/SMTP → 生成授权码（16 位小写字母）
3. **导出到环境变量**（不要写进 shell 配置文件，用完 `unset`）：

```bash
read -rs INVOICE_IMAP_PASSWORD && export INVOICE_IMAP_PASSWORD
```

用 `read -rs` 而不是 `export VAR='值'`：后者会把授权码留在 shell history 里。

Task 1 和 Task 3–6 不需要以上任何一项，可以先做。

---

## 与解析验证计划的关系

本计划**先于** `2026-08-03-invoice-parse-spike.md` 执行，因为后者的 Task 4–9 全部阻塞在"没有真实发票样本"上。

```
本计划 Task 7  ──产出──►  fixtures/samples/*        （真实发票文件）
                          fixtures/manifest.toml    （清单骨架，期望值待填）
                                    │
                          人工补填期望值（打开每张票抄字段）
                                    │
                                    ▼
                     解析验证计划 Task 4–9 解除阻塞
```

**Workspace 归属**：本计划创建 workspace root 和 `crates/invoice-collect`。解析验证计划的 Task 1 届时只需**追加** `crates/invoice-parse` 到 `members`，不要重建 root。

## File Structure

```
work-tools/
├── Cargo.toml                              # workspace root（本计划创建）
├── .gitignore
├── crates/
│   └── invoice-collect/
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs                     # CLI 入口：probe / collect
│           ├── lib.rs
│           ├── config.rs                   # 环境变量凭证与检索参数
│           ├── imap_client.rs              # 连接、ID 命令、SEARCH、FETCH
│           ├── extract.rs                  # MIME 解析与附件提取
│           ├── classify.rs                 # 两级漏斗：发件人白名单 + 附件特征
│           ├── dedupe.rs                   # 三重键去重
│           ├── store.rs                    # 落盘与文件命名
│           └── manifest_gen.rs             # 清单骨架生成
└── fixtures/
    ├── manifest.toml                       # 生成物（提交）
    └── samples/                            # 生成物（不提交）
```

**职责边界**：

- `imap_client.rs` 是唯一联网的模块。其余模块输入都是内存里的字节，可离线测
- `extract.rs` 只做 MIME → `Vec<RawAttachment>`，不判断是不是发票
- `classify.rs` 只判断"是否发票 + 猜测格式"，不读文件内容以外的东西
- `dedupe.rs`、`store.rs`、`manifest_gen.rs` 各自纯函数化，输入输出明确

这个划分让 Task 3–6 完全离线可测——只有 Task 2 和 Task 7 需要真实邮箱。

---

## Task 1: Workspace 骨架与配置

**Files:**
- Create: `Cargo.toml`
- Create: `.gitignore`
- Create: `crates/invoice-collect/Cargo.toml`
- Create: `crates/invoice-collect/src/lib.rs`
- Create: `crates/invoice-collect/src/config.rs`
- Create: `crates/invoice-collect/src/main.rs`
- Test: `crates/invoice-collect/src/config.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: 无（首个任务）
- Produces:
  - `config::ImapConfig { host: String, port: u16, username: String, password: String }`
  - `config::ImapConfig::from_env(username: &str) -> anyhow::Result<ImapConfig>`
  - `config::DateRange { since: NaiveDate, before: NaiveDate }`
  - `config::DateRange::parse(since: &str, before: &str) -> anyhow::Result<DateRange>`
  - `config::DateRange::to_imap_search(&self) -> String`

- [ ] **Step 1: 创建 workspace root**

创建 `Cargo.toml`：

```toml
[workspace]
members = ["crates/invoice-collect"]
resolver = "2"

[workspace.package]
edition = "2021"
rust-version = "1.75"

[workspace.dependencies]
anyhow = "1.0"
thiserror = "2.0"
chrono = "0.4"
serde = { version = "1.0", features = ["derive"] }
toml = "0.8"
sha2 = "0.10"
imap = "3.0.0-alpha.15"
mail-parser = "0.9"
```

`imap` 3.x 仍是 alpha，但它是同步 API，对 CLI 工具比 async 简单得多。若届时有稳定版，用稳定版。

- [ ] **Step 2: 创建 .gitignore**

创建 `.gitignore`：

```
/target
fixtures/samples/
.env
*.eml
```

- [ ] **Step 3: 创建 crate 清单**

创建 `crates/invoice-collect/Cargo.toml`：

```toml
[package]
name = "invoice-collect"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true

[dependencies]
anyhow.workspace = true
thiserror.workspace = true
chrono.workspace = true
serde.workspace = true
toml.workspace = true
sha2.workspace = true
imap.workspace = true
mail-parser.workspace = true

[[bin]]
name = "invoice-collect"
path = "src/main.rs"
```

- [ ] **Step 4: 写 config.rs 与失败测试**

创建 `crates/invoice-collect/src/config.rs`：

```rust
use anyhow::{bail, Context};
use chrono::NaiveDate;

/// IMAP 连接参数。password 只从环境变量读入，不做任何持久化。
#[derive(Clone)]
pub struct ImapConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

// 手工实现 Debug，避免密码被日志或 panic 信息带出去
impl std::fmt::Debug for ImapConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

pub const ENV_PASSWORD: &str = "INVOICE_IMAP_PASSWORD";

impl ImapConfig {
    /// 按邮箱域名推断服务器。密码从 `INVOICE_IMAP_PASSWORD` 读取。
    pub fn from_env(username: &str) -> anyhow::Result<Self> {
        let password = std::env::var(ENV_PASSWORD).with_context(|| {
            format!("环境变量 {ENV_PASSWORD} 未设置。QQ 邮箱需填 16 位授权码，不是登录密码")
        })?;

        if password.trim().is_empty() {
            bail!("{ENV_PASSWORD} 为空");
        }

        let domain = username
            .rsplit('@')
            .next()
            .filter(|d| *d != username)
            .with_context(|| format!("{username} 不是合法邮箱地址"))?;

        let host = match domain.to_lowercase().as_str() {
            "qq.com" | "vip.qq.com" | "foxmail.com" => "imap.qq.com",
            "163.com" => "imap.163.com",
            "126.com" => "imap.126.com",
            "gmail.com" => "imap.gmail.com",
            "outlook.com" | "hotmail.com" => "outlook.office365.com",
            other => bail!("暂不支持的邮箱域名 {other}，请手工指定 host"),
        }
        .to_string();

        Ok(ImapConfig {
            host,
            port: 993,
            username: username.to_string(),
            password,
        })
    }

    /// QQ 邮箱要求授权码为 16 位小写字母。不符合时给出可操作的提示。
    pub fn warn_if_password_looks_wrong(&self) -> Option<String> {
        if !self.host.contains("qq.com") {
            return None;
        }
        let p = &self.password;
        if p.len() == 16 && p.chars().all(|c| c.is_ascii_lowercase()) {
            None
        } else {
            Some(format!(
                "QQ 邮箱的 IMAP 密码应为 16 位小写授权码，当前值长度 {}。\
                 请在 设置 → 账户 → POP3/IMAP/SMTP 服务 中生成授权码",
                p.len()
            ))
        }
    }
}

/// 检索日期范围。半开区间 [since, before)。
#[derive(Debug, Clone, PartialEq)]
pub struct DateRange {
    pub since: NaiveDate,
    pub before: NaiveDate,
}

impl DateRange {
    pub fn parse(since: &str, before: &str) -> anyhow::Result<Self> {
        let since = NaiveDate::parse_from_str(since, "%Y-%m-%d")
            .with_context(|| format!("起始日期 {since} 不是 YYYY-MM-DD 格式"))?;
        let before = NaiveDate::parse_from_str(before, "%Y-%m-%d")
            .with_context(|| format!("结束日期 {before} 不是 YYYY-MM-DD 格式"))?;

        if before <= since {
            bail!("结束日期 {before} 必须晚于起始日期 {since}");
        }
        Ok(DateRange { since, before })
    }

    /// 转成 IMAP SEARCH 条件。IMAP 日期格式为 DD-Mon-YYYY，月份是英文缩写。
    /// 注意 SEARCH 作用于 INTERNALDATE（服务器收件时间），不是邮件头的 Date。
    pub fn to_imap_search(&self) -> String {
        format!(
            "SINCE {} BEFORE {}",
            self.since.format("%d-%b-%Y"),
            self.before.format("%d-%b-%Y")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 测试一律用明显是假的占位值。真实账号和真实凭证
    // 不得出现在代码、注释或测试里。
    const FAKE_QQ_USER: &str = "test-user@qq.com";
    /// 形状合法的假授权码：16 位小写字母
    const FAKE_AUTH_CODE: &str = "zzzzplaceholder1";
    /// 形状不合法的假密码，用于触发告警分支
    const FAKE_BAD_SHAPE: &str = "NotAnAuthCode!";

    #[test]
    fn qq_domain_maps_to_qq_imap_host() {
        std::env::set_var(ENV_PASSWORD, FAKE_AUTH_CODE);
        let cfg = ImapConfig::from_env(FAKE_QQ_USER).unwrap();
        assert_eq!(cfg.host, "imap.qq.com");
        assert_eq!(cfg.port, 993);
        assert_eq!(cfg.username, FAKE_QQ_USER);
    }

    #[test]
    fn debug_output_never_contains_password() {
        let sentinel = "sentinel-must-not-appear";
        std::env::set_var(ENV_PASSWORD, sentinel);
        let cfg = ImapConfig::from_env(FAKE_QQ_USER).unwrap();
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains(sentinel), "密码泄漏到 Debug: {rendered}");
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn missing_env_var_mentions_authorization_code() {
        std::env::remove_var(ENV_PASSWORD);
        let err = ImapConfig::from_env(FAKE_QQ_USER).unwrap_err();
        assert!(err.to_string().contains("授权码"), "实际: {err}");
    }

    #[test]
    fn account_password_shape_triggers_warning() {
        // 账号登录密码的典型形状（含大写和符号）不符合授权码规则
        std::env::set_var(ENV_PASSWORD, FAKE_BAD_SHAPE);
        let cfg = ImapConfig::from_env(FAKE_QQ_USER).unwrap();
        let warning = cfg.warn_if_password_looks_wrong().expect("应产生告警");
        assert!(warning.contains("16 位"), "实际: {warning}");
    }

    #[test]
    fn valid_authorization_code_produces_no_warning() {
        std::env::set_var(ENV_PASSWORD, FAKE_AUTH_CODE);
        let cfg = ImapConfig::from_env(FAKE_QQ_USER).unwrap();
        assert!(cfg.warn_if_password_looks_wrong().is_none());
    }

    #[test]
    fn june_2026_range_renders_imap_search() {
        let range = DateRange::parse("2026-06-01", "2026-07-01").unwrap();
        assert_eq!(range.to_imap_search(), "SINCE 01-Jun-2026 BEFORE 01-Jul-2026");
    }

    #[test]
    fn inverted_range_is_rejected() {
        let err = DateRange::parse("2026-07-01", "2026-06-01").unwrap_err();
        assert!(err.to_string().contains("必须晚于"), "实际: {err}");
    }

    #[test]
    fn unsupported_domain_is_rejected() {
        std::env::set_var(ENV_PASSWORD, "x");
        let err = ImapConfig::from_env("someone@example.org").unwrap_err();
        assert!(err.to_string().contains("暂不支持"), "实际: {err}");
    }
}
```

创建 `crates/invoice-collect/src/lib.rs`：

```rust
pub mod config;
```

- [ ] **Step 5: 运行测试确认失败**

Run: `cargo test -p invoice-collect config`
Expected: 编译失败——`main.rs` 尚不存在

- [ ] **Step 6: 建最小 main.rs**

创建 `crates/invoice-collect/src/main.rs`：

```rust
fn main() -> anyhow::Result<()> {
    eprintln!("用法: invoice-collect probe|collect");
    Ok(())
}
```

- [ ] **Step 7: 运行测试确认通过**

Run: `cargo test -p invoice-collect config`
Expected: 8 个测试全部 PASS

测试用 `std::env::set_var` 会相互干扰，若出现偶发失败，改用 `cargo test -- --test-threads=1` 确认，并在报告中记录。

- [ ] **Step 8: Commit**

```bash
git init
git add Cargo.toml .gitignore crates/ docs/
git commit -m "chore: scaffold invoice-collect workspace with redacting config"
```

---

## Task 2: IMAP 连通性探测（联网，含决策关口）

**Files:**
- Create: `crates/invoice-collect/src/imap_client.rs`
- Modify: `crates/invoice-collect/src/lib.rs`
- Modify: `crates/invoice-collect/src/main.rs`
- Test: `crates/invoice-collect/src/imap_client.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: `config::{ImapConfig, DateRange}`（Task 1）
- Produces:
  - `imap_client::MessageSummary { uid: u32, subject: String, from: String, internal_date: String, has_attachments: bool }`
  - `imap_client::Session::connect(cfg: &ImapConfig) -> anyhow::Result<Session>`
  - `imap_client::Session::list_folders(&mut self) -> anyhow::Result<Vec<String>>`
  - `imap_client::Session::search_range(&mut self, folder: &str, range: &DateRange) -> anyhow::Result<Vec<u32>>`
  - `imap_client::Session::fetch_summaries(&mut self, uids: &[u32]) -> anyhow::Result<Vec<MessageSummary>>`
  - `imap_client::Session::fetch_raw(&mut self, uid: u32) -> anyhow::Result<Vec<u8>>`

**这个任务是决策关口**：它回答"2026 年 6 月这个邮箱里到底有没有发票"。如果没有，后续任务的样本来源不成立，必须先换范围或换来源，不能盲目往下做。

- [ ] **Step 1: 写可离线测试的部分**

`Session` 的方法都要联网，无法单元测试。但 `ID` 命令的构造和文件夹名的引用规则可以离线测。

创建 `crates/invoice-collect/src/imap_client.rs`：

```rust
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
        unimplemented!()
    }

    pub fn list_folders(&mut self) -> Result<Vec<String>> {
        unimplemented!()
    }

    pub fn search_range(&mut self, folder: &str, range: &DateRange) -> Result<Vec<u32>> {
        unimplemented!()
    }

    pub fn fetch_summaries(&mut self, uids: &[u32]) -> Result<Vec<MessageSummary>> {
        unimplemented!()
    }

    pub fn fetch_raw(&mut self, uid: u32) -> Result<Vec<u8>> {
        unimplemented!()
    }
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
```

在 `lib.rs` 追加 `pub mod imap_client;`。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p invoice-collect imap_client`
Expected: FAIL，5 个测试因 `unimplemented!()` 之外的编译问题或直接 panic

- [ ] **Step 3: 实现 Session**

替换 `imap_client.rs` 里的五处 `unimplemented!()`：

```rust
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
        let set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let fetches = self
            .inner
            .uid_fetch(&set, "(INTERNALDATE BODYSTRUCTURE ENVELOPE)")
            .context("UID FETCH 概要失败")?;

        let mut out = Vec::new();
        for f in fetches.iter() {
            let uid = f.uid.unwrap_or(0);
            let envelope = f.envelope();

            let subject = envelope
                .and_then(|e| e.subject)
                .map(|s| decode_header_bytes(s))
                .unwrap_or_else(|| "(无主题)".to_string());

            let from = envelope
                .and_then(|e| e.from.as_ref())
                .and_then(|addrs| addrs.first())
                .map(|a| {
                    let mailbox = a.mailbox.map(decode_header_bytes).unwrap_or_default();
                    let host = a.host.map(decode_header_bytes).unwrap_or_default();
                    format!("{mailbox}@{host}")
                })
                .unwrap_or_else(|| "(未知发件人)".to_string());

            let internal_date = f
                .internal_date()
                .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "(无日期)".to_string());

            // BODYSTRUCTURE 里出现 multipart 通常意味着有附件。
            // 这只是概要判断，真正的附件提取在 Task 3。
            let has_attachments = f
                .bodystructure()
                .map(|bs| format!("{bs:?}").to_lowercase().contains("attachment"))
                .unwrap_or(false);

            out.push(MessageSummary {
                uid,
                subject,
                from,
                internal_date,
                has_attachments,
            });
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
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p invoice-collect imap_client`
Expected: 5 个测试全部 PASS

- [ ] **Step 5: 加 probe 子命令**

替换 `crates/invoice-collect/src/main.rs`：

```rust
use anyhow::{bail, Context};
use invoice_collect::config::{DateRange, ImapConfig};
use invoice_collect::imap_client::Session;

const DEFAULT_SINCE: &str = "2026-06-01";
const DEFAULT_BEFORE: &str = "2026-07-01";

const USAGE: &str = "用法:
  invoice-collect probe   <邮箱地址> [起始日期 结束日期]
  invoice-collect collect <邮箱地址> [起始日期 结束日期]

日期格式 YYYY-MM-DD，默认 2026-06-01 至 2026-07-01（半开区间）。
密码从环境变量 INVOICE_IMAP_PASSWORD 读取。QQ 邮箱需填 16 位授权码。";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("probe") => probe(&parse_target(&args)?),
        Some(other) => bail!("未知子命令: {other}\n\n{USAGE}"),
        None => {
            eprintln!("{USAGE}");
            Ok(())
        }
    }
}

/// 邮箱地址与日期范围都由命令行给出 —— 账号不硬编码进源码。
struct Target {
    username: String,
    range: DateRange,
}

fn parse_target(args: &[String]) -> anyhow::Result<Target> {
    let username = args
        .get(2)
        .with_context(|| format!("缺少邮箱地址参数\n\n{USAGE}"))?
        .clone();

    let since = args.get(3).map(String::as_str).unwrap_or(DEFAULT_SINCE);
    let before = args.get(4).map(String::as_str).unwrap_or(DEFAULT_BEFORE);

    Ok(Target {
        username,
        range: DateRange::parse(since, before)?,
    })
}

fn probe(target: &Target) -> anyhow::Result<()> {
    let cfg = ImapConfig::from_env(&target.username)?;
    let range = target.range.clone();

    println!("连接 {}:{} 账号 {}", cfg.host, cfg.port, cfg.username);
    let mut session = Session::connect(&cfg).context("建立 IMAP 会话失败")?;
    println!("登录成功\n");

    println!("可用文件夹：");
    for name in session.list_folders()? {
        println!("  {name}");
    }

    let uids = session.search_range("INBOX", &range)?;
    println!(
        "\nINBOX 中 {} 至 {} 共 {} 封邮件",
        range.since, range.before, uids.len()
    );

    if uids.is_empty() {
        println!("\n该范围内没有邮件。需要调整日期范围或更换样本来源。");
        return Ok(());
    }

    println!("\n{:<8} {:<18} {:<32} 附件  主题", "UID", "收件时间", "发件人");
    for s in session.fetch_summaries(&uids)? {
        let flag = if s.has_attachments { "有" } else { "无" };
        let subject: String = s.subject.chars().take(40).collect();
        println!(
            "{:<8} {:<18} {:<32} {:<4}  {}",
            s.uid, s.internal_date, s.from, flag, subject
        );
    }
    Ok(())
}
```

- [ ] **Step 6: 对真实邮箱运行探测**

先按「前置动作」把授权码读入环境变量，然后：

Run: `cargo run -p invoice-collect -- probe <你的邮箱地址>`

邮箱地址作为参数传入。**不要把它写进任何文件或提交记录。**

**这是决策关口。四种结果，处理方式不同：**

| 观察 | 含义 | 处理 |
|---|---|---|
| 登录失败提示授权码 | 密码不是授权码，或 IMAP 未开启 | 回到「前置动作」生成授权码 |
| 报 `Unsafe Login` | ID 命令未生效 | 检查 `run_command_and_check_ok` 是否真的发出；必要时换 `imap` crate 版本 |
| 登录成功但 6 月 0 封 | 该月无邮件 | **改用有邮件的月份**，把 `DEFAULT_SINCE`/`DEFAULT_BEFORE` 调整后重跑 |
| 有邮件但"附件"全为无 | 发票可能在正文链接里，不是附件 | **记录下来**：这类邮件需要下载链接跟踪，超出本计划范围，需单独处理 |

第三、四种情况必须在继续前解决——否则 Task 7 采集不到任何样本。

- [ ] **Step 7: Commit**

```bash
git add crates/invoice-collect/src/imap_client.rs crates/invoice-collect/src/lib.rs \
        crates/invoice-collect/src/main.rs
git commit -m "feat: add IMAP session with ID command and date-range search"
```

---

## Task 3: MIME 解析与附件提取（离线）

**Files:**
- Create: `crates/invoice-collect/src/extract.rs`
- Modify: `crates/invoice-collect/src/lib.rs`
- Test: `crates/invoice-collect/src/extract.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: 无（只依赖 `mail-parser`）
- Produces:
  - `extract::RawAttachment { filename: String, content_type: String, data: Vec<u8> }`
  - `extract::ExtractedEmail { message_id: Option<String>, subject: String, from: String, attachments: Vec<RawAttachment> }`
  - `extract::extract_email(raw: &[u8]) -> anyhow::Result<ExtractedEmail>`

**难点是中文文件名**。国内开票平台的附件名常见三种编码方式，都要支持：

| 形式 | 例子 | 出处 |
|---|---|---|
| RFC 2047 + UTF-8 | `=?UTF-8?B?5Y+R56Wo?=.pdf` | 多数平台 |
| RFC 2047 + GB18030 | `=?GB18030?B?t6LGsQ==?=.pdf` | 老系统 |
| RFC 2231 | `filename*=UTF-8''%E5%8F%91%E7%A5%A8.pdf` | 规范做法 |

`mail-parser` 三种都能解，但必须用测试锁住——文件名错乱会导致后续分类和落盘全乱。

- [ ] **Step 1: 写失败测试**

创建 `crates/invoice-collect/src/extract.rs`：

```rust
use anyhow::{Context, Result};

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
    unimplemented!()
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
```

在 `lib.rs` 追加 `pub mod extract;`。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p invoice-collect extract`
Expected: FAIL，panic 于 `not implemented`

- [ ] **Step 3: 实现 extract_email**

替换 `extract.rs` 里的 `unimplemented!()`：

```rust
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
```

`mail-parser` 的 `attachments()` 已经处理了嵌套 multipart、Base64/QP 解码、以及 RFC 2047/2231 文件名解码。`contents()` 返回的是解码后的原始字节。

若某个测试的文件名断言失败，说明 `mail-parser` 对该编码形式的处理与预期不同——**先打印实际值再调整**，不要改测试期望去迁就实现：

```rust
eprintln!("实际文件名: {:?}", email.attachments[0].filename);
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p invoice-collect extract`
Expected: 8 个测试全部 PASS

- [ ] **Step 5: Commit**

```bash
git add crates/invoice-collect/src/extract.rs crates/invoice-collect/src/lib.rs
git commit -m "feat: extract MIME attachments with CJK filename decoding"
```

---

## Task 4: 发票分类漏斗（离线）

**Files:**
- Create: `crates/invoice-collect/src/classify.rs`
- Modify: `crates/invoice-collect/src/lib.rs`
- Test: `crates/invoice-collect/src/classify.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: `extract::{ExtractedEmail, RawAttachment}`（Task 3）
- Produces:
  - `classify::SampleFormat { Xml, Ofd, PdfRail, PdfFlight, PdfVat, Image }`
  - `classify::Classification { format: SampleFormat, platform: String, reason: MatchReason }`
  - `classify::MatchReason { SenderWhitelist, AttachmentFeature }`
  - `classify::classify_attachment(email: &ExtractedEmail, att: &RawAttachment) -> Option<Classification>`
  - `classify::platform_of_sender(from: &str) -> Option<&'static str>`

**只做产品方案 M1 的前两级漏斗**（发件人白名单 + 附件特征），不做第三级 LLM 判定。理由：本计划的目的是采集样本，宁可漏掉少数边缘邮件，也不引入 API 依赖和成本。漏掉的可以人工补。

`SampleFormat` 的取值与解析验证计划的 `fixtures/manifest.toml` 的 `format` 字段一一对应——这是两个计划的接缝。

- [ ] **Step 1: 写失败测试**

创建 `crates/invoice-collect/src/classify.rs`：

```rust
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

/// 发件人域名 → 平台标识。用于文件命名和统计。
pub fn platform_of_sender(from: &str) -> Option<&'static str> {
    unimplemented!()
}

/// 判断一个附件是否为发票，并推断其格式。
pub fn classify_attachment(
    email: &ExtractedEmail,
    att: &RawAttachment,
) -> Option<Classification> {
    unimplemented!()
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
        let c = classify_attachment(&email, &att("发票.jpg", "image/jpeg")).unwrap();
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
```

在 `lib.rs` 追加 `pub mod classify;`。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p invoice-collect classify`
Expected: FAIL，panic 于 `not implemented`

- [ ] **Step 3: 实现分类漏斗**

替换 `classify.rs` 的两处 `unimplemented!()`：

```rust
/// 发件人域名白名单。产品方案要求这份列表可云端更新，
/// MVP 阶段先内置 —— 本采集工具只需覆盖常见平台。
const SENDER_WHITELIST: &[(&str, &str)] = &[
    ("12306.cn", "12306"),
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

pub fn platform_of_sender(from: &str) -> Option<&'static str> {
    let lower = from.to_lowercase();
    SENDER_WHITELIST
        .iter()
        .find(|(domain, _)| lower.ends_with(domain) || lower.contains(&format!("@{domain}")))
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
        Some(SampleFormat::PdfFlight)
    } else {
        Some(SampleFormat::PdfVat)
    }
}

pub fn classify_attachment(
    email: &ExtractedEmail,
    att: &RawAttachment,
) -> Option<Classification> {
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
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p invoice-collect classify`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add crates/invoice-collect/src/classify.rs crates/invoice-collect/src/lib.rs
git commit -m "feat: classify invoice attachments via sender whitelist and features"
```

---

## Task 5: 三重键去重与落盘（离线）

**Files:**
- Create: `crates/invoice-collect/src/dedupe.rs`
- Create: `crates/invoice-collect/src/store.rs`
- Modify: `crates/invoice-collect/src/lib.rs`
- Modify: `crates/invoice-collect/Cargo.toml`
- Test: 两个模块各自 inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `extract::RawAttachment`（Task 3）、`classify::{Classification, SampleFormat}`（Task 4）
- Produces:
  - `dedupe::Deduper::new() -> Deduper`
  - `dedupe::Deduper::is_new(&mut self, message_id: Option<&str>, data: &[u8]) -> bool`
  - `dedupe::sha256_hex(data: &[u8]) -> String`
  - `store::SavedSample { rel_path: String, sha8: String, byte_len: usize }`
  - `store::save_sample(root: &Path, seq: usize, cls: &Classification, att: &RawAttachment) -> anyhow::Result<SavedSample>`

产品方案要求三重键：`Message-ID` + 文件 SHA256 + 发票号。**本计划只实现前两个**——发票号要解析文件才知道，而解析是另一个计划的事。前两个键足以消除"平台重发 + 用户转发"这两种主要重复来源。

- [ ] **Step 1: 加依赖**

`sha2` 已在 Task 1 的 workspace 依赖里，确认 `crates/invoice-collect/Cargo.toml` 已包含 `sha2.workspace = true`。追加：

```toml
[dev-dependencies]
tempfile = "3.13"
```

- [ ] **Step 2: 写 dedupe 的失败测试**

创建 `crates/invoice-collect/src/dedupe.rs`：

```rust
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 双键去重器：Message-ID 去重整封邮件，文件 SHA256 去重同一份附件。
#[derive(Default)]
pub struct Deduper {
    seen_message_ids: HashSet<String>,
    seen_file_hashes: HashSet<String>,
}

impl Deduper {
    pub fn new() -> Self {
        Self::default()
    }

    /// 判断这份附件是否为新内容。有副作用：会记录已见过的键。
    ///
    /// Message-ID 相同**且**文件内容相同才算重复。
    /// 只有 Message-ID 相同不算 —— 一封邮件可以带多张不同的发票。
    pub fn is_new(&mut self, message_id: Option<&str>, data: &[u8]) -> bool {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_stable_and_64_hex_chars() {
        let h = sha256_hex(b"hello");
        assert_eq!(h.len(), 64);
        assert_eq!(h, sha256_hex(b"hello"));
        assert_ne!(h, sha256_hex(b"world"));
    }

    #[test]
    fn first_occurrence_is_new() {
        let mut d = Deduper::new();
        assert!(d.is_new(Some("id1"), b"content"));
    }

    #[test]
    fn identical_content_from_same_email_is_duplicate() {
        let mut d = Deduper::new();
        assert!(d.is_new(Some("id1"), b"content"));
        assert!(!d.is_new(Some("id1"), b"content"));
    }

    #[test]
    fn same_content_resent_under_new_message_id_is_duplicate() {
        // 平台重发场景：新邮件，同一份 PDF
        let mut d = Deduper::new();
        assert!(d.is_new(Some("id1"), b"same-pdf"));
        assert!(!d.is_new(Some("id2"), b"same-pdf"));
    }

    #[test]
    fn different_attachments_in_one_email_are_both_new() {
        // 一封邮件带两张不同的发票
        let mut d = Deduper::new();
        assert!(d.is_new(Some("id1"), b"invoice-a"));
        assert!(d.is_new(Some("id1"), b"invoice-b"));
    }

    #[test]
    fn missing_message_id_still_dedupes_by_content() {
        let mut d = Deduper::new();
        assert!(d.is_new(None, b"x"));
        assert!(!d.is_new(None, b"x"));
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p invoice-collect dedupe`
Expected: FAIL，panic 于 `not implemented`

- [ ] **Step 4: 实现 is_new**

替换 `dedupe.rs` 里的 `unimplemented!()`：

```rust
    pub fn is_new(&mut self, message_id: Option<&str>, data: &[u8]) -> bool {
        let file_hash = sha256_hex(data);

        // 文件内容重复 = 同一份附件，无论来自哪封邮件
        if self.seen_file_hashes.contains(&file_hash) {
            return false;
        }

        self.seen_file_hashes.insert(file_hash);
        if let Some(id) = message_id {
            self.seen_message_ids.insert(id.to_string());
        }
        true
    }
```

`seen_message_ids` 目前只做记录、不参与判定——文件哈希已经是更强的判据。保留这个字段是为了 Task 7 能输出"跨越多少封邮件"的统计。

- [ ] **Step 5: 运行确认通过**

Run: `cargo test -p invoice-collect dedupe`
Expected: 全部 PASS

- [ ] **Step 6: 写 store 的失败测试**

创建 `crates/invoice-collect/src/store.rs`：

```rust
use crate::classify::Classification;
use crate::dedupe::sha256_hex;
use crate::extract::RawAttachment;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct SavedSample {
    /// 相对 fixtures/ 的路径，直接写进 manifest.toml
    pub rel_path: String,
    pub sha8: String,
    pub byte_len: usize,
}

/// 落盘一份样本。
///
/// 命名格式 `{seq:02}-{platform}-{sha8}.{ext}`，例如 `01-12306-a3f9c1d2.pdf`。
/// 三个考量：序号让人工审阅有顺序；平台名让人一眼看出这是哪类票；
/// sha8 保证不撞名。原始中文文件名不进路径 —— 跨平台文件名兼容性问题太多。
pub fn save_sample(
    root: &Path,
    seq: usize,
    cls: &Classification,
    att: &RawAttachment,
) -> Result<SavedSample> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{MatchReason, SampleFormat};

    fn classification(format: SampleFormat, platform: &str) -> Classification {
        Classification {
            format,
            platform: platform.into(),
            reason: MatchReason::SenderWhitelist,
        }
    }

    fn attachment(data: &[u8]) -> RawAttachment {
        RawAttachment {
            filename: "电子发票.pdf".into(),
            content_type: "application/pdf".into(),
            data: data.to_vec(),
        }
    }

    #[test]
    fn writes_file_and_returns_relative_path() {
        let tmp = tempfile::tempdir().unwrap();
        let saved = save_sample(
            tmp.path(),
            1,
            &classification(SampleFormat::PdfRail, "12306"),
            &attachment(b"%PDF-1.4"),
        )
        .unwrap();

        assert!(saved.rel_path.starts_with("samples/01-12306-"));
        assert!(saved.rel_path.ends_with(".pdf"));
        assert_eq!(saved.byte_len, 8);

        let on_disk = tmp.path().join(&saved.rel_path);
        assert!(on_disk.exists(), "文件未落盘: {}", on_disk.display());
        assert_eq!(std::fs::read(&on_disk).unwrap(), b"%PDF-1.4");
    }

    #[test]
    fn sha8_is_first_8_chars_of_full_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let data = b"%PDF-1.4";
        let saved = save_sample(
            tmp.path(),
            1,
            &classification(SampleFormat::PdfVat, "unknown"),
            &attachment(data),
        )
        .unwrap();
        assert_eq!(saved.sha8, sha256_hex(data)[..8]);
    }

    #[test]
    fn extension_follows_format_not_original_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let mut att = attachment(b"<Invoice/>");
        att.filename = "发票.PDF".into(); // 原名误导
        let saved = save_sample(
            tmp.path(),
            3,
            &classification(SampleFormat::Xml, "tax"),
            &att,
        )
        .unwrap();
        assert!(saved.rel_path.ends_with(".xml"), "实际: {}", saved.rel_path);
    }

    #[test]
    fn sequence_number_is_zero_padded_to_two_digits() {
        let tmp = tempfile::tempdir().unwrap();
        let saved = save_sample(
            tmp.path(),
            7,
            &classification(SampleFormat::Ofd, "tax"),
            &attachment(b"PK\x03\x04"),
        )
        .unwrap();
        assert!(saved.rel_path.contains("/07-"), "实际: {}", saved.rel_path);
    }

    #[test]
    fn creates_samples_directory_if_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("does/not/exist");
        let saved = save_sample(
            &nested,
            1,
            &classification(SampleFormat::Image, "unknown"),
            &attachment(b"\xff\xd8\xff"),
        )
        .unwrap();
        assert!(nested.join(&saved.rel_path).exists());
    }
}
```

在 `lib.rs` 追加 `pub mod dedupe;` 和 `pub mod store;`。

- [ ] **Step 7: 运行确认失败**

Run: `cargo test -p invoice-collect store`
Expected: FAIL，panic 于 `not implemented`

- [ ] **Step 8: 实现 save_sample**

替换 `store.rs` 里的 `unimplemented!()`：

```rust
pub fn save_sample(
    root: &Path,
    seq: usize,
    cls: &Classification,
    att: &RawAttachment,
) -> Result<SavedSample> {
    let samples_dir = root.join("samples");
    std::fs::create_dir_all(&samples_dir)
        .with_context(|| format!("创建目录 {} 失败", samples_dir.display()))?;

    let sha8 = sha256_hex(&att.data)[..8].to_string();
    let file_name = format!(
        "{seq:02}-{}-{}.{}",
        cls.platform,
        sha8,
        cls.format.extension()
    );
    let rel_path = format!("samples/{file_name}");
    let full_path: PathBuf = root.join(&rel_path);

    std::fs::write(&full_path, &att.data)
        .with_context(|| format!("写入 {} 失败", full_path.display()))?;

    Ok(SavedSample {
        rel_path,
        sha8,
        byte_len: att.data.len(),
    })
}
```

- [ ] **Step 9: 运行确认通过**

Run: `cargo test -p invoice-collect`
Expected: dedupe 与 store 的测试全部 PASS

- [ ] **Step 10: Commit**

```bash
git add crates/invoice-collect/src/dedupe.rs crates/invoice-collect/src/store.rs \
        crates/invoice-collect/src/lib.rs crates/invoice-collect/Cargo.toml
git commit -m "feat: dedupe attachments by content hash and persist samples"
```

---

## Task 6: 清单骨架生成（离线）

**Files:**
- Create: `crates/invoice-collect/src/manifest_gen.rs`
- Modify: `crates/invoice-collect/src/lib.rs`
- Test: `crates/invoice-collect/src/manifest_gen.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: `classify::Classification`（Task 4）、`store::SavedSample`（Task 5）
- Produces:
  - `manifest_gen::ManifestEntry { saved: SavedSample, format: String, platform: String, original_filename: String, subject: String }`
  - `manifest_gen::render(entries: &[ManifestEntry]) -> String`

**这是两个计划的接缝**。产出的 TOML 必须能被解析验证计划 Task 2 的 `Manifest::load` 读进去。字段名必须精确匹配那边的 `Sample` 结构：`path`、`format`、`ticket_type`、`invoice_number`、`issue_date`、`total_amount`、`tax_amount`、`tax_rate`、`buyer_name`、`seller_name`。

期望值无法自动填——**必须人工打开每张票抄写**。所以生成的是骨架：路径和格式填好，期望值留占位，并把原始文件名和邮件主题作为注释写在旁边，方便人工对照。

- [ ] **Step 1: 写失败测试**

创建 `crates/invoice-collect/src/manifest_gen.rs`：

```rust
use crate::store::SavedSample;

#[derive(Debug, Clone)]
pub struct ManifestEntry {
    pub saved: SavedSample,
    pub format: String,
    pub platform: String,
    pub original_filename: String,
    pub subject: String,
}

/// 生成 manifest.toml 内容。
///
/// 字段名与解析验证计划的 `manifest::Sample` 严格对应。
/// ticket_type 与各期望值留待人工填写。
pub fn render(entries: &[ManifestEntry]) -> String {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: &str, format: &str, platform: &str, subject: &str) -> ManifestEntry {
        ManifestEntry {
            saved: SavedSample {
                rel_path: format!("samples/{seq}-{platform}-abcd1234.pdf"),
                sha8: "abcd1234".into(),
                byte_len: 12345,
            },
            format: format.into(),
            platform: platform.into(),
            original_filename: "电子发票.pdf".into(),
            subject: subject.into(),
        }
    }

    #[test]
    fn renders_one_sample_block_per_entry() {
        let out = render(&[
            entry("01", "pdf-rail", "12306", "您的电子发票"),
            entry("02", "pdf-vat", "unknown", "住宿发票"),
        ]);
        assert_eq!(out.matches("[[sample]]").count(), 2);
    }

    #[test]
    fn includes_path_and_format_filled_in() {
        let out = render(&[entry("01", "pdf-rail", "12306", "x")]);
        assert!(out.contains(r#"path = "samples/01-12306-abcd1234.pdf""#), "实际:\n{out}");
        assert!(out.contains(r#"format = "pdf-rail""#), "实际:\n{out}");
    }

    #[test]
    fn required_fields_are_empty_placeholders() {
        let out = render(&[entry("01", "pdf-rail", "12306", "x")]);
        // 必填字段留空串占位，人工填写前不应有假数据
        assert!(out.contains(r#"invoice_number = """#), "实际:\n{out}");
        assert!(out.contains(r#"total_amount = """#), "实际:\n{out}");
        assert!(out.contains(r#"issue_date = """#), "实际:\n{out}");
    }

    #[test]
    fn optional_fields_are_commented_out_not_empty_strings() {
        // 空串会反序列化成 Some("")，导致比对时把空串喂给 Decimal 解析器，
        // 每个可选字段都报假的不匹配。必须输出成注释行。
        let out = render(&[entry("01", "pdf-rail", "12306", "x")]);
        assert!(out.contains(r#"# tax_amount = """#), "实际:\n{out}");
        assert!(out.contains(r#"# tax_rate = """#), "实际:\n{out}");

        let parsed: toml::Value = toml::from_str(&out).unwrap();
        let sample = &parsed["sample"][0];
        assert!(
            sample.get("tax_amount").is_none(),
            "可选字段解析后应为缺失，实际存在: {sample:?}"
        );
    }

    #[test]
    fn annotates_original_filename_and_subject_as_comments() {
        let out = render(&[entry("01", "pdf-rail", "12306", "您的电子发票")]);
        assert!(out.contains("# 原始文件名: 电子发票.pdf"), "实际:\n{out}");
        assert!(out.contains("# 邮件主题: 您的电子发票"), "实际:\n{out}");
    }

    #[test]
    fn header_states_manual_fill_requirement() {
        let out = render(&[entry("01", "pdf-rail", "12306", "x")]);
        assert!(out.contains("人工填写"), "表头应说明需人工填写:\n{out}");
    }

    #[test]
    fn empty_input_still_produces_valid_header() {
        let out = render(&[]);
        assert!(out.contains("未采集到"), "实际:\n{out}");
        assert!(!out.contains("[[sample]]"));
    }

    #[test]
    fn output_parses_as_valid_toml() {
        let out = render(&[entry("01", "pdf-rail", "12306", "带\"引号\"的主题")]);
        let parsed: toml::Value = toml::from_str(&out)
            .unwrap_or_else(|e| panic!("生成的 TOML 无法解析: {e}\n---\n{out}"));
        assert!(parsed.get("sample").is_some());
    }
}
```

在 `lib.rs` 追加 `pub mod manifest_gen;`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p invoice-collect manifest_gen`
Expected: FAIL，panic 于 `not implemented`

- [ ] **Step 3: 实现 render**

替换 `manifest_gen.rs` 里的 `unimplemented!()`：

```rust
use std::fmt::Write as _;

pub fn render(entries: &[ManifestEntry]) -> String {
    let mut out = String::new();

    out.push_str("# 发票样本清单\n#\n");
    out.push_str("# 由 invoice-collect 自动生成。path 与 format 已填好，\n");
    out.push_str("# 其余字段需**人工填写** —— 打开每个样本文件，把实际值抄进来。\n");
    out.push_str("# 这些值是解析器的验收依据，填错会让验证结论失效。\n#\n");
    out.push_str("# ticket_type 取值: Rail | Flight | Hotel | CityTransport | Meal | Other\n");
    out.push_str("# 金额与税率用字符串，保留原始小数位，例如 \"553.00\" \"0.09\"\n");
    out.push_str("# 日期用 YYYY-MM-DD\n");
    out.push_str("# 发票上没有的可选字段，删掉该行即可\n\n");

    if entries.is_empty() {
        out.push_str("# 未采集到任何样本。请检查 invoice-collect probe 的输出，\n");
        out.push_str("# 确认目标日期范围内确实存在带发票附件的邮件。\n");
        return out;
    }

    for e in entries {
        let _ = writeln!(out, "# 原始文件名: {}", sanitize_comment(&e.original_filename));
        let _ = writeln!(out, "# 邮件主题: {}", sanitize_comment(&e.subject));
        let _ = writeln!(out, "# 平台: {} · 大小: {} 字节", e.platform, e.saved.byte_len);
        out.push_str("[[sample]]\n");
        let _ = writeln!(out, "path = \"{}\"", e.saved.rel_path);
        let _ = writeln!(out, "format = \"{}\"", e.format);
        out.push_str("ticket_type = \"Other\"       # 待确认\n");
        // 必填字段：留空串，人工必须填
        out.push_str("invoice_number = \"\"\n");
        out.push_str("issue_date = \"\"\n");
        out.push_str("total_amount = \"\"\n");
        // 可选字段：以注释行输出。发票上有这一项就取消注释并填值；
        // 没有就保持注释。绝不能输出空串 —— 空串会被 serde 反序列化成
        // Some("")，比对时传给 Decimal::from_str 解析失败，
        // 每张样本的每个可选字段都会报假的不匹配。
        out.push_str("# tax_amount = \"\"\n");
        out.push_str("# tax_rate = \"\"\n");
        out.push_str("# buyer_name = \"\"\n");
        out.push_str("# seller_name = \"\"\n\n");
    }

    out
}

/// 注释里不能出现换行，否则会破坏 TOML 结构
fn sanitize_comment(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p invoice-collect manifest_gen`
Expected: 7 个测试全部 PASS

`output_parses_as_valid_toml` 这个测试尤其重要——它保证生成物能被另一个计划直接消费。

- [ ] **Step 5: Commit**

```bash
git add crates/invoice-collect/src/manifest_gen.rs crates/invoice-collect/src/lib.rs
git commit -m "feat: generate manifest skeleton for manual expected-value entry"
```

---

## Task 7: 全量采集（联网，产出样本集）

**Files:**
- Modify: `crates/invoice-collect/src/main.rs`
- Create: `fixtures/manifest.toml`（生成物）
- Create: `fixtures/samples/*`（生成物，不提交）
- Create: `docs/collection-report.md`（手工记录结果）

**Interfaces:**
- Consumes: 全部前六个任务
- Produces: 样本文件与清单骨架

- [ ] **Step 1: 加 collect 子命令**

在 `main.rs` 顶部追加 imports：

```rust
use invoice_collect::classify::classify_attachment;
use invoice_collect::dedupe::Deduper;
use invoice_collect::extract::extract_email;
use invoice_collect::manifest_gen::{render, ManifestEntry};
use invoice_collect::store::save_sample;
use std::path::Path;
```

在 `match` 中追加分支：

```rust
        Some("collect") => collect(&parse_target(&args)?),
```

追加函数：

```rust
fn collect(target: &Target) -> anyhow::Result<()> {
    let cfg = ImapConfig::from_env(&target.username)?;
    let range = target.range.clone();
    let fixtures_root = Path::new("fixtures");

    let mut session = Session::connect(&cfg)?;
    let uids = session.search_range("INBOX", &range)?;
    println!("范围内 {} 封邮件，开始逐封处理\n", uids.len());

    let mut deduper = Deduper::new();
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let mut stats = Stats::default();

    for uid in &uids {
        stats.emails_scanned += 1;

        let raw = match session.fetch_raw(*uid) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  UID {uid} 拉取失败，跳过: {e}");
                stats.fetch_failures += 1;
                continue;
            }
        };

        let email = match extract_email(&raw) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("  UID {uid} MIME 解析失败，跳过: {e}");
                stats.parse_failures += 1;
                continue;
            }
        };

        if email.attachments.is_empty() {
            continue;
        }
        stats.emails_with_attachments += 1;

        for att in &email.attachments {
            stats.attachments_seen += 1;

            let Some(cls) = classify_attachment(&email, att) else {
                stats.not_invoice += 1;
                continue;
            };

            if !deduper.is_new(email.message_id.as_deref(), &att.data) {
                stats.duplicates += 1;
                continue;
            }

            let seq = entries.len() + 1;
            let saved = save_sample(fixtures_root, seq, &cls, att)?;
            println!(
                "  [{:>2}] {:<10} {:<12} {} 字节  ← {}",
                seq,
                cls.format.as_manifest_str(),
                cls.platform,
                saved.byte_len,
                att.filename
            );

            entries.push(ManifestEntry {
                saved,
                format: cls.format.as_manifest_str().to_string(),
                platform: cls.platform.clone(),
                original_filename: att.filename.clone(),
                subject: email.subject.clone(),
            });
        }
    }

    let manifest_path = fixtures_root.join("manifest.toml");
    std::fs::create_dir_all(fixtures_root)?;
    std::fs::write(&manifest_path, render(&entries))?;

    stats.print(entries.len());
    print_format_breakdown(&entries);
    println!("\n清单骨架已写入 {}", manifest_path.display());
    println!("下一步：打开每个样本文件，把期望值填进清单。");

    Ok(())
}

#[derive(Default)]
struct Stats {
    emails_scanned: usize,
    emails_with_attachments: usize,
    attachments_seen: usize,
    not_invoice: usize,
    duplicates: usize,
    fetch_failures: usize,
    parse_failures: usize,
}

impl Stats {
    fn print(&self, saved: usize) {
        println!("\n─── 采集统计 ───");
        println!("扫描邮件          {}", self.emails_scanned);
        println!("其中含附件        {}", self.emails_with_attachments);
        println!("附件总数          {}", self.attachments_seen);
        println!("判定为非发票      {}", self.not_invoice);
        println!("重复丢弃          {}", self.duplicates);
        println!("拉取失败          {}", self.fetch_failures);
        println!("解析失败          {}", self.parse_failures);
        println!("最终落盘          {saved}");
    }
}

fn print_format_breakdown(entries: &[ManifestEntry]) {
    use std::collections::BTreeMap;
    let mut by_format: BTreeMap<&str, usize> = BTreeMap::new();
    for e in entries {
        *by_format.entry(e.format.as_str()).or_insert(0) += 1;
    }

    println!("\n─── 格式分布 vs 解析验证计划的需求 ───");
    let needed: &[(&str, usize)] = &[
        ("xml", 5),
        ("ofd", 5),
        ("pdf-rail", 3),
        ("pdf-flight", 3),
        ("pdf-vat", 3),
        ("image", 10),
    ];
    for (format, need) in needed {
        let got = by_format.get(format).copied().unwrap_or(0);
        let mark = if got >= *need { "✓" } else { "缺" };
        println!("  {mark} {format:<12} {got}/{need}");
    }
}
```

- [ ] **Step 2: 运行采集**

```bash
read -rs INVOICE_IMAP_PASSWORD && export INVOICE_IMAP_PASSWORD
cargo run -p invoice-collect -- collect <你的邮箱地址>
unset INVOICE_IMAP_PASSWORD
```

Expected: 逐封处理日志 + 采集统计 + 格式分布表 + `fixtures/manifest.toml` 生成

- [ ] **Step 3: 按格式分布表判定后续动作**

表里每个"缺"都要处理。四种缺口，对策不同：

| 缺口 | 可能原因 | 对策 |
|---|---|---|
| `xml` / `ofd` 为 0 | 该邮箱没收过数电票，或平台只发 PDF | 从税务局电子发票平台或开票邮件里补；这两类是 L0 路径的唯一验证依据，**不能跳过** |
| `image` 为 0 | 图片格式增值税票来自纸票扫描/拍照，不会出现在邮件里 | **手工补**：扫描或拍摄纸质发票放入 `fixtures/samples/`，手工追加清单条目 |
| `pdf-flight` 为 0 | 该月没坐飞机 | 扩大日期范围重跑，或从其他月份补 |
| 全部为 0 | 该月无发票邮件 | 回到 Task 2 的决策关口，换日期范围 |

`image` 那一项值得单说：它天然不来自邮箱（纸票拍照不会自己发到邮箱），所以采集器采不到是正常的，必须人工补。而 L2 OCR 是解析验证计划里技术风险最高的一环，样本不能省。

- [ ] **Step 4: 人工填写期望值**

逐个打开 `fixtures/samples/` 下的文件，把实际字段抄进 `fixtures/manifest.toml`。

XML 和 OFD 无法直接阅读，两个办法：
- OFD 用 WPS Office 或数字福建 OFD 阅读器打开
- XML 用浏览器或文本编辑器打开，肉眼找字段

**填写时注意**：
- 金额保留原始小数位（`553.00` 不要写成 `553`）
- 税率写小数（`9%` 写成 `0.09`）
- **可选字段（`tax_amount` / `tax_rate` / `buyer_name` / `seller_name`）生成时是注释行**。发票上有这一项就取消注释并填值；没有就保持注释原样
- **绝不要把可选字段留成空串**。空串会被反序列化成 `Some("")`，比对时喂给 `Decimal::from_str` 解析失败，每张样本的每个空字段都会报一条假的不匹配，看起来像解析器坏了

- [ ] **Step 5: 记录采集报告**

创建 `docs/collection-report.md`：

```markdown
# 发票样本采集报告

**采集时间**：<填写>
**日期范围**：2026-06-01 至 2026-06-30

> 不要在本报告里写邮箱地址或任何凭证。

## 采集统计

<粘贴 collect 命令输出的统计段>

## 格式分布

<粘贴格式分布表>

## 缺口与补齐方式

| 格式 | 采集到 | 需要 | 补齐方式 |
|---|---|---|---|
| xml |  | 5 |  |
| ofd |  | 5 |  |
| pdf-rail |  | 3 |  |
| pdf-flight |  | 3 |  |
| pdf-vat |  | 3 |  |
| image |  | 10 | 手工扫描/拍照 |

## 作废票负例

- [ ] 已取得（来源：______）
- [ ] 未取得 —— 解析验证计划的验签负例测试将标注为覆盖缺口

## 观察到的问题

<例如：某平台的发票在正文链接里而非附件；某些邮件的附件名编码异常等>
```

- [ ] **Step 6: Commit**

```bash
git add fixtures/manifest.toml docs/collection-report.md crates/invoice-collect/src/main.rs
git commit -m "feat: add collect command producing sample set and manifest skeleton"
```

`fixtures/samples/` 已被 gitignore，不会进仓库。

---

## 完成定义

- [ ] `cargo test -p invoice-collect` 全绿
- [ ] `probe` 能连上邮箱并列出目标范围内的邮件
- [ ] `collect` 产出 `fixtures/samples/` 与 `fixtures/manifest.toml`
- [ ] 生成的 manifest.toml 能被 `toml::from_str` 解析（Task 6 的测试已锁）
- [ ] 格式分布表中每一项达到解析验证计划的数量要求，或已在采集报告中记录缺口与补齐方式
- [ ] `fixtures/manifest.toml` 的期望值已人工填写完毕
- [ ] 采集报告已记录

**达成后，解析验证计划的 Task 4–9 即解除阻塞。**

## 已知范围限制

| 限制 | 影响 | 何时处理 |
|---|---|---|
| 不做 LLM 判定（漏斗第 3 级） | 少数不在白名单、文件名也无关键词的发票会漏掉 | 产品实现时补；本计划人工补样本即可 |
| 不跟踪正文里的下载链接 | 部分平台只在邮件里放链接，不带附件 | 若 Task 2 发现此类邮件占比高，需单独设计 |
| 去重只用两键（缺发票号） | 同一张票以不同文件形式重发会被当作两份 | 发票号需解析后才知道，属解析模块职责 |
| 只搜 INBOX | 已归档到子文件夹的发票搜不到 | `probe` 会列出所有文件夹，必要时改 `collect` 的目标文件夹 |
| `image` 格式采集不到 | 纸票照片不来自邮箱 | 必须人工补，见 Task 7 Step 3 |
