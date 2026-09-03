use chrono::Local;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::AppState;
use invoice_store::models::{Batch, BatchGrouping, BatchStatus};

/// 批次数据传输对象（简化版，用于列表展示）
#[derive(Debug, Serialize, Deserialize)]
pub struct BatchDto {
    pub id: i64,
    pub name: String,
    pub month: String,
    pub status: String, // "draft" | "submitted" | "approved" | "completed" | "rejected"
    pub total_amount: String, // 格式化后的金额字符串
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
            submitted_at: batch
                .submitted_at
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            approved_at: batch
                .approved_at
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            completed_at: batch
                .completed_at
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            rejected_at: batch
                .rejected_at
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
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

fn grouping_has_pending_review(grouping: &BatchGrouping) -> bool {
    let ambiguities_are_empty =
        serde_json::from_str::<Vec<serde_json::Value>>(&grouping.ambiguities_json)
            .map(|ambiguities| ambiguities.is_empty())
            .unwrap_or(false);
    !ambiguities_are_empty || grouping.groups.iter().any(|group| group.requires_review)
}

/// 列出所有批次
#[tauri::command]
pub fn list_batches(state: State<Mutex<AppState>>) -> AppResult<Vec<BatchDto>> {
    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    let batches = db
        .list_batches()
        .map_err(|e| AppError::database(format!("查询批次失败: {}", e)))?;

    Ok(batches.into_iter().map(BatchDto::from).collect())
}

/// 获取单个批次详情
#[tauri::command]
pub fn get_batch(id: i64, state: State<Mutex<AppState>>) -> AppResult<BatchDto> {
    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    let batch = db
        .get_batch(id)
        .map_err(|e| AppError::database(format!("查询批次失败: {}", e)))?;

    Ok(BatchDto::from(batch))
}

/// 获取流水线保存的可追溯归组结果。旧的手工批次返回 null。
#[tauri::command]
pub fn get_batch_grouping(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Option<BatchGrouping>> {
    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {e}")))?;
    app_state
        .ledger_db()?
        .get_batch_grouping(batch_id)
        .map_err(|e| AppError::database(format!("读取归组结果失败: {e}")))
}

/// 创建新批次。月份不是用户创建批次时需要填写的业务字段，仅保留为旧库兼容索引。
#[tauri::command]
pub fn create_batch(name: String, state: State<Mutex<AppState>>) -> AppResult<i64> {
    // 验证输入
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::validation("批次名称不能为空"));
    }
    if name.len() > 100 {
        return Err(AppError::validation("批次名称不能超过100个字符"));
    }

    let month = Local::now().format("%Y-%m").to_string();

    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    let id = db
        .create_batch(name, &month)
        .map_err(|e| AppError::database(format!("创建批次失败: {}", e)))?;

    tracing::info!(batch_id = id, month = %month, "批次创建成功");
    Ok(id)
}

/// 转换批次状态
#[tauri::command]
pub fn transition_batch_status(
    id: i64,
    new_status: String,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    let status = string_to_status(&new_status)?;

    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    if matches!(status, BatchStatus::Submitted) {
        let invoices = db
            .list_reimbursable_invoices_by_batch(id)
            .map_err(|e| AppError::database(format!("读取审核发票失败: {e}")))?;
        if invoices.is_empty() {
            return Err(AppError::validation("空批次不能提交审核结果"));
        }
        let duplicate_count = invoices
            .iter()
            .filter(|invoice| invoice.is_duplicate)
            .count();
        if duplicate_count > 0 {
            return Err(AppError::validation(format!(
                "仍有 {duplicate_count} 张疑似重复发票未处理"
            )));
        }
        let grouping = db
            .get_batch_grouping(id)
            .map_err(|e| AppError::database(format!("读取归组审核状态失败: {e}")))?;
        if grouping.as_ref().is_some_and(grouping_has_pending_review) {
            return Err(AppError::validation("归组仍有待确认项，请先完成归组审核"));
        }
    }

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
    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    // 先检查状态
    let batch = db
        .get_batch(id)
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
        assert!(string_to_status("Draft").is_err()); // 大小写敏感
    }
}

#[cfg(test)]
fn grouping_with_ambiguities(ambiguities_json: &str) -> BatchGrouping {
    BatchGrouping {
        batch_id: 1,
        rule_version: "test".to_string(),
        home_cities_json: "[]".to_string(),
        overall_confidence: 1.0,
        ambiguities_json: ambiguities_json.to_string(),
        created_at: "2026-06-01 00:00:00".to_string(),
        groups: Vec::new(),
    }
}

#[test]
fn grouping_submission_guard_checks_ambiguities_json() {
    assert!(!grouping_has_pending_review(&grouping_with_ambiguities(
        "[]"
    )));
    assert!(grouping_has_pending_review(&grouping_with_ambiguities(
        "[\"待确认\"]"
    )));
    assert!(grouping_has_pending_review(&grouping_with_ambiguities(
        "invalid"
    )));
}

#[test]
fn grouping_submission_guard_checks_group_review_flag() {
    use invoice_store::models::InvoiceGroup;

    let mut grouping = grouping_with_ambiguities("[]");
    grouping.groups.push(InvoiceGroup {
        id: 1,
        group_index: 0,
        kind: "needs_review".to_string(),
        title: "待确认".to_string(),
        start_date: "2026-06-01".to_string(),
        end_date: "2026-06-01".to_string(),
        confidence: 0.5,
        requires_review: true,
        evidence_json: "{}".to_string(),
        members: Vec::new(),
    });
    assert!(grouping_has_pending_review(&grouping));
    grouping.groups[0].requires_review = false;
    assert!(!grouping_has_pending_review(&grouping));
}
