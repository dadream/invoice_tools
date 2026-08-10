use std::fs;
use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// 日志根目录：`$INVOICE_ASSISTANT_HOME/logs`，默认 `~/.invoice-assistant/logs`。
/// 日志只落本地文件，绝不上传。
pub fn log_dir() -> anyhow::Result<PathBuf> {
    if let Some(root) = std::env::var_os("INVOICE_ASSISTANT_HOME") {
        return Ok(PathBuf::from(root).join("logs"));
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    Ok(PathBuf::from(home).join(".invoice-assistant").join("logs"))
}

/// 构造按天滚动的非阻塞 writer。返回的 guard 必须存活到进程结束。
fn build_writer(
    dir: &Path,
) -> anyhow::Result<(tracing_appender::non_blocking::NonBlocking, WorkerGuard)> {
    fs::create_dir_all(dir)?;
    let appender = tracing_appender::rolling::daily(dir, "app.log");
    Ok(tracing_appender::non_blocking(appender))
}

/// 初始化全局日志。调用方需持有返回的 guard。
pub fn init() -> anyhow::Result<WorkerGuard> {
    let dir = log_dir()?;
    let (writer, guard) = build_writer(&dir)?;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(writer).with_ansi(false).with_target(false))
        .with(fmt::layer().with_target(false))
        .init();

    tracing::info!(dir = %dir.display(), "日志系统已初始化");
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_dir_honors_env_override() {
        let tmp = std::env::temp_dir().join("ia-log-test-override");
        // SAFETY: 单线程测试内设置环境变量
        std::env::set_var("INVOICE_ASSISTANT_HOME", &tmp);
        let dir = log_dir().unwrap();
        std::env::remove_var("INVOICE_ASSISTANT_HOME");
        assert_eq!(dir, tmp.join("logs"));
    }

    #[test]
    fn log_dir_defaults_under_home() {
        std::env::remove_var("INVOICE_ASSISTANT_HOME");
        let dir = log_dir().unwrap();
        assert!(dir.ends_with("logs"), "应以 logs 结尾: {dir:?}");
        assert!(
            dir.to_string_lossy().contains(".invoice-assistant"),
            "应位于 .invoice-assistant 下: {dir:?}"
        );
    }

    #[test]
    fn writes_log_lines_to_target_dir() {
        let tmp = std::env::temp_dir().join(format!("ia-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let logs = tmp.join("logs");
        {
            let (writer, _guard) = build_writer(&logs).unwrap();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(writer)
                .with_ansi(false)
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                tracing::info!("probe-marker");
            });
            // _guard 在此处 drop，触发 flush
        }
        let found = std::fs::read_dir(&logs)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| std::fs::read_to_string(e.path())
                .map(|s| s.contains("probe-marker"))
                .unwrap_or(false));
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(found, "日志目录中应存在含 probe-marker 的文件");
    }
}
