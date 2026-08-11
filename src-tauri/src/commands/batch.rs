use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::State;

use invoice_store::models::{Batch, BatchStatus};
use crate::error::{AppError, AppResult};
use crate::AppState;

/// 批次数据传输对象（简化版，用于列表展示）
#[derive(Debug, Serialize, Deserialize)]
pub struct BatchDto {
    pub id: i64,
    pub name: String,
    pub month: String,
    pub status: String,  // "draft" | "submitted" | "approved" | "completed" | "rejected"
    pub total_amount: String,  // 格式化后的金额字符串
    pub invoice_count: i32,
    pub created_at: String,
    pub updated_at: String,
    pub submitted_at: Option<String>,
    pub approved_at: Option<String>,
    pub completed_at: Option<String>,
    pub rejected_at: Option<String>,
}

impl From<Batch> for BatchDto {
    fn from(batch: Batch) -> Self {
        Self {
            id: batch.id,
            name: batch.name,
            month: batch.month,
            status: status_to_string(&batch.status),
            total_amount: batch.total_amount.to_string(),
            invoice_count: batch.invoice_count,
            created_at: batch.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            updated_at: batch.updated_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            submitted_at: batch.submitted_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            approved_at: batch.approved_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            completed_at: batch.completed_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            rejected_at: batch.rejected_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
        }
    }
}

fn status_to_string(status: &BatchStatus) -> String {
    match status {
        BatchStatus::Draft => "draft".to_string(),
        BatchStatus::Submitted => "submitted".to_string(),
        BatchStatus::Approved => "approved".to_string(),
        BatchStatus::Completed => "completed".to_string(),
        BatchStatus::Rejected => "rejected".to_string(),
    }
}

fn string_to_status(s: &str) -> AppResult<BatchStatus> {
    match s {
        "draft" => Ok(BatchStatus::Draft),
        "submitted" => Ok(BatchStatus::Submitted),
        "approved" => Ok(BatchStatus::Approved),
        "completed" => Ok(BatchStatus::Completed),
        "rejected" => Ok(BatchStatus::Rejected),
        _ => Err(AppError::validation(format!("无效的状态: {}", s))),
    }
}

/// 列出所有批次
#[tauri::command]
pub fn list_batches(state: State<Mutex<AppState>>) -> AppResult<Vec<BatchDto>> {
    let app_state = state.lock().map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    let batches = db.list_batches()
        .map_err(|e| AppError::database(format!("查询批次失败: {}", e)))?;

    Ok(batches.into_iter().map(BatchDto::from).collect())
}

/// 获取单个批次详情
#[tauri::command]
pub fn get_batch(id: i64, state: State<Mutex<AppState>>) -> AppResult<BatchDto> {
    let app_state = state.lock().map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    let batch = db.get_batch(id)
        .map_err(|e| AppError::database(format!("查询批次失败: {}", e)))?;

    Ok(BatchDto::from(batch))
}

/// 创建新批次
#[tauri::command]
pub fn create_batch(name: String, month: String, state: State<Mutex<AppState>>) -> AppResult<i64> {
    // 验证输入
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::validation("批次名称不能为空"));
    }
    if name.len() > 100 {
        return Err(AppError::validation("批次名称不能超过100个字符"));
    }

    // 验证月份格式 YYYY-MM
    if !month.chars().all(|c| c.is_ascii_digit() || c == '-') || month.len() != 7 {
        return Err(AppError::validation("月份格式必须为 YYYY-MM"));
    }

    let app_state = state.lock().map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    let id = db.create_batch(name, &month)
        .map_err(|e| AppError::database(format!("创建批次失败: {}", e)))?;

    tracing::info!(batch_id = id, month = %month, "批次创建成功");
    Ok(id)
}

/// 转换批次状态
#[tauri::command]
pub fn transition_batch_status(
    id: i64,
    new_status: String,
    state: State<Mutex<AppState>>
) -> AppResult<()> {
    let status = string_to_status(&new_status)?;

    let app_state = state.lock().map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    db.transition_batch_status(id, status)
        .map_err(|e| match e {
            invoice_store::StoreError::InvalidStateTransition { from, to } => {
                AppError::validation(format!("不允许从 {} 转换到 {}", from, to))
            }
            _ => AppError::database(format!("状态转换失败: {}", e)),
        })?;

    tracing::info!(batch_id = id, new_status = %new_status, "批次状态转换成功");
    Ok(())
}

/// 删除批次（仅允许删除 Draft 状态的批次）
#[tauri::command]
pub fn delete_batch(id: i64, state: State<Mutex<AppState>>) -> AppResult<()> {
    let app_state = state.lock().map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    // 先检查状态
    let batch = db.get_batch(id)
        .map_err(|e| AppError::database(format!("查询批次失败: {}", e)))?;

    if !matches!(batch.status, BatchStatus::Draft) {
        return Err(AppError::validation("只能删除草稿状态的批次"));
    }

    db.delete_batch(id)
        .map_err(|e| AppError::database(format!("删除批次失败: {}", e)))?;

    tracing::info!(batch_id = id, "批次删除成功");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrips() {
        for status in [
            BatchStatus::Draft,
            BatchStatus::Submitted,
            BatchStatus::Approved,
            BatchStatus::Completed,
            BatchStatus::Rejected,
        ] {
            let s = status_to_string(&status);
            let back = string_to_status(&s).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn rejects_invalid_status_string() {
        assert!(string_to_status("pending").is_err());
        assert!(string_to_status("Draft").is_err());  // 大小写敏感
    }
}
