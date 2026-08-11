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
    const FAKE_AUTH_CODE: &str = "abcdplaceholders";
    /// 形状不合法的假密码，用于触发告警分支
    const FAKE_BAD_SHAPE: &str = "NotAnAuthCode!";

    /// 环境变量是进程级共享状态，而 cargo 默认并行跑同一二进制内的测试。
    /// 任何 set_var/remove_var 都会污染其他用例（典型症状：remove_var 的用例
    /// 偶发读到别人刚写入的值）。所有触碰 `ENV_PASSWORD` 的测试统一先取此锁。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 取锁；忽略前一个用例 panic 导致的中毒状态，锁保护的只是串行性。
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn qq_domain_maps_to_qq_imap_host() {
        let _guard = env_guard();
        std::env::set_var(ENV_PASSWORD, FAKE_AUTH_CODE);
        let cfg = ImapConfig::from_env(FAKE_QQ_USER).unwrap();
        assert_eq!(cfg.host, "imap.qq.com");
        assert_eq!(cfg.port, 993);
        assert_eq!(cfg.username, FAKE_QQ_USER);
    }

    #[test]
    fn debug_output_never_contains_password() {
        let _guard = env_guard();
        let sentinel = "sentinel-must-not-appear";
        std::env::set_var(ENV_PASSWORD, sentinel);
        let cfg = ImapConfig::from_env(FAKE_QQ_USER).unwrap();
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains(sentinel), "密码泄漏到 Debug: {rendered}");
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn missing_env_var_mentions_authorization_code() {
        let _guard = env_guard();
        std::env::remove_var(ENV_PASSWORD);
        let err = ImapConfig::from_env(FAKE_QQ_USER).unwrap_err();
        assert!(err.to_string().contains("授权码"), "实际: {err}");
    }

    #[test]
    fn account_password_shape_triggers_warning() {
        let _guard = env_guard();
        // 账号登录密码的典型形状（含大写和符号）不符合授权码规则
        std::env::set_var(ENV_PASSWORD, FAKE_BAD_SHAPE);
        let cfg = ImapConfig::from_env(FAKE_QQ_USER).unwrap();
        let warning = cfg.warn_if_password_looks_wrong().expect("应产生告警");
        assert!(warning.contains("16 位"), "实际: {warning}");
    }

    #[test]
    fn valid_authorization_code_produces_no_warning() {
        let _guard = env_guard();
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
        let _guard = env_guard();
        std::env::set_var(ENV_PASSWORD, "x");
        let err = ImapConfig::from_env("someone@example.org").unwrap_err();
        assert!(err.to_string().contains("暂不支持"), "实际: {err}");
    }
}
