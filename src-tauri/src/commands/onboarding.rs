//! H3 首次运行向导后端命令

use crate::error::{AppError, AppResult};
use crate::AppState;
use std::sync::Mutex;
use tauri::State;

/// 检测是否为首次运行（检查是否有邮箱账号）
#[tauri::command]
pub fn is_first_run(state: State<Mutex<AppState>>) -> AppResult<bool> {
    let app_state = state.lock().unwrap();
    let db = app_state.accounts_db()?;

    let accounts = db
        .list_accounts()
        .map_err(|e| AppError::database(format!("检查账号列表失败: {}", e)))?;

    // 如果没有账号，说明是首次运行
    Ok(accounts.is_empty())
}
