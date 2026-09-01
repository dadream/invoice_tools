use std::fs;
use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// 日志根目录：`DataRoot/logs`。日志只落本地文件，绝不上传。
/// 日志只落本地文件，绝不上传。
pub fn log_dir() -> anyhow::Result<PathBuf> {
    crate::paths::logs_dir()
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
        .with(
            fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_target(false),
        )
        .with(fmt::layer().with_target(false))
        .init();

    // 完整数据目录可能包含 Windows 用户名，不写入常规支持日志。
    tracing::info!("日志系统已初始化");
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_dir_honors_env_override() {
        let _guard = crate::paths::test_env_lock();
        let tmp = std::env::temp_dir().join("ia-log-test-override");
        // SAFETY: 单线程测试内设置环境变量
        std::env::set_var("INVOICE_ASSISTANT_HOME", &tmp);
        let dir = log_dir().unwrap();
        std::env::remove_var("INVOICE_ASSISTANT_HOME");
        assert_eq!(dir, tmp.join("logs"));
    }

    #[test]
    fn log_dir_defaults_under_local_app_data() {
        let _guard = crate::paths::test_env_lock();
        std::env::remove_var("INVOICE_ASSISTANT_HOME");
        let dir = log_dir().unwrap();
        assert!(dir.ends_with("logs"), "应以 logs 结尾: {dir:?}");
        assert!(
            dir.ends_with(PathBuf::from("InvoiceAssistant").join("Data").join("logs")),
            "应位于本机 InvoiceAssistant/Data 下: {dir:?}"
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
            .any(|e| {
                std::fs::read_to_string(e.path())
                    .map(|s| s.contains("probe-marker"))
                    .unwrap_or(false)
            });
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(found, "日志目录中应存在含 probe-marker 的文件");
    }
}
