//! H3 首次运行向导后端命令

use crate::error::{AppError, AppResult};
use crate::AppState;
use std::sync::Mutex;
use tauri::State;

/// 首次运行由向导完成标记决定；未配置邮箱也可以进入纯本地模式。
#[tauri::command]
pub fn is_first_run(state: State<Mutex<AppState>>) -> AppResult<bool> {
    let app_state = state.lock().unwrap();
    let completed = app_state
        .ledger_db()?
        .get_setting("onboarding_completed")
        .map_err(|e| AppError::database(format!("检查首次运行状态失败: {}", e)))?;
    Ok(completed.as_deref() != Some("true"))
}
