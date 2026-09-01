use std::sync::Mutex;

use invoice_store::models::EmailImportMessage;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::AppState;

/// 读取批次邮件台账；历史批次没有记录时返回空数组。
#[tauri::command]
pub fn list_email_import_ledger(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<EmailImportMessage>> {
    if batch_id <= 0 {
        return Err(AppError::validation("批次标识无效"));
    }
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .list_email_import_messages(batch_id)
        .map_err(|error| AppError::database(format!("读取邮件处理台账失败: {error}")))
}

/// 用户确认邮件已处理、无需处理，或重新打开待办。
#[tauri::command]
pub fn resolve_email_import_message(
    message_id: i64,
    action: String,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    if message_id <= 0 {
        return Err(AppError::validation("邮件台账标识无效"));
    }
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .resolve_email_import_message(message_id, &action)
        .map_err(|error| match error {
            invoice_store::StoreError::Validation(message) => AppError::validation(message),
            invoice_store::StoreError::NotFound(message) => AppError::validation(message),
            other => AppError::database(format!("更新邮件处理结果失败: {other}")),
        })
}
