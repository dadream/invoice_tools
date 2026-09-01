use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::logger;

const MAX_NAME_LEN: usize = 64;

#[derive(Debug, Serialize, Deserialize)]
pub struct VersionInfo {
    pub version: String,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthReport {
    pub ok: bool,
    pub log_dir: String,
    pub version: String,
}

/// 冒烟命令：验证 IPC 往返。
#[tauri::command]
pub fn greet(name: String) -> AppResult<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        // 不记录用户输入内容，避免把 PII 写进日志
        tracing::warn!("greet 收到空姓名");
        return Err(AppError::validation("姓名不能为空"));
    }
    if trimmed.chars().count() > MAX_NAME_LEN {
        return Err(AppError::validation(format!(
            "姓名不能超过 {MAX_NAME_LEN} 个字符"
        )));
    }
    tracing::info!(name_len = trimmed.chars().count(), "greet 调用成功");
    Ok(format!("你好，{trimmed}！欢迎使用发票报销助手。"))
}

#[tauri::command]
pub fn get_version() -> AppResult<VersionInfo> {
    Ok(VersionInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        name: env!("CARGO_PKG_NAME").to_string(),
    })
}

/// 自检：确认日志目录可解析，供前端展示后端状态。
#[tauri::command]
pub fn health_check() -> AppResult<HealthReport> {
    let dir = logger::log_dir().map_err(AppError::from)?;
    Ok(HealthReport {
        ok: true,
        log_dir: dir.display().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// 故意返回指定分类的错误，用于前端错误处理路径的手工与自动验收。
#[tauri::command]
pub fn trigger_error(kind: String) -> AppResult<()> {
    Err(match kind.as_str() {
        "database" => AppError::database("模拟数据库故障"),
        "parse" => AppError::parse("模拟解析失败"),
        "network" => AppError::network("模拟网络超时"),
        "io" => AppError::io("模拟文件读写失败"),
        "validation" => AppError::validation("模拟字段校验失败"),
        _ => AppError::internal("模拟内部错误"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn greet_returns_chinese_greeting() {
        let msg = greet("张三".to_string()).unwrap();
        assert!(msg.contains("张三"), "应包含姓名: {msg}");
        assert!(msg.contains("发票报销助手"), "应包含应用名: {msg}");
    }

    #[test]
    fn greet_rejects_blank_name() {
        let err = greet("   ".to_string()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Validation);
    }

    #[test]
    fn greet_rejects_overlong_name() {
        let err = greet("字".repeat(65)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Validation);
    }

    #[test]
    fn get_version_reports_crate_metadata() {
        let info = get_version().unwrap();
        assert_eq!(info.name, "invoice-assistant");
        assert!(!info.version.is_empty());
    }

    #[test]
    fn health_check_reports_log_dir() {
        let report = health_check().unwrap();
        assert!(report.ok);
        assert!(
            report.log_dir.contains("logs"),
            "应含 logs: {}",
            report.log_dir
        );
    }

    #[test]
    fn trigger_error_maps_known_kinds() {
        assert_eq!(
            trigger_error("network".into()).unwrap_err().kind(),
            ErrorKind::Network
        );
        assert_eq!(
            trigger_error("database".into()).unwrap_err().kind(),
            ErrorKind::Database
        );
        // 未知分类归为 internal
        assert_eq!(
            trigger_error("nonsense".into()).unwrap_err().kind(),
            ErrorKind::Internal
        );
    }
}
