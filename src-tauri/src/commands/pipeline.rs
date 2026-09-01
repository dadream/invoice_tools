//! 流水线命令模块：端到端流程串联（采集 → 解析 → 去重 → 归组 → 审核 → 导出）
//!
//! 采用事件驱动架构，通过 Tauri events 实时推送进度给前端。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use invoice_collect::config::{DateRange, ImapConfig};
use invoice_collect::{classify, dedupe, extract, imap_client};
use invoice_grouping::{group_invoices, types::*, GROUPING_RULE_VERSION};
use invoice_parse::manifest::TagHints;
use invoice_parse::model::{
    ParseLevel, ParsedInvoice, TicketType as ParseTicketType, TransportDocumentKind,
};
use invoice_store::models::{
    IndexedBatchGrouping, IndexedInvoiceGroup, IndexedInvoiceGroupMember, NewEmailImportAttachment,
    NewEmailImportMessage, NewPendingInvoiceDocument, PipelineRun, ReportedInvoice,
};

use crate::commands::invoice::{
    builtin_hints, parse_level_to_string, parse_pdf_with_fallbacks, to_store_ticket_type,
};
use crate::error::{AppError, AppResult};
use crate::AppState;

use super::pipeline_cancel::{
    cancellation_requested, ensure_not_cancelled, register_active_pipeline, request_cancel,
    ActivePipelineRegistration, CancellationToken, CANCELLATION_MESSAGE,
};

/// 流水线配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    pub batch_name: String,
    pub month: String, // "2026-07"
    #[serde(default)]
    pub target_batch_id: Option<i64>,
    pub source: PipelineSource,
    pub date_range: DateRangeDto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PipelineSource {
    Email,
    CollectionImport {
        import_id: i64,
    },
    Local {
        paths: Vec<String>,
        #[serde(default)]
        target_email_message_id: Option<i64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateRangeDto {
    pub start: String, // "2026-07-01"
    pub end: String,   // UI 包含结束日，例如 "2026-07-31"
}

/// 阶段进度事件
#[derive(Debug, Clone, Serialize)]
pub struct StageProgress {
    pub stage: String, // "collect" | "parse" | "dedupe" | "group" | "review" | "export"
    pub progress: f32, // 0.0 - 1.0
    pub current: Option<usize>,
    pub total: Option<usize>,
    pub message: String,
}

/// 错误事件
#[derive(Debug, Clone, Serialize)]
pub struct PipelineError {
    pub stage: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineCancelled {
    pub stage: String,
    pub message: String,
}

/// 完成事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineComplete {
    pub batch_id: i64,
    pub invoice_count: usize,
    pub total_amount: String,
    pub excel_path: Option<String>,
    pub link_only_email_count: usize,
    pub pending_document_count: usize,
    #[serde(default)]
    pub source_file_count: usize,
    #[serde(default)]
    pub parsed_document_count: usize,
    #[serde(default)]
    pub canonical_invoice_count: usize,
    #[serde(default)]
    pub duplicate_document_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PipelineImportBaseline {
    invoice_count: i64,
    total_amount: String,
    pending_document_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoverablePipelineDto {
    pub pipeline_id: String,
    pub batch_name: String,
    pub target_batch_id: Option<i64>,
    pub month: String,
    pub source_kind: String,
    pub stage: String,
    pub status: String,
    pub last_error: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DedupeResult {
    invoices: Vec<ParsedInvoice>,
    /// 规范化后的唯一费用位于数组前部；其余元素仅用于保留同票其他格式原件。
    #[serde(default)]
    canonical_count: usize,
    #[serde(default)]
    canonicalization_version: u32,
    /// 旧版检查点按发票号保存原因；保留用于恢复已经落盘的任务。
    #[serde(default)]
    duplicate_reasons: HashMap<String, String>,
    /// 新版按输入下标保存原因，避免同号发票互相覆盖，并支持同批次查重。
    #[serde(default)]
    duplicate_reasons_by_index: Vec<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingDocumentCandidate {
    source_path: PathBuf,
    proposed_role: String,
    detection_reason: String,
}

#[derive(Debug, Default)]
struct ParseStageResult {
    invoices: Vec<ParsedInvoice>,
    pending_documents: Vec<PendingDocumentCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupedCheckpoint {
    home_city: String,
    result: GroupingResult,
    #[serde(default)]
    canonicalization_version: u32,
}

#[derive(Debug)]
struct CanonicalizedInvoices {
    invoices: Vec<ParsedInvoice>,
    canonical_count: usize,
}

const CANONICALIZATION_VERSION: u32 = 2;

const SOURCE_NOTICES_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceNotices {
    format_version: u32,
    link_only_email_count: usize,
}

impl SourceNotices {
    fn empty() -> Self {
        Self {
            format_version: SOURCE_NOTICES_FORMAT_VERSION,
            link_only_email_count: 0,
        }
    }
}

#[derive(Debug, Default)]
struct EmailCollectionResult {
    files: Vec<PathBuf>,
    link_only_email_count: usize,
    messages: Vec<NewEmailImportMessage>,
}

struct PipelineTaskContext<'a> {
    app: &'a AppHandle,
    pipeline_id: &'a str,
    cancellation: &'a CancellationToken,
}

#[tauri::command]
pub fn preview_local_import(
    paths: Vec<String>,
) -> AppResult<crate::local_source::LocalInputPreview> {
    if paths.is_empty() || paths.len() > 100 || paths.iter().any(|path| path.trim().is_empty()) {
        return Err(AppError::validation("请选择 1 至 100 个发票文件或文件夹"));
    }
    let roots = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    crate::local_source::preview_local_inputs(&roots)
        .map_err(|error| AppError::validation(format!("本地选择检查失败: {error}")))
}

/// 启动流水线。pipeline_id 由前端预先生成，使事件监听器能在任务启动前注册。
#[tauri::command]
pub async fn start_pipeline(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    pipeline_id: String,
    config: PipelineConfig,
) -> AppResult<()> {
    validate_pipeline_config(&config)?;
    uuid::Uuid::parse_str(&pipeline_id).map_err(|_| AppError::validation("流水线标识无效"))?;
    let registration = register_active_pipeline(&pipeline_id)?;

    tracing::info!(
        pipeline_id = %pipeline_id,
        batch_name = %config.batch_name,
        month = %config.month,
        "流水线启动请求"
    );

    let task_dir = get_temp_dir()?.join(&pipeline_id);
    std::fs::create_dir_all(&task_dir)?;
    let config_json = serde_json::to_string(&config)
        .map_err(|error| AppError::internal(format!("保存流水线配置失败: {error}")))?;
    let source_kind = match &config.source {
        PipelineSource::Email => "email",
        PipelineSource::CollectionImport { .. } => "collection_import",
        PipelineSource::Local { .. } => "local",
    };
    {
        let app_state = state.lock().unwrap();
        let db = app_state.ledger_db()?;
        if let Some(target_batch_id) = config.target_batch_id {
            let target = db
                .get_batch(target_batch_id)
                .map_err(|error| AppError::validation(format!("目标批次不存在：{error}")))?;
            if !matches!(target.status, invoice_store::models::BatchStatus::Draft) {
                return Err(AppError::validation("只能向审核中的批次导入数据"));
            }
        }
        db.create_pipeline_run(
            &pipeline_id,
            &config_json,
            source_kind,
            &task_dir.to_string_lossy(),
        )
        .map_err(|error| AppError::database(format!("创建流水线检查点失败: {error}")))?;
        if let PipelineSource::CollectionImport { import_id } = &config.source {
            if let Err(error) = db.link_batch_collection_import_pipeline(*import_id, &pipeline_id) {
                let _ = db.mark_pipeline_failed(&pipeline_id, &error.to_string());
                return Err(AppError::validation(format!(
                    "关联收集材料快照失败: {error}"
                )));
            }
        }
    }
    spawn_pipeline(app, pipeline_id, config, registration);
    Ok(())
}

#[tauri::command]
pub fn cancel_pipeline(pipeline_id: String) -> AppResult<()> {
    uuid::Uuid::parse_str(&pipeline_id).map_err(|_| AppError::validation("流水线标识无效"))?;
    request_cancel(&pipeline_id)?;
    tracing::info!(pipeline_id = %pipeline_id, "用户请求安全停止流水线");
    Ok(())
}

#[tauri::command]
pub fn list_recoverable_pipelines(
    state: State<'_, Mutex<AppState>>,
) -> AppResult<Vec<RecoverablePipelineDto>> {
    let app_state = state.lock().unwrap();
    let runs = app_state
        .ledger_db()?
        .list_recoverable_pipeline_runs()
        .map_err(|error| AppError::database(format!("读取可恢复任务失败: {error}")))?;
    runs.into_iter().map(recoverable_dto).collect()
}

#[tauri::command]
pub async fn resume_pipeline(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    pipeline_id: String,
) -> AppResult<()> {
    uuid::Uuid::parse_str(&pipeline_id).map_err(|_| AppError::validation("流水线标识无效"))?;
    let registration = register_active_pipeline(&pipeline_id)?;
    let config = {
        let app_state = state.lock().unwrap();
        let db = app_state.ledger_db()?;
        let run = db
            .get_pipeline_run(&pipeline_id)
            .map_err(|error| AppError::database(format!("读取恢复任务失败: {error}")))?;
        let config: PipelineConfig = serde_json::from_str(&run.config_json)
            .map_err(|error| AppError::validation(format!("恢复任务配置无效: {error}")))?;
        validate_pipeline_config(&config)?;
        db.mark_pipeline_running(&pipeline_id)
            .map_err(|error| AppError::validation(format!("任务不能恢复: {error}")))?;
        config
    };
    spawn_pipeline(app, pipeline_id, config, registration);
    Ok(())
}

fn recoverable_dto(run: PipelineRun) -> AppResult<RecoverablePipelineDto> {
    let config: PipelineConfig = serde_json::from_str(&run.config_json)
        .map_err(|error| AppError::validation(format!("恢复任务配置无效: {error}")))?;
    Ok(RecoverablePipelineDto {
        pipeline_id: run.pipeline_id,
        batch_name: config.batch_name,
        target_batch_id: config.target_batch_id,
        month: config.month,
        source_kind: run.source_kind,
        stage: run.stage,
        status: run.status,
        last_error: run.last_error,
        updated_at: run.updated_at,
    })
}

fn spawn_pipeline(
    app: AppHandle,
    pipeline_id: String,
    config: PipelineConfig,
    registration: ActivePipelineRegistration,
) {
    let pid = pipeline_id.clone();
    let app_handle = app.clone();
    let token = registration.token();
    tauri::async_runtime::spawn(async move {
        let error_app = app_handle.clone();
        let error_pid = pid.clone();
        let result = run_pipeline_impl(app_handle, pid, config, &token).await;
        if let Err(error) = result {
            if cancellation_requested(&token) {
                let stage = mark_interrupted_and_error_stage(&error_app, &error_pid);
                emit_cancelled(&error_app, &error_pid, stage);
                tracing::info!(pipeline_id = %error_pid, "流水线已在安全边界停止");
            } else {
                let stage = mark_failed_and_error_stage(&error_app, &error_pid, error.message());
                emit_error(&error_app, &error_pid, stage, error.message());
                tracing::error!(
                    pipeline_id = %error_pid,
                    stage,
                    error_kind = ?error.kind(),
                    "流水线执行失败"
                );
            }
        }
        drop(registration);
    });
}

fn mark_failed_and_error_stage(app: &AppHandle, pipeline_id: &str, message: &str) -> &'static str {
    let state = app.state::<Mutex<AppState>>();
    let Ok(app_state) = state.lock() else {
        return "collect";
    };
    let Ok(db) = app_state.ledger_db() else {
        return "collect";
    };
    if let Err(error) = db.mark_pipeline_failed(pipeline_id, message) {
        tracing::error!("无法记录流水线失败状态: {error}");
    }
    if let Err(error) = db.mark_batch_collection_import_failed(pipeline_id) {
        tracing::error!("无法记录收集材料导入失败状态: {error}");
    }
    match db.get_pipeline_run(pipeline_id).map(|run| run.stage) {
        Ok(stage) if stage == "collected" => "parse",
        Ok(stage) if stage == "parsed" => "dedupe",
        Ok(stage) if stage == "deduped" => "group",
        Ok(stage) if stage == "grouped" => "review",
        _ => "collect",
    }
}

fn mark_interrupted_and_error_stage(app: &AppHandle, pipeline_id: &str) -> &'static str {
    let state = app.state::<Mutex<AppState>>();
    let Ok(app_state) = state.lock() else {
        return "collect";
    };
    let Ok(db) = app_state.ledger_db() else {
        return "collect";
    };
    if let Err(error) = db.mark_pipeline_interrupted(pipeline_id, CANCELLATION_MESSAGE) {
        tracing::error!("无法记录流水线安全停止状态: {error}");
    }
    if let Err(error) = db.mark_batch_collection_import_failed(pipeline_id) {
        tracing::error!("无法记录收集材料导入中断状态: {error}");
    }
    match db.get_pipeline_run(pipeline_id).map(|run| run.stage) {
        Ok(stage) if stage == "collected" => "parse",
        Ok(stage) if stage == "parsed" => "dedupe",
        Ok(stage) if stage == "deduped" => "group",
        Ok(stage) if stage == "grouped" => "review",
        _ => "collect",
    }
}

fn validate_pipeline_config(config: &PipelineConfig) -> AppResult<()> {
    let batch_name = config.batch_name.trim();
    if batch_name.is_empty() {
        return Err(AppError::validation("批次名称不能为空"));
    }
    if batch_name.chars().count() > 100 {
        return Err(AppError::validation("批次名称不能超过 100 个字符"));
    }
    if config.target_batch_id.is_some_and(|id| id <= 0) {
        return Err(AppError::validation("目标批次标识无效"));
    }
    NaiveDate::parse_from_str(&format!("{}-01", config.month), "%Y-%m-%d")
        .map_err(|_| AppError::validation("月份格式必须为 YYYY-MM"))?;

    let start = NaiveDate::parse_from_str(&config.date_range.start, "%Y-%m-%d")
        .map_err(|_| AppError::validation("开始日期格式必须为 YYYY-MM-DD"))?;
    let end = NaiveDate::parse_from_str(&config.date_range.end, "%Y-%m-%d")
        .map_err(|_| AppError::validation("结束日期格式必须为 YYYY-MM-DD"))?;
    if end < start {
        return Err(AppError::validation("结束日期不能早于开始日期"));
    }

    if let PipelineSource::Local {
        paths,
        target_email_message_id,
    } = &config.source
    {
        if paths.is_empty() {
            return Err(AppError::validation("请选择至少一个发票文件或文件夹"));
        }
        if paths.len() > 100 {
            return Err(AppError::validation("一次最多选择 100 个本地来源根路径"));
        }
        if paths.iter().any(|path| path.trim().is_empty()) {
            return Err(AppError::validation("本地来源路径不能为空"));
        }
        if target_email_message_id.is_some_and(|id| id <= 0) {
            return Err(AppError::validation("邮件台账目标标识无效"));
        }
        if target_email_message_id.is_some() && config.target_batch_id.is_none() {
            return Err(AppError::validation("邮件补充导入必须指定当前目标批次"));
        }
    }
    if let PipelineSource::CollectionImport { import_id } = &config.source {
        if *import_id <= 0 {
            return Err(AppError::validation("收集材料快照标识无效"));
        }
        if config.target_batch_id.is_none() {
            return Err(AppError::validation("收集材料必须导入到指定报销批次"));
        }
    }
    Ok(())
}

/// 流水线核心执行逻辑（从 app 中获取 state）
async fn run_pipeline_impl(
    app: AppHandle,
    pipeline_id: String,
    config: PipelineConfig,
    cancellation: &CancellationToken,
) -> AppResult<()> {
    tracing::info!(
        pipeline_id = %pipeline_id,
        batch_name = %config.batch_name,
        "流水线启动"
    );
    ensure_not_cancelled(cancellation)?;

    let state = app.state::<Mutex<AppState>>();
    let run = {
        let app_state = state.lock().unwrap();
        app_state
            .ledger_db()?
            .get_pipeline_run(&pipeline_id)
            .map_err(|error| AppError::database(format!("读取流水线状态失败: {error}")))?
    };
    let task_dir = PathBuf::from(&run.task_dir);
    let expected_task_dir = get_temp_dir()?.join(&pipeline_id);
    if task_dir != expected_task_dir {
        return Err(AppError::validation("流水线任务目录与产品数据目录不匹配"));
    }
    if run.status == "completed" {
        if crate::pipeline_checkpoint::checkpoint_exists(&task_dir, "complete") {
            let result = crate::pipeline_checkpoint::read_json_checkpoint::<PipelineComplete>(
                &task_dir, "complete",
            )?;
            if run.source_kind == "collection_import" {
                let app_state = state.lock().unwrap();
                app_state
                    .ledger_db()?
                    .mark_batch_collection_import_completed(&pipeline_id)
                    .map_err(|error| {
                        AppError::database(format!("恢复收集材料导入状态失败: {error}"))
                    })?;
            }
            emit_complete(&app, &pipeline_id, result);
            return Ok(());
        }
        let source_notices = read_source_notices(&task_dir)?;
        let batch_id = run
            .batch_id
            .ok_or_else(|| AppError::database("已完成流水线缺少批次标识"))?;
        let current = pipeline_import_baseline(&state, Some(batch_id))?;
        let (invoice_count, total_amount, pending_document_count) =
            if crate::pipeline_checkpoint::checkpoint_exists(&task_dir, "store-baseline") {
                let baseline = crate::pipeline_checkpoint::read_json_checkpoint::<
                    PipelineImportBaseline,
                >(&task_dir, "store-baseline")?;
                let baseline_total = baseline
                    .total_amount
                    .parse::<rust_decimal::Decimal>()
                    .map_err(|error| AppError::database(format!("读取导入前金额失败: {error}")))?;
                let current_total = current
                    .total_amount
                    .parse::<rust_decimal::Decimal>()
                    .map_err(|error| AppError::database(format!("读取导入后金额失败: {error}")))?;
                (
                    usize::try_from(current.invoice_count.saturating_sub(baseline.invoice_count))
                        .unwrap_or_default(),
                    if current_total >= baseline_total {
                        (current_total - baseline_total).to_string()
                    } else {
                        "0".to_string()
                    },
                    current
                        .pending_document_count
                        .saturating_sub(baseline.pending_document_count),
                )
            } else {
                // 兼容功能升级前已经完成、没有增量基线的任务。
                (
                    usize::try_from(current.invoice_count).unwrap_or_default(),
                    current.total_amount,
                    if crate::pipeline_checkpoint::checkpoint_exists(&task_dir, "materials") {
                        crate::pipeline_checkpoint::read_json_checkpoint::<
                            Vec<PendingDocumentCandidate>,
                        >(&task_dir, "materials")?
                        .len()
                    } else {
                        0
                    },
                )
            };
        let result = PipelineComplete {
            batch_id,
            invoice_count,
            total_amount,
            excel_path: None,
            link_only_email_count: source_notices.link_only_email_count,
            pending_document_count,
            source_file_count: 0,
            parsed_document_count: 0,
            canonical_invoice_count: invoice_count,
            duplicate_document_count: 0,
        };
        crate::pipeline_checkpoint::write_json_checkpoint(&task_dir, "complete", &result)?;
        if run.source_kind == "collection_import" {
            let app_state = state.lock().unwrap();
            app_state
                .ledger_db()?
                .mark_batch_collection_import_completed(&pipeline_id)
                .map_err(|error| {
                    AppError::database(format!("恢复收集材料导入状态失败: {error}"))
                })?;
        }
        emit_complete(&app, &pipeline_id, result);
        return Ok(());
    }

    let (files, source_notices, mut email_messages) =
        if crate::pipeline_checkpoint::checkpoint_exists(&task_dir, "collected") {
            emit_progress(
                &app,
                &pipeline_id,
                "collect",
                0.95,
                0,
                None,
                "校验已保存的采集检查点...",
            );
            let email_messages =
                if crate::pipeline_checkpoint::checkpoint_exists(&task_dir, "email-ledger") {
                    crate::pipeline_checkpoint::read_json_checkpoint::<Vec<NewEmailImportMessage>>(
                        &task_dir,
                        "email-ledger",
                    )?
                } else {
                    Vec::new()
                };
            (
                crate::pipeline_checkpoint::load_collected(&task_dir)?,
                read_source_notices(&task_dir)?,
                email_messages,
            )
        } else {
            let (collected, source_notices, email_messages) = match &config.source {
                PipelineSource::Email => {
                    emit_progress(
                        &app,
                        &pipeline_id,
                        "collect",
                        0.0,
                        0,
                        None,
                        "连接邮箱服务器...",
                    );
                    // 授权码只从当前进程会话读取，不访问 accounts.db credentials 表。
                    let (email, password) = {
                        let app_state = state.lock().unwrap();
                        app_state.session_credential_copy().ok_or_else(|| {
                            AppError::validation("本次会话尚未输入邮箱授权码，请先到设置中配置")
                        })?
                    };
                    let collected = collect_email_invoices(
                        &app,
                        &pipeline_id,
                        &email,
                        &password,
                        &config,
                        cancellation,
                    )
                    .await?;
                    let notices = SourceNotices {
                        format_version: SOURCE_NOTICES_FORMAT_VERSION,
                        link_only_email_count: collected.link_only_email_count,
                    };
                    (collected.files, notices, collected.messages)
                }
                PipelineSource::CollectionImport { import_id } => {
                    let target_batch_id = config
                        .target_batch_id
                        .ok_or_else(|| AppError::validation("收集材料缺少目标报销批次"))?;
                    emit_progress(
                        &app,
                        &pipeline_id,
                        "collect",
                        0.1,
                        0,
                        None,
                        "读取已冻结的收集材料快照...",
                    );
                    let roots = {
                        let app_state = state.lock().unwrap();
                        app_state
                            .ledger_db()?
                            .collection_import_file_paths(*import_id, target_batch_id)
                            .map_err(|error| {
                                AppError::validation(format!("读取收集材料快照失败: {error}"))
                            })?
                            .into_iter()
                            .map(PathBuf::from)
                            .collect::<Vec<_>>()
                    };
                    let data_root = crate::paths::data_root()
                        .map_err(|error| AppError::io(format!("无法定位数据目录: {error}")))?;
                    let roots = resolve_managed_material_paths(&data_root, &roots)?;
                    let staging = task_dir.join("collection-import");
                    let collected = crate::local_source::collect_local_inputs(&roots, &staging)
                        .map_err(|error| {
                            AppError::io(format!("复制收集材料到批次暂存区失败: {error}"))
                        })?;
                    emit_progress(
                        &app,
                        &pipeline_id,
                        "collect",
                        1.0,
                        collected.files.len(),
                        Some(collected.files.len()),
                        &format!("已装载 {} 个收集材料，开始解析", collected.files.len()),
                    );
                    (collected.files, SourceNotices::empty(), Vec::new())
                }
                PipelineSource::Local {
                    paths,
                    target_email_message_id,
                } => {
                    emit_progress(
                        &app,
                        &pipeline_id,
                        "collect",
                        0.1,
                        0,
                        Some(paths.len()),
                        "扫描所选本地文件...",
                    );
                    let roots: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
                    let staging = task_dir.join("local");
                    let collected = crate::local_source::collect_local_inputs(&roots, &staging)
                        .map_err(|error| AppError::io(format!("本地来源读取失败: {error}")))?;
                    emit_progress(
                        &app,
                        &pipeline_id,
                        "collect",
                        1.0,
                        collected.files.len(),
                        Some(collected.files.len()),
                        &format!(
                            "本地收集完成：{} 个可解析文件，{} 个重复，{} 个已跳过",
                            collected.files.len(),
                            collected.duplicates,
                            collected.skipped
                        ),
                    );
                    let notices = SourceNotices {
                        format_version: SOURCE_NOTICES_FORMAT_VERSION,
                        link_only_email_count: collected.link_only_emails,
                    };
                    let email_messages = target_email_message_id
                        .map(|message_id| {
                            manual_email_supplement_message(message_id, &collected.files)
                        })
                        .transpose()?
                        .into_iter()
                        .collect();
                    (collected.files, notices, email_messages)
                }
            };
            crate::pipeline_checkpoint::write_json_checkpoint(
                &task_dir,
                "source-notices",
                &source_notices,
            )?;
            crate::pipeline_checkpoint::write_collected(&task_dir, &collected)?;
            crate::pipeline_checkpoint::write_json_checkpoint(
                &task_dir,
                "email-ledger",
                &email_messages,
            )?;
            (collected, source_notices, email_messages)
        };
    if files.is_empty() {
        if email_messages.is_empty() {
            return Err(empty_collection_error(&source_notices));
        }
        let batch_id = {
            let app_state = state.lock().unwrap();
            app_state
                .ledger_db()?
                .complete_pipeline_with_email_ledger_only(
                    &pipeline_id,
                    &config.batch_name,
                    &config.month,
                    config.target_batch_id,
                    &email_messages,
                )
                .map_err(|error| AppError::database(format!("保存邮件处理台账失败: {error}")))?
        };
        let result = PipelineComplete {
            batch_id,
            invoice_count: 0,
            total_amount: "0".to_string(),
            excel_path: None,
            link_only_email_count: source_notices.link_only_email_count,
            pending_document_count: 0,
            source_file_count: 0,
            parsed_document_count: 0,
            canonical_invoice_count: 0,
            duplicate_document_count: 0,
        };
        crate::pipeline_checkpoint::write_json_checkpoint(&task_dir, "complete", &result)?;
        emit_complete(&app, &pipeline_id, result);
        return Ok(());
    }
    record_checkpoint(&state, &pipeline_id, "collected")?;
    ensure_not_cancelled(cancellation)?;

    let parse_result = if crate::pipeline_checkpoint::checkpoint_exists(&task_dir, "parsed") {
        emit_progress(
            &app,
            &pipeline_id,
            "parse",
            0.95,
            0,
            None,
            "校验解析检查点...",
        );
        let parsed = crate::pipeline_checkpoint::load_parsed(&task_dir)?;
        crate::pipeline_checkpoint::validate_parsed_sources(&parsed, &files)?;
        let pending_documents =
            if crate::pipeline_checkpoint::checkpoint_exists(&task_dir, "materials") {
                crate::pipeline_checkpoint::read_json_checkpoint::<Vec<PendingDocumentCandidate>>(
                    &task_dir,
                    "materials",
                )?
            } else {
                Vec::new()
            };
        validate_pending_document_sources(&pending_documents, &files)?;
        ParseStageResult {
            invoices: parsed,
            pending_documents,
        }
    } else {
        emit_progress(
            &app,
            &pipeline_id,
            "parse",
            0.0,
            0,
            Some(files.len()),
            "开始解析发票...",
        );
        let parsed = parse_invoices(&app, &pipeline_id, &files, cancellation).await?;
        if parsed.invoices.is_empty() && parsed.pending_documents.is_empty() {
            return Err(AppError::parse(
                "没有识别到可计入费用的主发票；未识别文件已保留在任务检查点",
            ));
        }
        crate::pipeline_checkpoint::validate_parsed_sources(&parsed.invoices, &files)?;
        validate_pending_document_sources(&parsed.pending_documents, &files)?;
        crate::pipeline_checkpoint::write_parsed(&task_dir, &parsed.invoices)?;
        crate::pipeline_checkpoint::write_json_checkpoint(
            &task_dir,
            "materials",
            &parsed.pending_documents,
        )?;
        parsed
    };
    let parsed = parse_result.invoices;
    let pending_documents = parse_result.pending_documents;
    record_checkpoint(&state, &pipeline_id, "parsed")?;
    ensure_not_cancelled(cancellation)?;

    let canonicalized = canonicalize_parsed_invoices(parsed);

    let mut checked: DedupeResult =
        if crate::pipeline_checkpoint::checkpoint_exists(&task_dir, "deduped") {
            emit_progress(
                &app,
                &pipeline_id,
                "dedupe",
                0.95,
                0,
                None,
                "校验去重检查点...",
            );
            let existing: DedupeResult =
                crate::pipeline_checkpoint::read_json_checkpoint(&task_dir, "deduped")?;
            if existing.canonicalization_version == CANONICALIZATION_VERSION
                && (existing.canonical_count > 0 || existing.invoices.is_empty())
                && existing.canonical_count <= existing.invoices.len()
            {
                crate::pipeline_checkpoint::validate_parsed_sources(&existing.invoices, &files)?;
                existing
            } else {
                emit_progress(
                    &app,
                    &pipeline_id,
                    "dedupe",
                    0.0,
                    0,
                    Some(canonicalized.canonical_count),
                    "旧检查点不含唯一发票规范化结果，重新检查重复发票...",
                );
                let checked =
                    dedupe_invoices(&app, &pipeline_id, &state, canonicalized, cancellation)
                        .await?;
                crate::pipeline_checkpoint::write_json_checkpoint(&task_dir, "deduped", &checked)?;
                checked
            }
        } else {
            emit_progress(
                &app,
                &pipeline_id,
                "dedupe",
                0.0,
                0,
                Some(canonicalized.canonical_count),
                "检查重复发票...",
            );
            let checked =
                dedupe_invoices(&app, &pipeline_id, &state, canonicalized, cancellation).await?;
            crate::pipeline_checkpoint::write_json_checkpoint(&task_dir, "deduped", &checked)?;
            checked
        };
    if checked.canonical_count > checked.invoices.len()
        || (checked.canonical_count == 0 && !checked.invoices.is_empty())
    {
        return Err(AppError::validation(
            "唯一发票规范化检查点无效，任务不会继续",
        ));
    }
    enrich_invoices_from_supporting_documents(
        &mut checked.invoices[..checked.canonical_count],
        &pending_documents,
    );
    bind_email_ledger_associations(&mut email_messages, &checked.invoices, &pending_documents)?;
    record_checkpoint(&state, &pipeline_id, "deduped")?;
    ensure_not_cancelled(cancellation)?;

    let reusable_grouped: Option<GroupedCheckpoint> =
        if crate::pipeline_checkpoint::checkpoint_exists(&task_dir, "grouped") {
            emit_progress(
                &app,
                &pipeline_id,
                "group",
                0.95,
                0,
                None,
                "校验归组检查点...",
            );
            let checkpoint: GroupedCheckpoint =
                crate::pipeline_checkpoint::read_json_checkpoint(&task_dir, "grouped")?;
            (checkpoint.canonicalization_version == CANONICALIZATION_VERSION).then_some(checkpoint)
        } else {
            None
        };
    let grouped: GroupedCheckpoint = if let Some(grouped) = reusable_grouped {
        grouped
    } else {
        emit_progress(&app, &pipeline_id, "group", 0.0, 0, None, "归组行程...");
        let (home_city, home_station_aliases) = {
            let app_state = state.lock().unwrap();
            let db = app_state.ledger_db()?;
            let home_city = db
                .get_setting("home_city")
                .map_err(|error| AppError::database(format!("读取常驻城市失败: {error}")))?
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| AppError::validation("请先在设置中填写常驻城市"))?;
            let aliases = super::settings::load_effective_home_station_aliases(db, &home_city)?;
            (home_city, aliases)
        };
        let result = group_invoices_stage(
            &app,
            &pipeline_id,
            &checked.invoices[..checked.canonical_count],
            &home_city,
            &home_station_aliases,
            cancellation,
        )
        .await?;
        let grouped = GroupedCheckpoint {
            home_city,
            result,
            canonicalization_version: CANONICALIZATION_VERSION,
        };
        crate::pipeline_checkpoint::write_json_checkpoint(&task_dir, "grouped", &grouped)?;
        grouped
    };
    record_checkpoint(&state, &pipeline_id, "grouped")?;
    ensure_not_cancelled(cancellation)?;

    emit_progress(
        &app,
        &pipeline_id,
        "review",
        0.0,
        0,
        Some(checked.invoices.len()),
        "保存草稿，准备人工审核...",
    );
    let task_context = PipelineTaskContext {
        app: &app,
        pipeline_id: &pipeline_id,
        cancellation,
    };
    let baseline = if crate::pipeline_checkpoint::checkpoint_exists(&task_dir, "store-baseline") {
        crate::pipeline_checkpoint::read_json_checkpoint::<PipelineImportBaseline>(
            &task_dir,
            "store-baseline",
        )?
    } else {
        let baseline = pipeline_import_baseline(&state, config.target_batch_id)?;
        crate::pipeline_checkpoint::write_json_checkpoint(&task_dir, "store-baseline", &baseline)?;
        baseline
    };
    let batch_id = store_batch(
        &task_context,
        &state,
        &config,
        &checked,
        &grouped.result,
        &grouped.home_city,
        &pending_documents,
        &email_messages,
    )
    .await?;
    emit_progress(
        &app,
        &pipeline_id,
        "review",
        1.0,
        checked.invoices.len(),
        Some(checked.invoices.len()),
        "草稿已生成，请在批次管理中逐项审核；审核完成前不能导出。",
    );

    let current = pipeline_import_baseline(&state, Some(batch_id))?;
    let baseline_total = baseline
        .total_amount
        .parse::<rust_decimal::Decimal>()
        .map_err(|error| AppError::database(format!("读取导入前金额失败: {error}")))?;
    let current_total = current
        .total_amount
        .parse::<rust_decimal::Decimal>()
        .map_err(|error| AppError::database(format!("读取导入后金额失败: {error}")))?;
    let added_total = if current_total >= baseline_total {
        current_total - baseline_total
    } else {
        rust_decimal::Decimal::ZERO
    };
    let result = PipelineComplete {
        batch_id,
        invoice_count: usize::try_from(
            current.invoice_count.saturating_sub(baseline.invoice_count),
        )
        .unwrap_or_default(),
        total_amount: added_total.to_string(),
        excel_path: None,
        link_only_email_count: source_notices.link_only_email_count,
        pending_document_count: current
            .pending_document_count
            .saturating_sub(baseline.pending_document_count),
        source_file_count: files.len(),
        parsed_document_count: checked.invoices.len(),
        canonical_invoice_count: checked.canonical_count,
        duplicate_document_count: checked
            .invoices
            .len()
            .saturating_sub(checked.canonical_count),
    };
    crate::pipeline_checkpoint::write_json_checkpoint(&task_dir, "complete", &result)?;
    if matches!(&config.source, PipelineSource::CollectionImport { .. }) {
        let app_state = state.lock().unwrap();
        app_state
            .ledger_db()?
            .mark_batch_collection_import_completed(&pipeline_id)
            .map_err(|error| AppError::database(format!("完成收集材料导入状态失败: {error}")))?;
    }
    emit_complete(&app, &pipeline_id, result.clone());

    tracing::info!(
        pipeline_id = %pipeline_id,
        batch_id,
        invoice_count = result.invoice_count,
        "流水线已生成待审核草稿"
    );

    Ok(())
}

fn pipeline_import_baseline(
    state: &State<'_, Mutex<AppState>>,
    batch_id: Option<i64>,
) -> AppResult<PipelineImportBaseline> {
    let Some(batch_id) = batch_id else {
        return Ok(PipelineImportBaseline {
            invoice_count: 0,
            total_amount: "0".to_string(),
            pending_document_count: 0,
        });
    };
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;
    let batch = db
        .get_batch(batch_id)
        .map_err(|error| AppError::database(format!("读取批次导入统计失败: {error}")))?;
    let pending_document_count = db
        .list_pending_invoice_documents(batch_id)
        .map_err(|error| AppError::database(format!("读取批次待处理材料失败: {error}")))?
        .into_iter()
        .filter(|document| document.status == "pending")
        .count();
    Ok(PipelineImportBaseline {
        invoice_count: i64::from(batch.invoice_count),
        total_amount: batch.total_amount.to_string(),
        pending_document_count,
    })
}

fn record_checkpoint(
    state: &State<'_, Mutex<AppState>>,
    pipeline_id: &str,
    stage: &str,
) -> AppResult<()> {
    let app_state = state.lock().unwrap();
    app_state
        .ledger_db()?
        .update_pipeline_checkpoint(pipeline_id, stage)
        .map_err(|error| AppError::database(format!("记录流水线 {stage} 检查点失败: {error}")))
}

fn read_source_notices(task_dir: &Path) -> AppResult<SourceNotices> {
    if !crate::pipeline_checkpoint::checkpoint_exists(task_dir, "source-notices") {
        // 兼容 LINK-001 上线前已创建的任务。
        return Ok(SourceNotices::empty());
    }
    let notices: SourceNotices =
        crate::pipeline_checkpoint::read_json_checkpoint(task_dir, "source-notices")?;
    if notices.format_version != SOURCE_NOTICES_FORMAT_VERSION {
        return Err(AppError::validation("来源提示检查点版本不受支持"));
    }
    Ok(notices)
}

fn empty_collection_error(notices: &SourceNotices) -> AppError {
    if notices.link_only_email_count > 0 {
        AppError::validation(format!(
            "未找到可直接解析的发票附件；发现 {} 封疑似通过正文链接交付发票的邮件。为避免访问不可信链接，软件不会自动打开或下载。请在邮箱客户端核对发件人和域名，手动下载发票后返回“本地文件”导入",
            notices.link_only_email_count
        ))
    } else {
        AppError::validation("未找到任何发票附件")
    }
}

/// Stage 1: 采集发票附件
async fn collect_email_invoices(
    app: &AppHandle,
    pipeline_id: &str,
    email: &str,
    password: &str,
    config: &PipelineConfig,
    cancellation: &CancellationToken,
) -> AppResult<EmailCollectionResult> {
    ensure_not_cancelled(cancellation)?;
    // UI 的结束日包含当天；IMAP 使用半开区间 [since, before)。
    let inclusive_end = NaiveDate::parse_from_str(&config.date_range.end, "%Y-%m-%d")
        .map_err(|_| AppError::validation("结束日期格式必须为 YYYY-MM-DD"))?;
    let before = inclusive_end
        .succ_opt()
        .ok_or_else(|| AppError::validation("结束日期超出支持范围"))?;
    let before_text = before.format("%Y-%m-%d").to_string();
    let date_range = DateRange::parse(&config.date_range.start, &before_text)
        .map_err(|e| AppError::validation(format!("日期范围格式错误: {}", e)))?;

    let imap_config = ImapConfig::from_credentials(email, password)
        .map_err(|e| AppError::validation(format!("邮箱配置错误: {}", e)))?;

    emit_progress(
        app,
        pipeline_id,
        "collect",
        0.1,
        0,
        None,
        "连接 IMAP 服务器...",
    );

    let mut session = imap_client::Session::connect(&imap_config)
        .map_err(|e| AppError::network(format!("IMAP 连接失败: {}", e)))?;

    emit_progress(app, pipeline_id, "collect", 0.3, 0, None, "搜索发票邮件...");

    let uids = session
        .search_range("INBOX", &date_range)
        .map_err(|e| AppError::network(format!("搜索邮件失败: {}", e)))?;

    if uids.is_empty() {
        return Ok(EmailCollectionResult::default());
    }

    let summaries = session
        .fetch_summaries(&uids)
        .map_err(|e| AppError::network(format!("读取邮件概要失败: {e}")))?;
    let summary_by_uid = summaries
        .into_iter()
        .map(|summary| (summary.uid, summary))
        .collect::<HashMap<_, _>>();

    emit_progress(
        app,
        pipeline_id,
        "collect",
        0.5,
        0,
        Some(uids.len()),
        &format!("找到 {} 封邮件，开始下载附件...", uids.len()),
    );

    // 创建临时目录
    let temp_dir = get_temp_dir()?.join(pipeline_id).join("email");
    std::fs::create_dir_all(&temp_dir)?;

    let mut files = Vec::new();
    let mut staged_bytes = 0u64;
    let mut deduper = dedupe::Deduper::new();
    let mut link_only_email_count = 0usize;
    let mut messages = Vec::with_capacity(uids.len());

    for (idx, uid) in uids.iter().enumerate() {
        if cancellation_requested(cancellation) {
            break;
        }
        emit_progress(
            app,
            pipeline_id,
            "collect",
            0.5 + 0.5 * (idx as f32 / uids.len() as f32),
            idx,
            Some(uids.len()),
            &format!("处理邮件 {}/{}", idx + 1, uids.len()),
        );

        let raw = match session.fetch_raw(*uid) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(uid, "获取邮件失败: {}", e);
                let summary = summary_by_uid.get(uid);
                messages.push(NewEmailImportMessage {
                    existing_message_id: None,
                    mailbox_folder: "INBOX".to_string(),
                    uid: i64::from(*uid),
                    message_id_sha256: None,
                    sender: ledger_text(
                        summary
                            .map(|value| value.from.as_str())
                            .unwrap_or("(未知发件人)"),
                        500,
                    ),
                    subject: ledger_text(
                        summary
                            .map(|value| value.subject.as_str())
                            .unwrap_or("(无主题)"),
                        1_000,
                    ),
                    received_at: summary.map(|value| value.internal_date.clone()),
                    initial_status: "failed".to_string(),
                    error_category: Some("fetch_failed".to_string()),
                    attachments: Vec::new(),
                });
                continue;
            }
        };

        let email = match extract::extract_email(&raw) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(uid, "解析邮件失败: {}", e);
                let summary = summary_by_uid.get(uid);
                messages.push(NewEmailImportMessage {
                    existing_message_id: None,
                    mailbox_folder: "INBOX".to_string(),
                    uid: i64::from(*uid),
                    message_id_sha256: None,
                    sender: ledger_text(
                        summary
                            .map(|value| value.from.as_str())
                            .unwrap_or("(未知发件人)"),
                        500,
                    ),
                    subject: ledger_text(
                        summary
                            .map(|value| value.subject.as_str())
                            .unwrap_or("(无主题)"),
                        1_000,
                    ),
                    received_at: summary.map(|value| value.internal_date.clone()),
                    initial_status: "failed".to_string(),
                    error_category: Some("mime_parse_failed".to_string()),
                    attachments: Vec::new(),
                });
                continue;
            }
        };

        let mut accepted_candidate = false;
        let mut ledger_attachments = Vec::new();
        // 分类和去重
        for att in &email.attachments {
            // 解压 ZIP（如果需要）
            let expanded = extract::extract_zip_if_needed(att);

            if expanded.is_empty() {
                ledger_attachments.push(NewEmailImportAttachment {
                    content_sha256: Some(dedupe::sha256_hex(&att.data)),
                    original_name: ledger_text(&att.filename, 500),
                    container_name: None,
                    mime_type: Some(ledger_text(&att.content_type, 200)),
                    byte_len: i64::try_from(att.data.len()).unwrap_or(i64::MAX),
                    status: "failed".to_string(),
                    role_hint: "unknown".to_string(),
                    reason: "archive_invalid_or_unsafe".to_string(),
                    is_content_duplicate: false,
                    invoice_input_index: None,
                    pending_document_index: None,
                    manual_import: false,
                });
                continue;
            }

            for item in expanded {
                let content_hash = dedupe::sha256_hex(&item.data);
                let container_name = att
                    .filename
                    .to_ascii_lowercase()
                    .ends_with(".zip")
                    .then(|| ledger_text(&att.filename, 500));
                // 复用采集层分类器，避免把邮件签名图或 Logo 送入 OCR。
                if classify::classify_attachment(&email, &item).is_none() {
                    ledger_attachments.push(NewEmailImportAttachment {
                        content_sha256: Some(content_hash),
                        original_name: ledger_text(&item.filename, 500),
                        container_name,
                        mime_type: Some(ledger_text(&item.content_type, 200)),
                        byte_len: i64::try_from(item.data.len()).unwrap_or(i64::MAX),
                        status: "not_invoice".to_string(),
                        role_hint: "unknown".to_string(),
                        reason: "classifier_rejected".to_string(),
                        is_content_duplicate: false,
                        invoice_input_index: None,
                        pending_document_index: None,
                        manual_import: false,
                    });
                    continue;
                }
                let item_bytes = item.data.len() as u64;
                if item_bytes == 0 || item_bytes > crate::local_source::MAX_FILE_BYTES {
                    tracing::warn!(uid, "邮箱附件为空或超过单文件大小上限，已跳过");
                    ledger_attachments.push(NewEmailImportAttachment {
                        content_sha256: Some(content_hash),
                        original_name: ledger_text(&item.filename, 500),
                        container_name,
                        mime_type: Some(ledger_text(&item.content_type, 200)),
                        byte_len: i64::try_from(item.data.len()).unwrap_or(i64::MAX),
                        status: "unsupported".to_string(),
                        role_hint: "unknown".to_string(),
                        reason: if item_bytes == 0 {
                            "empty_attachment".to_string()
                        } else {
                            "attachment_too_large".to_string()
                        },
                        is_content_duplicate: false,
                        invoice_input_index: None,
                        pending_document_index: None,
                        manual_import: false,
                    });
                    continue;
                }
                accepted_candidate = true;

                // 重复附件仍表示邮件不是“仅链接交付”，但不再重复暂存。
                let is_content_duplicate = !deduper.is_new(None, &item.data);
                ledger_attachments.push(NewEmailImportAttachment {
                    content_sha256: Some(content_hash.clone()),
                    original_name: ledger_text(&item.filename, 500),
                    container_name,
                    mime_type: Some(ledger_text(&item.content_type, 200)),
                    byte_len: i64::try_from(item.data.len()).unwrap_or(i64::MAX),
                    status: if is_content_duplicate {
                        "duplicate".to_string()
                    } else {
                        "failed".to_string()
                    },
                    role_hint: "unknown".to_string(),
                    reason: if is_content_duplicate {
                        "same_content_seen_in_import".to_string()
                    } else {
                        "candidate_pending_parse".to_string()
                    },
                    is_content_duplicate,
                    invoice_input_index: None,
                    pending_document_index: None,
                    manual_import: false,
                });
                if is_content_duplicate {
                    continue;
                }
                let next_total = crate::local_source::checked_staged_total(
                    files.len(),
                    staged_bytes,
                    item_bytes,
                )
                .map_err(|error| AppError::validation(error.to_string()))?;
                let staged_name = email_staging_filename(&content_hash, &item.filename);
                let file_path = temp_dir.join(staged_name);
                std::fs::write(&file_path, &item.data)?;
                files.push(file_path);
                staged_bytes = next_total;
            }
        }
        if email.invoice_link_hint && !accepted_candidate {
            link_only_email_count = link_only_email_count
                .checked_add(1)
                .ok_or_else(|| AppError::internal("疑似链接邮件计数溢出"))?;
        }
        let has_attachment_problem = ledger_attachments
            .iter()
            .any(|attachment| matches!(attachment.status.as_str(), "failed" | "unsupported"));
        let initial_status = if accepted_candidate || has_attachment_problem {
            "needs_attachment_review"
        } else if email.invoice_link_hint {
            "manual_download"
        } else if email.invoice_notice_hint {
            "needs_confirmation"
        } else {
            "not_invoice"
        };
        let summary = summary_by_uid.get(uid);
        messages.push(NewEmailImportMessage {
            existing_message_id: None,
            mailbox_folder: "INBOX".to_string(),
            uid: i64::from(*uid),
            message_id_sha256: email
                .message_id
                .as_deref()
                .map(|value| dedupe::sha256_hex(value.as_bytes())),
            sender: ledger_text(&email.from, 500),
            subject: ledger_text(&email.subject, 1_000),
            received_at: summary.map(|value| value.internal_date.clone()),
            initial_status: initial_status.to_string(),
            error_category: None,
            attachments: ledger_attachments,
        });
    }
    session
        .verify_read_only_unchanged("INBOX")
        .map_err(|e| AppError::network(format!("邮箱只读复核失败: {e}")))?;
    ensure_not_cancelled(cancellation)?;

    Ok(EmailCollectionResult {
        files,
        link_only_email_count,
        messages,
    })
}

/// Stage 2: 解析发票
async fn parse_invoices(
    app: &AppHandle,
    pipeline_id: &str,
    files: &[PathBuf],
    cancellation: &CancellationToken,
) -> AppResult<ParseStageResult> {
    let mut result = ParseStageResult::default();
    let hints = builtin_hints();

    for (idx, file) in files.iter().enumerate() {
        ensure_not_cancelled(cancellation)?;
        emit_progress(
            app,
            pipeline_id,
            "parse",
            idx as f32 / files.len() as f32,
            idx,
            Some(files.len()),
            &format!("解析 {}/{}", idx + 1, files.len()),
        );

        if let Some(document) = classify_supporting_document_before_parse(file) {
            result.pending_documents.push(document);
            continue;
        }

        match parse_single_invoice(file, &hints) {
            Ok(invoice) => result.invoices.push(invoice),
            Err(e) => {
                tracing::warn!(
                    index = idx + 1,
                    extension = %file.extension().and_then(|value| value.to_str()).unwrap_or("unknown"),
                    category = parse_failure_category(&e),
                    "单个文件解析失败"
                );
                result
                    .pending_documents
                    .push(classify_pending_document(file, &e));
                // 单个文件失败不终止流水线
            }
        }
        ensure_not_cancelled(cancellation)?;
    }

    Ok(result)
}

/// 行程单、配送明细和酒店结账单是主发票的配套材料，不应先解析成一条独立费用。
/// 只有结构特征足够明确时才在主解析前拦截；无法唯一关联时仍进入待处理清单。
fn classify_supporting_document_before_parse(path: &Path) -> Option<PendingDocumentCandidate> {
    let facts = supporting_facts_for_path(path)?;
    let (proposed_role, detection_reason) = match facts.kind.as_str() {
        "ride_hailing_itinerary" => ("itinerary", "itinerary_detected"),
        "courier_detail" => ("detail", "detail_detected"),
        "hotel_folio" => ("supporting", "hotel_folio_detected"),
        _ => return None,
    };
    Some(PendingDocumentCandidate {
        source_path: path.to_path_buf(),
        proposed_role: proposed_role.to_string(),
        detection_reason: detection_reason.to_string(),
    })
}

fn validate_pending_document_sources(
    pending_documents: &[PendingDocumentCandidate],
    collected: &[PathBuf],
) -> AppResult<()> {
    crate::pipeline_checkpoint::validate_collected_source_paths(
        pending_documents
            .iter()
            .map(|document| document.source_path.as_path()),
        collected,
        "待挂载材料检查点引用了采集清单之外的文件，任务不会继续",
    )
}

/// Collection snapshots store paths in ledger.db so the selection remains immutable. Treat those
/// paths as untrusted on every use: only ordinary files inside the managed collection library may
/// enter the parser. This also prevents a restored or modified database from reading arbitrary
/// user files.
fn resolve_managed_material_paths(
    data_root: &Path,
    locators: &[PathBuf],
) -> AppResult<Vec<PathBuf>> {
    if locators.is_empty() {
        return Err(AppError::validation("收集材料快照为空，无法导入批次"));
    }
    let mut managed_roots = Vec::new();
    for name in ["collection-files", "files"] {
        let root = data_root.join(name);
        if !root.exists() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&root)
            .map_err(|_| AppError::validation("受控材料库路径无效"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(AppError::validation("受控材料库必须是普通本地目录"));
        }
        managed_roots.push(
            std::fs::canonicalize(root).map_err(|_| AppError::validation("受控材料库路径无效"))?,
        );
    }
    if managed_roots.is_empty() {
        return Err(AppError::validation(
            "邮件收集材料库不存在，请重新收集或恢复完整备份",
        ));
    }

    let mut validated = Vec::with_capacity(locators.len());
    for locator in locators {
        let path = resolve_material_locator(data_root, locator)?;
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| AppError::validation("收集材料不存在，请重新收集或恢复完整备份"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(AppError::validation("收集材料必须是普通本地文件"));
        }
        let canonical =
            std::fs::canonicalize(path).map_err(|_| AppError::validation("收集材料路径无效"))?;
        if !managed_roots.iter().any(|root| canonical.starts_with(root)) {
            return Err(AppError::validation("收集材料越过受控材料库，任务不会继续"));
        }
        validated.push(canonical);
    }
    Ok(validated)
}

fn resolve_material_locator(data_root: &Path, locator: &Path) -> AppResult<PathBuf> {
    if locator.is_relative() {
        let mut components = locator.components();
        let first = components.next();
        if !matches!(first, Some(std::path::Component::Normal(name)) if name == "collection-files" || name == "files")
            || components.any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(AppError::validation("收集材料定位符无效"));
        }
        return Ok(data_root.join(locator));
    }
    if locator.exists() {
        return Ok(locator.to_path_buf());
    }

    // v13 preview builds briefly stored absolute paths. Rebase their managed suffix so backups
    // made by those builds can still move to a different Windows profile or drive.
    let components = locator.components().collect::<Vec<_>>();
    let marker = components.iter().position(|component| {
        matches!(component, std::path::Component::Normal(name) if *name == "collection-files")
    }).or_else(|| components.iter().position(|component| {
        matches!(component, std::path::Component::Normal(name) if *name == "files")
    }));
    let Some(marker) = marker else {
        return Err(AppError::validation("收集材料绝对路径无法迁移"));
    };
    let mut rebased = data_root.to_path_buf();
    for component in &components[marker..] {
        let std::path::Component::Normal(part) = component else {
            return Err(AppError::validation("收集材料绝对路径无法迁移"));
        };
        rebased.push(part);
    }
    Ok(rebased)
}

fn classify_pending_document(path: &Path, error: &anyhow::Error) -> PendingDocumentCandidate {
    let mut proposed_role = "supporting";
    let mut detection_reason = parse_failure_category(error);
    if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("pdf"))
    {
        let text = std::fs::read(path)
            .ok()
            .and_then(|bytes| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    invoice_parse::pdf::extract_text(&bytes, path)
                }))
                .ok()
            })
            .and_then(Result::ok);
        if let Some(text) = text {
            if invoice_parse::pdf::is_unambiguous_ride_hailing_itinerary(&text)
                || text.contains("行程单")
            {
                proposed_role = "itinerary";
                detection_reason = "itinerary_detected";
            } else if text.contains("明细") || text.contains("清单") {
                proposed_role = "detail";
                detection_reason = "detail_detected";
            }
        }
    }
    PendingDocumentCandidate {
        source_path: path.to_path_buf(),
        proposed_role: proposed_role.to_string(),
        detection_reason: detection_reason.to_string(),
    }
}

/// 日志只记录稳定的错误类别。底层解析错误可能包含用户目录、原始文件名或
/// 发票字段，因此不得直接写入应用日志。
fn parse_failure_category(error: &anyhow::Error) -> &'static str {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("ocr") {
        "ocr_failed"
    } else if message.contains("ofd") || message.contains("zip") {
        "ofd_failed"
    } else if message.contains("pdf") {
        "pdf_failed"
    } else if message.contains("xml") {
        "xml_failed"
    } else {
        "parse_failed"
    }
}

/// 将同一发票的 PDF/OFD/XML/图片候选先合并成一个稳定费用事实，再保留其余
/// 原件作为副本。规范化结果把所有唯一发票放在数组前部，归组只读取这部分；
/// 后部副本沿用合并后的关键字段，使存储层只挂载文件、不再创建第二项费用。
fn canonicalize_parsed_invoices(invoices: Vec<ParsedInvoice>) -> CanonicalizedInvoices {
    let mut group_indexes = HashMap::<String, usize>::new();
    let mut groups = Vec::<Vec<ParsedInvoice>>::new();
    for (index, mut invoice) in invoices.into_iter().enumerate() {
        invoice.invoice_number = invoice.invoice_number.trim().to_string();
        let key = if invoice.invoice_number.is_empty() {
            format!("source:{index}")
        } else {
            format!(
                "{}|{}|{}",
                invoice.invoice_number,
                invoice.issue_date,
                invoice.total_amount.normalize()
            )
        };
        let group_index = *group_indexes.entry(key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[group_index].push(invoice);
    }

    let mut canonical = Vec::with_capacity(groups.len());
    let mut alternates = Vec::new();
    for candidates in groups {
        let best_data_index = candidates
            .iter()
            .enumerate()
            .max_by_key(|(_, candidate)| candidate_quality_score(candidate))
            .map(|(index, _)| index)
            .unwrap_or_default();
        let preferred_document_index = candidates
            .iter()
            .enumerate()
            .min_by_key(|(_, candidate)| {
                (
                    preview_document_rank(&candidate.source_path),
                    std::cmp::Reverse(candidate_quality_score(candidate)),
                )
            })
            .map(|(index, _)| index)
            .unwrap_or_default();
        let mut merged = candidates[best_data_index].clone();
        let mut ordered = (0..candidates.len()).collect::<Vec<_>>();
        ordered.sort_by_key(|index| {
            (
                data_document_rank(&candidates[*index].source_path),
                std::cmp::Reverse(candidate_quality_score(&candidates[*index])),
            )
        });
        for index in ordered {
            let candidate = &candidates[index];
            merge_optional(&mut merged.tax_amount, &candidate.tax_amount);
            merge_optional(&mut merged.tax_rate, &candidate.tax_rate);
            merge_optional_string(&mut merged.buyer_name, &candidate.buyer_name);
            merge_optional_string(&mut merged.seller_name, &candidate.seller_name);
            merge_optional_string(&mut merged.city, &candidate.city);
            merge_optional_string(&mut merged.travel_route, &candidate.travel_route);
            merge_optional(&mut merged.departure_time, &candidate.departure_time);
            merge_optional(&mut merged.checkin_date, &candidate.checkin_date);
            if merged.ticket_type == ParseTicketType::Other
                && candidate.ticket_type != ParseTicketType::Other
            {
                merged.ticket_type = candidate.ticket_type;
            } else if merged.ticket_type != ParseTicketType::Other
                && candidate.ticket_type != ParseTicketType::Other
                && merged.ticket_type != candidate.ticket_type
            {
                merged.parse_level = ParseLevel::L4;
                merged.confidence = merged.confidence.min(candidate.confidence);
            }
        }
        merged.source_path = candidates[preferred_document_index].source_path.clone();
        canonical.push(merged.clone());
        for (index, candidate) in candidates.into_iter().enumerate() {
            if index == preferred_document_index {
                continue;
            }
            let mut alternate = merged.clone();
            alternate.source_path = candidate.source_path;
            alternates.push(alternate);
        }
    }
    let canonical_count = canonical.len();
    canonical.extend(alternates);
    CanonicalizedInvoices {
        invoices: canonical,
        canonical_count,
    }
}

fn merge_optional<T: Clone>(target: &mut Option<T>, candidate: &Option<T>) {
    if target.is_none() {
        *target = candidate.clone();
    }
}

fn merge_optional_string(target: &mut Option<String>, candidate: &Option<String>) {
    let target_missing = target
        .as_deref()
        .map_or(true, |value| value.trim().is_empty());
    if target_missing {
        *target = candidate
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
}

fn candidate_quality_score(candidate: &ParsedInvoice) -> i32 {
    let mut score = match candidate.parse_level {
        ParseLevel::L0 => 24,
        ParseLevel::L1 => 16,
        ParseLevel::L2 => 8,
        ParseLevel::L4 => 0,
    };
    if candidate.ticket_type != ParseTicketType::Other {
        score += 64;
    }
    if candidate.departure_time.is_some() || candidate.checkin_date.is_some() {
        score += 32;
    }
    if candidate
        .travel_route
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        score += 16;
    }
    if candidate
        .city
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        score += 8;
    }
    if candidate
        .seller_name
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        score += 4;
    }
    if candidate
        .buyer_name
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        score += 2;
    }
    if candidate.tax_amount.is_some() {
        score += 1;
    }
    score + (candidate.confidence.clamp(0.0, 1.0) * 10.0).round() as i32
}

fn data_document_rank(path: &Path) -> u8 {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "xml" => 0,
        "ofd" => 1,
        "pdf" => 2,
        "png" | "jpg" | "jpeg" | "webp" | "bmp" => 3,
        _ => 4,
    }
}

fn preview_document_rank(path: &Path) -> u8 {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => 0,
        "png" | "jpg" | "jpeg" | "webp" | "bmp" => 1,
        "ofd" => 2,
        "xml" => 3,
        _ => 4,
    }
}

/// Stage 3: 去重检查
async fn dedupe_invoices(
    app: &AppHandle,
    pipeline_id: &str,
    state: &tauri::State<'_, Mutex<AppState>>,
    canonicalized: CanonicalizedInvoices,
    cancellation: &CancellationToken,
) -> AppResult<DedupeResult> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    let CanonicalizedInvoices {
        invoices,
        canonical_count,
    } = canonicalized;

    let mut checked = DedupeResult {
        invoices,
        canonical_count,
        canonicalization_version: CANONICALIZATION_VERSION,
        duplicate_reasons: HashMap::new(),
        duplicate_reasons_by_index: Vec::new(),
    };
    let mut first_seen_invoice_numbers = HashMap::<String, usize>::new();

    for (idx, invoice) in checked.invoices[..canonical_count].iter().enumerate() {
        ensure_not_cancelled(cancellation)?;
        emit_progress(
            app,
            pipeline_id,
            "dedupe",
            idx as f32 / canonical_count as f32,
            idx,
            Some(canonical_count),
            &format!("检查 {}/{}", idx + 1, canonical_count),
        );

        // 多字段查重：发票号精确命中，或（金额 + 日期 + 票种）模糊命中。
        // 票种必须用发票自身的值，硬编码会让模糊匹配比对到错误票种。
        let ticket_type = to_store_ticket_type(invoice.ticket_type).to_str();
        let duplicates = db
            .find_potential_duplicates(
                &invoice.invoice_number,
                &invoice.total_amount,
                &invoice.issue_date,
                ticket_type,
                None,
            )
            .map_err(|e| AppError::database(format!("查重失败: {}", e)))?;

        let confirmed = duplicates
            .iter()
            .any(|d| d.invoice_number == invoice.invoice_number);

        let historical_reason = if confirmed {
            Some(format!(
                "发票号与历史台账一致（命中 {} 条）",
                duplicates.len()
            ))
        } else if !duplicates.is_empty() {
            Some(format!(
                "金额、日期和票种与历史台账一致（命中 {} 条）",
                duplicates.len()
            ))
        } else {
            None
        };
        let in_batch_reason = in_batch_duplicate_reason(
            &mut first_seen_invoice_numbers,
            &invoice.invoice_number,
            idx,
        );
        let reason = historical_reason.or(in_batch_reason.clone());
        if reason.is_some() {
            tracing::warn!(
                historical_suspects = duplicates.len(),
                in_batch = in_batch_reason.is_some(),
                "发现疑似重复，保留待人工确认"
            );
        }
        checked.duplicate_reasons_by_index.push(reason);
    }
    checked
        .duplicate_reasons_by_index
        .resize(checked.invoices.len(), None);

    Ok(checked)
}

/// 返回同批次精确发票号重复原因。首条记录保留为正常项，后续记录只标记、不删除。
/// 发票号只用于内存查重，不进入日志。
fn in_batch_duplicate_reason(
    first_seen: &mut HashMap<String, usize>,
    invoice_number: &str,
    index: usize,
) -> Option<String> {
    let normalized = invoice_number.trim();
    if normalized.is_empty() {
        return None;
    }
    if let Some(first_index) = first_seen.get(normalized) {
        return Some(format!(
            "同一批次内发票号一致（首次出现于第 {} 张）",
            first_index + 1
        ));
    }
    first_seen.insert(normalized.to_string(), index);
    None
}

/// Stage 4: 归组行程
async fn group_invoices_stage(
    app: &AppHandle,
    pipeline_id: &str,
    invoices: &[ParsedInvoice],
    home_city: &str,
    home_station_aliases: &[StationCityAlias],
    cancellation: &CancellationToken,
) -> AppResult<GroupingResult> {
    ensure_not_cancelled(cancellation)?;
    emit_progress(app, pipeline_id, "group", 0.5, 0, None, "执行归组算法...");

    // 创建简单的 no-op 解决器
    struct SimpleResolver;
    impl AmbiguityResolver for SimpleResolver {
        fn resolve(
            &self,
            _ambiguities: &[Ambiguity],
        ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
            Ok(Vec::new())
        }
    }

    let config = GroupingConfig {
        home_cities: vec![home_city.to_string()],
        home_station_aliases: Some(home_station_aliases.to_vec()),
        ambiguity_handler: Box::new(SimpleResolver),
    };

    let result = group_invoices(invoices, &config)
        .map_err(|e| AppError::internal(format!("归组失败: {}", e)))?;
    ensure_not_cancelled(cancellation)?;

    emit_progress(
        app,
        pipeline_id,
        "group",
        1.0,
        0,
        None,
        &format!("归组完成，识别 {} 个行程", result.trips.len()),
    );

    Ok(result)
}

/// Stage 6: 保存批次和发票到数据库
#[allow(clippy::too_many_arguments)]
async fn store_batch(
    context: &PipelineTaskContext<'_>,
    state: &tauri::State<'_, Mutex<AppState>>,
    config: &PipelineConfig,
    checked: &DedupeResult,
    grouped: &GroupingResult,
    home_city: &str,
    pending_documents: &[PendingDocumentCandidate],
    email_messages: &[NewEmailImportMessage],
) -> AppResult<i64> {
    let app = context.app;
    let pipeline_id = context.pipeline_id;
    let cancellation = context.cancellation;
    ensure_not_cancelled(cancellation)?;
    let data_root = crate::paths::data_root().map_err(AppError::from)?;
    let stored_paths =
        crate::pipeline_checkpoint::prepare_originals(&data_root, pipeline_id, &checked.invoices)?;
    let pending_paths = crate::pipeline_checkpoint::prepare_pending_documents(
        &data_root,
        pipeline_id,
        &pending_documents
            .iter()
            .map(|document| document.source_path.clone())
            .collect::<Vec<_>>(),
    )?;
    let mut verification_by_invoice = HashMap::<String, String>::new();
    for (invoice, stored_path) in checked.invoices.iter().zip(stored_paths.iter()) {
        let Some(status) = verification_for_path(stored_path)? else {
            continue;
        };
        let key = canonical_identity_key(invoice);
        let replace = verification_by_invoice.get(&key).map_or(true, |existing| {
            verification_rank(&status) > verification_rank(existing)
        });
        if replace {
            verification_by_invoice.insert(key, status);
        }
    }
    let mut reported_invoices = Vec::with_capacity(checked.invoices.len());
    for (index, (invoice, stored_path)) in
        checked.invoices.iter().zip(stored_paths.iter()).enumerate()
    {
        ensure_not_cancelled(cancellation)?;
        emit_progress(
            app,
            pipeline_id,
            "review",
            (index + 1) as f32 / checked.invoices.len() as f32,
            index,
            Some(checked.invoices.len()),
            &format!("校验原件 {}/{}", index + 1, checked.invoices.len()),
        );
        let duplicate_reason = checked
            .duplicate_reasons_by_index
            .get(index)
            .and_then(|reason| reason.clone())
            .or_else(|| {
                // 兼容旧版 deduped.json 检查点。
                checked
                    .duplicate_reasons
                    .get(&invoice.invoice_number)
                    .cloned()
            });
        reported_invoices.push(ReportedInvoice {
            id: 0, // 自动生成
            batch_id: 0,
            invoice_number: invoice.invoice_number.clone(),
            issue_date: invoice.issue_date,
            amount: invoice.total_amount,
            tax_amount: invoice.tax_amount,
            buyer_name: invoice.buyer_name.clone(),
            seller_name: invoice.seller_name.clone(),
            ticket_type: to_store_ticket_type(invoice.ticket_type),
            city: invoice.city.clone(),
            departure_time: invoice.departure_time,
            checkin_date: invoice.checkin_date,
            file_path: stored_path.to_string_lossy().into_owned(),
            created_at: chrono::Utc::now().naive_utc(),
            updated_at: chrono::Utc::now().naive_utc(),
            verification_result: verification_by_invoice
                .get(&canonical_identity_key(invoice))
                .cloned(),
            is_duplicate: duplicate_reason.is_some(),
            duplicate_reason,
        });
    }

    let groups = grouped
        .trips
        .iter()
        .enumerate()
        .map(|(group_index, trip)| {
            let (kind, title, mut requires_review) = grouping_kind_metadata(&trip.kind);
            let active_transport_input_indexes = trip
                .invoice_ids
                .iter()
                .copied()
                .filter(|input_index| {
                    let invoice = &checked.invoices[*input_index];
                    matches!(
                        invoice.ticket_type,
                        ParseTicketType::Rail | ParseTicketType::Flight
                    ) && invoice.transport_document_kind.is_route_anchor()
                })
                .collect::<Vec<_>>();
            let transport_adjustment_input_indexes = trip
                .invoice_ids
                .iter()
                .copied()
                .filter(|input_index| {
                    matches!(
                        checked.invoices[*input_index].transport_document_kind,
                        TransportDocumentKind::Refund | TransportDocumentKind::Change
                    )
                })
                .collect::<Vec<_>>();
            let transport_evidence_status = if kind == "business_trip" {
                if active_transport_input_indexes.is_empty() {
                    requires_review = true;
                    "missing"
                } else {
                    "present"
                }
            } else {
                "not_applicable"
            };
            let parse_review_reasons = trip
                .invoice_ids
                .iter()
                .filter_map(|input_index| {
                    let invoice = checked.invoices.get(*input_index)?;
                    parse_review_reason(invoice.parse_level, invoice.confidence).map(|reason| {
                        serde_json::json!({
                            "inputIndex": input_index,
                            "parseLevel": parse_level_to_string(invoice.parse_level),
                            "confidence": invoice.confidence,
                            "reason": reason,
                        })
                    })
                })
                .collect::<Vec<_>>();
            if !parse_review_reasons.is_empty() {
                requires_review = true;
            }
            let members = trip
                .invoice_ids
                .iter()
                .map(|input_index| {
                    let invoice = checked.invoices.get(*input_index).ok_or_else(|| {
                        AppError::internal(format!("归组发票索引越界: {input_index}"))
                    })?;
                    Ok(IndexedInvoiceGroupMember {
                        input_index: *input_index,
                        match_reason: pipeline_group_member_reason(invoice),
                    })
                })
                .collect::<AppResult<Vec<_>>>()?;
            let evidence_json = serde_json::json!({
                "tripKind": trip.kind,
                "homeCity": home_city,
                "inputIndexes": trip.invoice_ids,
                "parseReviewReasons": parse_review_reasons,
                "transportEvidenceStatus": transport_evidence_status,
                "activeTransportInputIndexes": active_transport_input_indexes,
                "transportAdjustmentInputIndexes": transport_adjustment_input_indexes,
            })
            .to_string();
            Ok(IndexedInvoiceGroup {
                group_index,
                kind,
                title,
                start_date: trip.start_date.to_string(),
                end_date: trip.end_date.to_string(),
                confidence: trip.confidence,
                requires_review,
                evidence_json,
                members,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;

    let grouping = IndexedBatchGrouping {
        rule_version: GROUPING_RULE_VERSION.to_string(),
        home_cities_json: serde_json::to_string(&vec![home_city])
            .map_err(|error| AppError::internal(format!("序列化常驻城市失败: {error}")))?,
        overall_confidence: grouped.overall_confidence,
        ambiguities_json: serde_json::to_string(&grouped.ambiguities)
            .map_err(|error| AppError::internal(format!("序列化归组歧义失败: {error}")))?,
        groups,
    };
    let canonical_invoices = &checked.invoices[..checked.canonical_count];
    let pending_records = pending_documents
        .iter()
        .zip(pending_paths.iter())
        .map(|(candidate, stored_path)| {
            let bytes = std::fs::read(stored_path)
                .map_err(|error| AppError::io(format!("校验待挂载材料失败（{}）", error.kind())))?;
            Ok(NewPendingInvoiceDocument {
                proposed_role: candidate.proposed_role.clone(),
                file_path: stored_path.to_string_lossy().into_owned(),
                original_name: staged_original_name(&candidate.source_path),
                mime_type: mime_type_for_pipeline_path(stored_path).map(str::to_string),
                sha256: Some(dedupe::sha256_hex(&bytes)),
                detection_reason: candidate.detection_reason.clone(),
                auto_assign_invoice_index: automatic_supporting_match(
                    candidate,
                    canonical_invoices,
                ),
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    ensure_not_cancelled(cancellation)?;
    let app_state = state.lock().unwrap();
    app_state
        .ledger_db()?
        .store_pipeline_batch_atomic_with_email_ledger(
            pipeline_id,
            &config.batch_name,
            &config.month,
            config.target_batch_id,
            &reported_invoices,
            &grouping,
            &pending_records,
            email_messages,
        )
        .map_err(|error| AppError::database(format!("原子保存待审核批次失败: {error}")))
}

fn canonical_identity_key(invoice: &ParsedInvoice) -> String {
    format!(
        "{}|{}|{}",
        invoice.invoice_number.trim(),
        invoice.issue_date,
        invoice.total_amount.normalize()
    )
}

fn automatic_supporting_match(
    candidate: &PendingDocumentCandidate,
    invoices: &[ParsedInvoice],
) -> Option<usize> {
    let facts = supporting_facts_for_candidate(candidate)?;
    supporting_match_index(&facts, invoices)
}

fn supporting_facts_for_candidate(
    candidate: &PendingDocumentCandidate,
) -> Option<invoice_parse::pdf::SupportingDocumentFacts> {
    supporting_facts_for_path(&candidate.source_path)
}

fn supporting_facts_for_path(path: &Path) -> Option<invoice_parse::pdf::SupportingDocumentFacts> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())?
        .to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "pdf" | "png" | "jpg" | "jpeg" | "webp" | "bmp"
    ) {
        return None;
    }
    let text_facts = (extension == "pdf")
        .then(|| std::fs::read(path).ok())
        .flatten()
        .and_then(|bytes| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                invoice_parse::pdf::extract_text(&bytes, path)
            }))
            .ok()?
            .ok()
            .and_then(|text| invoice_parse::pdf::extract_supporting_document_facts(&text))
        });
    text_facts.or_else(|| {
        let asset_dir = crate::paths::ocr_assets_dir().ok()?;
        crate::ocr_worker::supporting_facts_with_worker(path, &asset_dir)
            .ok()
            .flatten()
    })
}

fn supporting_match_index(
    facts: &invoice_parse::pdf::SupportingDocumentFacts,
    invoices: &[ParsedInvoice],
) -> Option<usize> {
    let mut matches = invoices
        .iter()
        .enumerate()
        .filter(|(_, invoice)| invoice.total_amount == facts.total_amount)
        .filter(|(_, invoice)| {
            let seller = invoice.seller_name.as_deref().unwrap_or_default();
            match (facts.kind.as_str(), facts.provider.as_str()) {
                ("ride_hailing_itinerary", "didi") => seller.contains("滴滴"),
                ("ride_hailing_itinerary", "caocao") => {
                    invoice.ticket_type == ParseTicketType::CityTransport
                        || seller.contains("曹操")
                        || seller.contains("吉利优行")
                }
                ("courier_detail", "courier") => {
                    invoice.ticket_type == ParseTicketType::CourierLogistics
                }
                ("hotel_folio", "hotel") => invoice.ticket_type == ParseTicketType::Hotel,
                _ => false,
            }
        })
        .map(|(index, _)| index);
    let matched = matches.next()?;
    matches.next().is_none().then_some(matched)
}

fn enrich_invoices_from_supporting_documents(
    invoices: &mut [ParsedInvoice],
    candidates: &[PendingDocumentCandidate],
) {
    for candidate in candidates {
        let Some(facts) = supporting_facts_for_candidate(candidate) else {
            continue;
        };
        let Some(index) = supporting_match_index(&facts, invoices) else {
            continue;
        };
        match facts.kind.as_str() {
            "ride_hailing_itinerary" => {
                let invoice = &mut invoices[index];
                invoice.ticket_type = ParseTicketType::CityTransport;
                invoice.city = facts.cities.first().cloned().or(invoice.city.clone());
                if let Some(start) = facts.start_date {
                    invoice.departure_time = start.and_hms_opt(0, 0, 0);
                }
                for (hotel_index, hotel) in invoices.iter_mut().enumerate() {
                    if hotel_index == index || hotel.ticket_type != ParseTicketType::Hotel {
                        continue;
                    }
                    let seller = hotel.seller_name.as_deref().unwrap_or_default();
                    if !facts
                        .hotel_mentions
                        .iter()
                        .any(|mention| seller.contains(mention))
                    {
                        continue;
                    }
                    hotel.city = facts.cities.first().cloned().or(hotel.city.clone());
                    hotel.checkin_date = facts.start_date.or(hotel.checkin_date);
                }
            }
            "hotel_folio" => {
                let invoice = &mut invoices[index];
                invoice.ticket_type = ParseTicketType::Hotel;
                invoice.checkin_date = facts.start_date;
                invoice.city = facts.cities.first().cloned().or(invoice.city.clone());
            }
            _ => {}
        }
    }
}

fn verification_rank(value: &str) -> u8 {
    match value {
        "invalid" => 5,
        "valid" => 4,
        "unsupported" => 3,
        "not_signed" => 2,
        "not_applicable" => 1,
        _ => 0,
    }
}

fn staged_original_name(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("document");
    let stripped = name
        .get(65..)
        .filter(|_| name.as_bytes().get(64) == Some(&b'-'))
        .unwrap_or(name);
    stripped.chars().take(255).collect()
}

fn ledger_text(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || *character == '\t')
        .take(max_chars)
        .collect()
}

fn manual_email_supplement_message(
    message_id: i64,
    files: &[PathBuf],
) -> AppResult<NewEmailImportMessage> {
    let attachments = files
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path).map_err(|error| {
                AppError::io(format!("读取邮件补充导入文件失败（{}）", error.kind()))
            })?;
            Ok(NewEmailImportAttachment {
                content_sha256: Some(dedupe::sha256_hex(&bytes)),
                original_name: staged_original_name(path),
                container_name: None,
                mime_type: mime_type_for_pipeline_path(path).map(str::to_string),
                byte_len: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                status: "failed".to_string(),
                role_hint: "unknown".to_string(),
                reason: "manual_candidate_pending_parse".to_string(),
                is_content_duplicate: false,
                invoice_input_index: None,
                pending_document_index: None,
                manual_import: true,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(NewEmailImportMessage {
        existing_message_id: Some(message_id),
        mailbox_folder: String::new(),
        uid: 0,
        message_id_sha256: None,
        sender: String::new(),
        subject: String::new(),
        received_at: None,
        initial_status: "needs_attachment_review".to_string(),
        error_category: None,
        attachments,
    })
}

fn bind_email_ledger_associations(
    messages: &mut [NewEmailImportMessage],
    invoices: &[ParsedInvoice],
    pending_documents: &[PendingDocumentCandidate],
) -> AppResult<()> {
    let mut invoice_by_hash = HashMap::new();
    for (index, invoice) in invoices.iter().enumerate() {
        let bytes = std::fs::read(&invoice.source_path)
            .map_err(|error| AppError::io(format!("关联邮件发票原件失败（{}）", error.kind())))?;
        invoice_by_hash
            .entry(dedupe::sha256_hex(&bytes))
            .or_insert(index);
    }
    let mut pending_by_hash = HashMap::new();
    for (index, pending) in pending_documents.iter().enumerate() {
        let bytes = std::fs::read(&pending.source_path)
            .map_err(|error| AppError::io(format!("关联邮件配套材料失败（{}）", error.kind())))?;
        pending_by_hash
            .entry(dedupe::sha256_hex(&bytes))
            .or_insert(index);
    }
    for attachment in messages
        .iter_mut()
        .flat_map(|message| message.attachments.iter_mut())
    {
        let Some(content_hash) = attachment.content_sha256.as_ref() else {
            continue;
        };
        if let Some(index) = invoice_by_hash.get(content_hash) {
            attachment.invoice_input_index = Some(*index);
            attachment.pending_document_index = None;
        } else if let Some(index) = pending_by_hash.get(content_hash) {
            attachment.pending_document_index = Some(*index);
            attachment.invoice_input_index = None;
        }
    }
    Ok(())
}

fn mime_type_for_pipeline_path(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => Some("application/pdf"),
        "ofd" => Some("application/ofd"),
        "xml" => Some("application/xml"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn grouping_kind_metadata(kind: &TripKind) -> (String, String, bool) {
    match kind {
        TripKind::BusinessTrip { cities, .. } => (
            "business_trip".to_string(),
            format!(
                "{}出差",
                if cities.is_empty() {
                    "目的地待确认".to_string()
                } else {
                    cities.join("、")
                }
            ),
            false,
        ),
        TripKind::LocalMonth { month, .. } => (
            "local_month".to_string(),
            format!("{month} 月市内消费"),
            false,
        ),
        TripKind::Excluded => ("excluded".to_string(), "已排除票据".to_string(), true),
        TripKind::NeedsReview { reason } => (
            "needs_review".to_string(),
            format!("待人工复核：{reason}"),
            true,
        ),
    }
}

fn pipeline_group_member_reason(invoice: &ParsedInvoice) -> String {
    let nature = match invoice.transport_document_kind {
        TransportDocumentKind::Refund => "；交通票性质：退票费，不作为路线节点",
        TransportDocumentKind::Change => "；交通票性质：改签费，不作为路线节点",
        TransportDocumentKind::Sale => "；交通票性质：有效售票",
        TransportDocumentKind::Unknown => "",
    };
    format!(
        "{GROUPING_RULE_VERSION}：票据日期 {}、类型 {:?}、城市 {} 与该组规则匹配；解析级别 {}，置信度 {:.0}%{}{}",
        invoice.issue_date,
        invoice.ticket_type,
        invoice.city.as_deref().unwrap_or("未识别"),
        parse_level_to_string(invoice.parse_level),
        invoice.confidence * 100.0,
        nature,
        parse_review_reason(invoice.parse_level, invoice.confidence)
            .map(|reason| format!("，需人工复核：{reason}"))
            .unwrap_or_default()
    )
}

fn parse_review_reason(level: ParseLevel, confidence: f32) -> Option<&'static str> {
    match level {
        ParseLevel::L2 => Some("OCR 识别结果需逐项核对"),
        ParseLevel::L4 => Some("关键字段冲突"),
        ParseLevel::L0 | ParseLevel::L1 if confidence < 0.90 => Some("解析置信度低于 90%"),
        ParseLevel::L0 | ParseLevel::L1 => None,
    }
}
// ==================== 辅助函数 ====================

/// 获取任务临时目录；与程序目录分离。
fn get_temp_dir() -> AppResult<PathBuf> {
    let temp_dir = crate::paths::temp_dir().map_err(AppError::from)?;
    Ok(temp_dir)
}

/// 清理文件名（移除非法字符）
const MAX_EMAIL_STAGING_COMPONENT_CHARS: usize = 80;

/// 邮箱暂存名必须保留扩展名，同时给仍依赖 Win32 `MAX_PATH` 的 PDF/OCR
/// 依赖留出空间。完整内容哈希保证唯一性，短 stem 只用于人工诊断。
fn email_staging_filename(content_hash: &str, name: &str) -> String {
    let path = Path::new(name);
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| {
            value
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(10)
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|value| !value.is_empty());
    let suffix = extension
        .as_deref()
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let prefix = format!("{content_hash}-");
    let stem_budget = MAX_EMAIL_STAGING_COMPONENT_CHARS
        .saturating_sub(prefix.len() + suffix.len())
        .max(1);
    let raw_stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("invoice");
    let mut stem = raw_stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(stem_budget)
        .collect::<String>();
    if stem.is_empty() {
        stem.push('i');
    }
    format!("{prefix}{stem}{suffix}")
}

/// 解析单个发票文件
fn verification_for_path(path: &Path) -> AppResult<Option<String>> {
    let declared_extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if matches!(
        declared_extension.as_str(),
        "pdf" | "png" | "jpg" | "jpeg" | "webp" | "bmp"
    ) {
        return Ok(Some("not_applicable".to_string()));
    }
    if !matches!(declared_extension.as_str(), "xml" | "ofd") && !path.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)
        .map_err(|error| AppError::io(format!("验签读取失败（{}）", error.kind())))?;
    let extension = detected_input_format(path, &bytes).to_string();
    if !matches!(
        extension.as_str(),
        "xml" | "ofd" | "pdf" | "png" | "jpg" | "jpeg" | "webp" | "bmp"
    ) {
        return Ok(None);
    }
    if matches!(
        extension.as_str(),
        "pdf" | "png" | "jpg" | "jpeg" | "webp" | "bmp"
    ) {
        return Ok(Some("not_applicable".to_string()));
    }
    let status = match extension.as_str() {
        "xml" => invoice_parse::verify::verify_xml_signature(&bytes, path),
        "ofd" => invoice_parse::verify::verify_ofd_signature(&bytes, path),
        _ => unreachable!(),
    };
    Ok(Some(match status {
        Ok(invoice_parse::verify::SignatureStatus::Valid) => "valid".to_string(),
        Ok(invoice_parse::verify::SignatureStatus::NotSigned) => "not_signed".to_string(),
        Ok(invoice_parse::verify::SignatureStatus::Invalid { .. }) => "invalid".to_string(),
        Ok(invoice_parse::verify::SignatureStatus::Unsupported { .. }) | Err(_) => {
            "unsupported".to_string()
        }
    }))
}

fn parse_single_invoice(path: &Path, hints: &TagHints) -> Result<ParsedInvoice, anyhow::Error> {
    let bytes = std::fs::read(path)?;
    let ext = detected_input_format(path, &bytes).to_string();

    if matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "bmp"
    ) {
        let asset_dir = crate::paths::ocr_assets_dir()
            .map_err(|_| anyhow::anyhow!("离线 OCR 组件路径不可用"))?;
        return crate::ocr_worker::parse_with_worker(path, &asset_dir, ParseTicketType::Other)
            .map_err(|error| anyhow::anyhow!(error.to_string()));
    }

    let path_buf = path.to_path_buf();

    if ext.eq_ignore_ascii_case("pdf") {
        return parse_pdf_with_fallbacks(path, &bytes, hints, ParseTicketType::Other)
            .map_err(|error| anyhow::anyhow!(error.message().to_string()));
    }

    // 使用 catch_unwind 包装，防止解析库 panic
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let ext_ref = ext.as_str();
        match ext_ref.to_lowercase().as_str() {
            "xml" => {
                invoice_parse::xml::parse_invoice_xml(&bytes, path, hints, ParseTicketType::Other)
            }
            "ofd" => {
                invoice_parse::ofd::parse_invoice_ofd(&bytes, path, hints, ParseTicketType::Other)
            }
            "pdf" => unreachable!("PDF 已在独立逐级隔离路径处理"),
            _ => {
                // 不支持的格式，返回错误
                Err(invoice_parse::model::ParseError::MalformedFormat {
                    format: "unknown",
                    path: path_buf.clone(),
                    detail: format!("不支持的文件格式: {}", ext_ref),
                })
            }
        }
    }));

    match result {
        Ok(Ok(invoice)) => Ok(invoice),
        Ok(Err(e)) => Err(anyhow::anyhow!("解析失败: {}", e)),
        Err(_) => Err(anyhow::anyhow!("解析库 panic")),
    }
}

/// 解析分派以文件内容为准，扩展名只在魔数不足以判断时作为回退。
/// 邮件供应商经常给出超长或错误文件名，不能让命名问题变成漏票。
fn detected_input_format<'a>(path: &'a Path, bytes: &'a [u8]) -> &'a str {
    if bytes.starts_with(b"%PDF-") {
        return "pdf";
    }
    if bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08")
    {
        return "ofd";
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "png";
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return "jpg";
    }
    if bytes.starts_with(b"BM") {
        return "bmp";
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return "webp";
    }
    let prefix = std::str::from_utf8(&bytes[..bytes.len().min(256)])
        .unwrap_or_default()
        .trim_start_matches('\u{feff}')
        .trim_start();
    if prefix.starts_with("<?xml") || prefix.starts_with("<Invoice") {
        return "xml";
    }
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
}

// ==================== 事件发送 ====================

fn emit_progress(
    app: &AppHandle,
    pipeline_id: &str,
    stage: &str,
    progress: f32,
    current: usize,
    total: Option<usize>,
    message: &str,
) {
    let event = StageProgress {
        stage: stage.to_string(),
        progress,
        current: Some(current),
        total,
        message: message.to_string(),
    };

    let event_name = format!("pipeline:progress:{}", pipeline_id);
    if let Err(e) = app.emit(&event_name, event) {
        tracing::error!("发送进度事件失败: {}", e);
    }
}

fn emit_error(app: &AppHandle, pipeline_id: &str, stage: &str, message: &str) {
    let event = PipelineError {
        stage: stage.to_string(),
        message: message.to_string(),
    };

    let event_name = format!("pipeline:error:{}", pipeline_id);
    if let Err(e) = app.emit(&event_name, event) {
        tracing::error!("发送错误事件失败: {}", e);
    }
}

fn emit_cancelled(app: &AppHandle, pipeline_id: &str, stage: &str) {
    let event = PipelineCancelled {
        stage: stage.to_string(),
        message: CANCELLATION_MESSAGE.to_string(),
    };
    let event_name = format!("pipeline:cancelled:{}", pipeline_id);
    if let Err(error) = app.emit(&event_name, event) {
        tracing::error!("发送安全停止事件失败: {error}");
    }
}

fn emit_complete(app: &AppHandle, pipeline_id: &str, result: PipelineComplete) {
    let event_name = format!("pipeline:complete:{}", pipeline_id);
    if let Err(e) = app.emit(&event_name, result) {
        tracing::error!("发送完成事件失败: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashSet};
    use std::str::FromStr;
    use std::time::Instant;

    #[derive(serde::Serialize)]
    struct PrivateParseOutcome {
        sample: String,
        extension: String,
        byte_len: u64,
        elapsed_ms: u128,
        parsed: Option<ParsedInvoice>,
        error_category: Option<&'static str>,
        error_private: Option<String>,
    }

    fn synthetic_parsed(path: &str, ticket_type: ParseTicketType) -> ParsedInvoice {
        ParsedInvoice {
            invoice_number: "26112000000000000001".to_string(),
            issue_date: chrono::NaiveDate::from_ymd_opt(2026, 6, 18).unwrap(),
            total_amount: rust_decimal::Decimal::from_str("1200.00").unwrap(),
            tax_amount: None,
            tax_rate: None,
            buyer_name: None,
            seller_name: None,
            ticket_type,
            transport_document_kind: Default::default(),
            parse_level: ParseLevel::L1,
            confidence: 0.98,
            city: None,
            travel_route: None,
            departure_time: None,
            checkin_date: None,
            source_path: PathBuf::from(path),
        }
    }

    #[test]
    fn canonicalization_merges_complementary_formats_before_grouping() {
        let pdf = synthetic_parsed("invoice.pdf", ParseTicketType::Other);
        let mut ofd = synthetic_parsed("invoice.ofd", ParseTicketType::Other);
        ofd.parse_level = ParseLevel::L0;
        ofd.seller_name = Some("synthetic seller".to_string());
        ofd.buyer_name = Some("synthetic buyer".to_string());
        ofd.tax_amount = Some(rust_decimal::Decimal::from_str("100.00").unwrap());
        let mut xml = synthetic_parsed("invoice.xml", ParseTicketType::Rail);
        xml.parse_level = ParseLevel::L0;
        xml.city = Some("北京".to_string());
        xml.travel_route = Some("北京南→上海虹桥".to_string());
        xml.departure_time = Some(
            chrono::NaiveDate::from_ymd_opt(2026, 6, 18)
                .unwrap()
                .and_hms_opt(8, 30, 0)
                .unwrap(),
        );

        let normalized = canonicalize_parsed_invoices(vec![ofd, pdf, xml]);

        assert_eq!(normalized.canonical_count, 1);
        assert_eq!(normalized.invoices.len(), 3);
        let canonical = &normalized.invoices[0];
        assert_eq!(canonical.source_path, PathBuf::from("invoice.pdf"));
        assert_eq!(canonical.ticket_type, ParseTicketType::Rail);
        assert_eq!(canonical.city.as_deref(), Some("北京"));
        assert_eq!(canonical.seller_name.as_deref(), Some("synthetic seller"));
        assert_eq!(canonical.tax_amount.unwrap().to_string(), "100.00");
        assert!(normalized.invoices[1..].iter().all(|alternate| {
            alternate.invoice_number == canonical.invoice_number
                && alternate.issue_date == canonical.issue_date
                && alternate.total_amount == canonical.total_amount
                && alternate.ticket_type == ParseTicketType::Rail
        }));
    }

    #[test]
    fn courier_detail_matches_unique_courier_expense_by_exact_amount() {
        let mut courier = synthetic_parsed("courier.pdf", ParseTicketType::CourierLogistics);
        courier.total_amount = rust_decimal::Decimal::from_str("68.82").unwrap();
        courier.seller_name = Some("深圳市顺丰同城物流有限公司北京分公司".to_string());
        let facts = invoice_parse::pdf::SupportingDocumentFacts {
            kind: "courier_detail".to_string(),
            provider: "courier".to_string(),
            total_amount: rust_decimal::Decimal::from_str("68.82").unwrap(),
            start_date: None,
            end_date: None,
            cities: Vec::new(),
            hotel_mentions: Vec::new(),
        };

        assert_eq!(supporting_match_index(&facts, &[courier]), Some(0));
    }

    fn valid_local_config() -> PipelineConfig {
        PipelineConfig {
            batch_name: "2026 年 6 月发票".to_string(),
            month: "2026-06".to_string(),
            target_batch_id: None,
            source: PipelineSource::Local {
                paths: vec!["C:/samples/invoice.xml".to_string()],
                target_email_message_id: None,
            },
            date_range: DateRangeDto {
                start: "2026-06-01".to_string(),
                end: "2026-06-30".to_string(),
            },
        }
    }

    #[test]
    fn ocr_and_low_confidence_results_require_review() {
        assert!(parse_review_reason(ParseLevel::L2, 0.99).is_some());
        assert!(parse_review_reason(ParseLevel::L4, 1.0).is_some());
        assert!(parse_review_reason(ParseLevel::L1, 0.89).is_some());
        assert!(parse_review_reason(ParseLevel::L1, 0.90).is_none());
        assert!(parse_review_reason(ParseLevel::L0, 1.0).is_none());
    }

    #[test]
    fn same_batch_exact_invoice_number_marks_only_later_occurrences() {
        let mut first_seen = HashMap::new();
        assert!(in_batch_duplicate_reason(&mut first_seen, "26112000000000000001", 0).is_none());
        let second = in_batch_duplicate_reason(&mut first_seen, " 26112000000000000001 ", 1)
            .expect("later exact number must be marked");
        assert!(second.contains("第 1 张"));
        assert!(!second.contains("26112000000000000001"));
        assert!(in_batch_duplicate_reason(&mut first_seen, "26112000000000000002", 2).is_none());
        assert!(in_batch_duplicate_reason(&mut first_seen, "  ", 3).is_none());
    }

    #[test]
    fn old_dedupe_checkpoint_without_index_reasons_remains_readable() {
        let json = r#"{
            "invoices": [],
            "duplicate_reasons": {"legacy-number": "历史台账命中"}
        }"#;
        let checkpoint: DedupeResult = serde_json::from_str(json).unwrap();
        assert!(checkpoint.duplicate_reasons_by_index.is_empty());
        assert_eq!(
            checkpoint
                .duplicate_reasons
                .get("legacy-number")
                .map(String::as_str),
            Some("历史台账命中")
        );
    }

    #[test]
    fn parse_failure_log_category_never_echoes_private_error_text() {
        let private_error =
            anyhow::anyhow!(r#"C:\Users\person\Invoices\private-name.ofd 不是有效的 ZIP 容器"#);
        let category = parse_failure_category(&private_error);
        assert_eq!(category, "ofd_failed");
        assert!(!category.contains("person"));
        assert!(!category.contains("private-name"));

        assert_eq!(
            parse_failure_category(&anyhow::anyhow!("离线 OCR 识别失败")),
            "ocr_failed"
        );
        assert_eq!(
            parse_failure_category(&anyhow::anyhow!("unclassified parser failure")),
            "parse_failed"
        );
    }

    #[test]
    fn email_staging_name_is_short_ascii_and_preserves_extension() {
        let content_hash = "a".repeat(64);
        let source = format!("{}发票原件名称末尾.PDF", "很长的中文名称".repeat(40));
        let staged = email_staging_filename(&content_hash, &source);

        assert!(staged.len() <= MAX_EMAIL_STAGING_COMPONENT_CHARS);
        assert!(staged.starts_with(&format!("{content_hash}-")));
        assert!(staged.ends_with(".pdf"));
        assert!(staged.is_ascii());
        assert!(!staged.ends_with(['.', ' ']));
    }

    #[test]
    fn email_staging_name_remains_unique_by_content_hash() {
        let first = email_staging_filename(&"a".repeat(64), "同名发票.pdf");
        let second = email_staging_filename(&"b".repeat(64), "同名发票.pdf");

        assert_ne!(first, second);
        assert_eq!(first.len(), second.len());
    }

    #[test]
    fn pipeline_preflight_accepts_valid_local_input() {
        assert!(validate_pipeline_config(&valid_local_config()).is_ok());
    }

    #[test]
    fn source_notices_are_optional_for_old_tasks_and_roundtrip_for_new_tasks() {
        let root = tempfile::tempdir().unwrap();
        let task = root.path().join("task");
        assert_eq!(read_source_notices(&task).unwrap(), SourceNotices::empty());

        let expected = SourceNotices {
            format_version: SOURCE_NOTICES_FORMAT_VERSION,
            link_only_email_count: 3,
        };
        crate::pipeline_checkpoint::write_json_checkpoint(&task, "source-notices", &expected)
            .unwrap();
        assert_eq!(read_source_notices(&task).unwrap(), expected);
    }

    #[test]
    fn link_only_empty_collection_requires_manual_safe_fallback() {
        let error = empty_collection_error(&SourceNotices {
            format_version: SOURCE_NOTICES_FORMAT_VERSION,
            link_only_email_count: 2,
        });
        let message = error.message();
        assert!(message.contains("2 封"));
        assert!(message.contains("不会自动打开或下载"));
        assert!(message.contains("核对发件人和域名"));
        assert!(message.contains("“本地文件”"));
        assert!(!message.contains("https://"));
    }

    #[test]
    fn pipeline_completion_serializes_link_only_count_without_private_data() {
        let json = serde_json::to_value(PipelineComplete {
            batch_id: 7,
            invoice_count: 1,
            total_amount: "100.00".to_string(),
            excel_path: None,
            link_only_email_count: 3,
            pending_document_count: 2,
            source_file_count: 8,
            parsed_document_count: 6,
            canonical_invoice_count: 4,
            duplicate_document_count: 2,
        })
        .unwrap();
        assert_eq!(json["link_only_email_count"], 3);
        assert_eq!(json["pending_document_count"], 2);
        assert_eq!(json["source_file_count"], 8);
        assert_eq!(json["canonical_invoice_count"], 4);
        assert_eq!(json.as_object().unwrap().len(), 10);
    }

    #[test]
    fn packaged_pipeline_collects_and_parses_the_synthetic_invoice_file() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri 应位于仓库根目录下");
        let invoice_file = repo_root
            .join("fixtures")
            .join("synthetic")
            .join("vat-invoice.xml");
        let staging = tempfile::tempdir().unwrap();

        let collected = crate::local_source::collect_local_inputs(&[invoice_file], staging.path())
            .expect("生产本地来源应读取合成发票文件");
        assert_eq!(collected.files.len(), 1);
        assert_eq!(collected.link_only_emails, 0);

        let parsed = parse_single_invoice(&collected.files[0], &builtin_hints())
            .expect("生产流水线 hints 应解析合成发票文件");
        assert_eq!(parsed.invoice_number, "26112000000000000001");
        assert_eq!(
            parsed.issue_date.format("%Y-%m-%d").to_string(),
            "2026-06-18"
        );
        assert_eq!(parsed.total_amount.to_string(), "1200.00");
    }

    #[test]
    fn pipeline_preflight_rejects_empty_local_source() {
        let mut config = valid_local_config();
        config.source = PipelineSource::Local {
            paths: vec![],
            target_email_message_id: None,
        };
        assert!(validate_pipeline_config(&config)
            .unwrap_err()
            .message()
            .contains("至少一个"));
    }

    #[test]
    fn collection_snapshot_only_reads_managed_material_files() {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("collection-files");
        std::fs::create_dir(&library).unwrap();
        let managed = library.join("invoice.pdf");
        let external = root.path().join("outside.pdf");
        std::fs::write(&managed, b"managed").unwrap();
        std::fs::write(&external, b"external").unwrap();

        let accepted = resolve_managed_material_paths(
            root.path(),
            &[PathBuf::from("collection-files").join("invoice.pdf")],
        )
        .unwrap();
        assert_eq!(accepted, vec![std::fs::canonicalize(managed).unwrap()]);
        assert!(resolve_managed_material_paths(root.path(), &[external])
            .unwrap_err()
            .message()
            .contains("越过受控材料库"));
        assert!(resolve_managed_material_paths(root.path(), &[]).is_err());
    }

    #[test]
    fn pipeline_preflight_rejects_reversed_dates() {
        let mut config = valid_local_config();
        config.date_range.start = "2026-07-01".to_string();
        assert!(validate_pipeline_config(&config)
            .unwrap_err()
            .message()
            .contains("不能早于"));
    }

    #[test]
    fn pipeline_preflight_rejects_invalid_month_and_long_name() {
        let mut config = valid_local_config();
        config.month = "2026-13".to_string();
        assert!(validate_pipeline_config(&config).is_err());

        config.month = "2026-06".to_string();
        config.batch_name = "批".repeat(101);
        assert!(validate_pipeline_config(&config).is_err());
    }

    #[test]
    fn pipeline_signature_status_is_preserved_for_review() {
        let temp = tempfile::tempdir().unwrap();
        let unsigned_xml = temp.path().join("unsigned.xml");
        std::fs::write(&unsigned_xml, b"<Invoice></Invoice>").unwrap();
        assert_eq!(
            verification_for_path(&unsigned_xml).unwrap().as_deref(),
            Some("not_signed")
        );

        let malformed_xml = temp.path().join("malformed.xml");
        std::fs::write(&malformed_xml, [0xff, 0xfe]).unwrap();
        assert_eq!(
            verification_for_path(&malformed_xml).unwrap().as_deref(),
            Some("unsupported")
        );
        assert_eq!(
            verification_for_path(Path::new("missing.pdf"))
                .unwrap()
                .as_deref(),
            Some("not_applicable")
        );
        assert!(verification_for_path(Path::new("unknown.txt"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn parser_dispatch_prefers_magic_over_missing_or_wrong_extension() {
        assert_eq!(
            detected_input_format(Path::new("long-name-without-extension"), b"%PDF-1.7\n"),
            "pdf"
        );
        assert_eq!(
            detected_input_format(Path::new("truncated.o"), b"PK\x03\x04synthetic"),
            "ofd"
        );
        assert_eq!(
            detected_input_format(
                Path::new("truncated.p"),
                b"\xef\xbb\xbf  <?xml version=\"1.0\"?><Invoice/>"
            ),
            "xml"
        );
        assert_eq!(
            detected_input_format(Path::new("wrong.xml"), b"%PDF-1.5\n"),
            "pdf"
        );
    }

    #[test]
    #[ignore = "requires an explicitly authorized private capture outside the Git repository"]
    fn real_private_capture_parses_candidates_without_logging_invoice_fields() {
        let capture_root = std::env::var_os("INVOICE_REAL_CAPTURE_ROOT")
            .map(PathBuf::from)
            .expect("INVOICE_REAL_CAPTURE_ROOT must be set for this ignored validation");
        let capture_root = std::fs::canonicalize(capture_root)
            .expect("private capture root must exist and be accessible");
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri must have a repository parent")
            .canonicalize()
            .expect("repository root must be accessible");
        assert!(
            !capture_root.starts_with(&repo_root),
            "real private validation data must stay outside the Git repository"
        );
        assert!(
            !std::fs::symlink_metadata(&capture_root)
                .expect("private capture root metadata must be readable")
                .file_type()
                .is_symlink(),
            "private capture root must not be a symbolic link"
        );

        let samples_root = capture_root.join("fixtures").join("samples");
        let extension_filter = std::env::var("INVOICE_REAL_PARSE_EXTENSION")
            .ok()
            .map(|value| value.to_ascii_lowercase());
        let mut files = std::fs::read_dir(&samples_root)
            .expect("private samples directory must be readable")
            .map(|entry| entry.expect("private sample entry must be readable").path())
            .filter(|path| path.is_file())
            .filter(|path| {
                matches!(
                    path.extension()
                        .and_then(|value| value.to_str())
                        .map(str::to_ascii_lowercase)
                        .as_deref(),
                    Some("xml" | "ofd" | "pdf" | "png" | "jpg" | "jpeg" | "webp" | "bmp")
                )
            })
            .filter(|path| {
                extension_filter.as_ref().map_or(true, |expected| {
                    path.extension()
                        .and_then(|value| value.to_str())
                        .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
                })
            })
            .collect::<Vec<_>>();
        files.sort();
        if let Ok(raw_limit) = std::env::var("INVOICE_REAL_PARSE_LIMIT") {
            let limit = raw_limit
                .parse::<usize>()
                .expect("INVOICE_REAL_PARSE_LIMIT must be a positive integer");
            assert!(limit > 0, "INVOICE_REAL_PARSE_LIMIT must be positive");
            files.truncate(limit);
        }
        assert!(
            !files.is_empty(),
            "private capture contains no supported candidate files"
        );

        let hints = builtin_hints();
        let mut outcomes = Vec::with_capacity(files.len());
        let mut levels = BTreeMap::<String, usize>::new();
        let mut formats = BTreeMap::<String, usize>::new();
        let mut success = 0usize;
        let mut failed = 0usize;
        let mut parsed_for_validation = Vec::new();
        for path in files {
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("unknown")
                .to_ascii_lowercase();
            *formats.entry(extension.clone()).or_default() += 1;
            let byte_len = std::fs::metadata(&path)
                .expect("private sample metadata must be readable")
                .len();
            let started = Instant::now();
            let result = parse_single_invoice(&path, &hints);
            let elapsed_ms = started.elapsed().as_millis();
            let sample = path
                .file_name()
                .and_then(|value| value.to_str())
                .expect("private sample filename must be UTF-8")
                .to_string();
            match result {
                Ok(parsed) => {
                    success += 1;
                    *levels
                        .entry(parse_level_to_string(parsed.parse_level).to_string())
                        .or_default() += 1;
                    parsed_for_validation.push(parsed.clone());
                    outcomes.push(PrivateParseOutcome {
                        sample,
                        extension,
                        byte_len,
                        elapsed_ms,
                        parsed: Some(parsed),
                        error_category: None,
                        error_private: None,
                    });
                }
                Err(error) => {
                    failed += 1;
                    outcomes.push(PrivateParseOutcome {
                        sample,
                        extension,
                        byte_len,
                        elapsed_ms,
                        parsed: None,
                        error_category: Some("parse_failed"),
                        error_private: Some(format!("{error:#}")),
                    });
                }
            }
        }

        let result_name = std::env::var("INVOICE_REAL_PARSE_RESULT_FILE")
            .unwrap_or_else(|_| "parse-results.private.json".to_string());
        assert!(
            result_name.ends_with(".private.json")
                && !result_name.contains(['/', '\\'])
                && result_name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || ".-_".contains(character)),
            "private parse result filename is invalid"
        );
        let result_path = capture_root.join(&result_name);
        let result_json = serde_json::to_vec_pretty(&outcomes)
            .expect("private parse outcomes must serialize to JSON");
        std::fs::write(&result_path, result_json)
            .expect("private parse outcomes must remain in the private capture root");

        let normalized = canonicalize_parsed_invoices(parsed_for_validation);
        struct ValidationResolver;
        impl AmbiguityResolver for ValidationResolver {
            fn resolve(
                &self,
                _ambiguities: &[Ambiguity],
            ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
                Ok(Vec::new())
            }
        }
        let grouping = group_invoices(
            &normalized.invoices[..normalized.canonical_count],
            &GroupingConfig {
                home_cities: vec!["北京".to_string()],
                home_station_aliases: None,
                ambiguity_handler: Box::new(ValidationResolver),
            },
        )
        .expect("normalized private invoices must be groupable");
        let mut membership_counts = HashMap::<usize, usize>::new();
        for input_index in grouping
            .trips
            .iter()
            .flat_map(|trip| trip.invoice_ids.iter().copied())
        {
            *membership_counts.entry(input_index).or_default() += 1;
        }
        let grouped_members = membership_counts.values().sum::<usize>();
        let multiple_group_members = membership_counts
            .values()
            .filter(|count| **count > 1)
            .count();
        let mut canonical_ticket_types = BTreeMap::<String, usize>::new();
        for invoice in &normalized.invoices[..normalized.canonical_count] {
            *canonical_ticket_types
                .entry(format!("{:?}", invoice.ticket_type))
                .or_default() += 1;
        }
        let business_groups = grouping
            .trips
            .iter()
            .filter(|trip| matches!(trip.kind, TripKind::BusinessTrip { .. }))
            .count();
        let local_groups = grouping
            .trips
            .iter()
            .filter(|trip| matches!(trip.kind, TripKind::LocalMonth { .. }))
            .count();
        let max_business_members = grouping
            .trips
            .iter()
            .filter(|trip| matches!(trip.kind, TripKind::BusinessTrip { .. }))
            .map(|trip| trip.invoice_ids.len())
            .max()
            .unwrap_or_default();
        let max_business_days = grouping
            .trips
            .iter()
            .filter(|trip| matches!(trip.kind, TripKind::BusinessTrip { .. }))
            .map(|trip| (trip.end_date - trip.start_date).num_days() + 1)
            .max()
            .unwrap_or_default();

        let summary = format!(
            "verification=real-private-parse-v2\ncandidate_files={}\nparse_success={}\nparse_failed={}\nformats={}\nlevels={}\ncanonical_invoices={}\nduplicate_documents={}\ncanonical_ticket_types={}\ngroups={}\nbusiness_groups={}\nlocal_groups={}\nmax_business_members={}\nmax_business_days={}\ngrouped_members={}\ndistinct_grouped_members={}\nmultiple_group_members={}\nambiguities={}\nprivate_fields_logged=false\n",
            outcomes.len(),
            success,
            failed,
            serde_json::to_string(&formats).expect("format counts must serialize"),
            serde_json::to_string(&levels).expect("level counts must serialize"),
            normalized.canonical_count,
            normalized
                .invoices
                .len()
                .saturating_sub(normalized.canonical_count),
            serde_json::to_string(&canonical_ticket_types)
                .expect("ticket counts must serialize"),
            grouping.trips.len(),
            business_groups,
            local_groups,
            max_business_members,
            max_business_days,
            grouped_members,
            membership_counts.len(),
            multiple_group_members,
            grouping.ambiguities.len(),
        );
        let summary_name = result_name
            .replace("results", "summary")
            .replace(".json", ".txt");
        std::fs::write(capture_root.join(summary_name), &summary)
            .expect("private parse summary must remain in the private capture root");
        print!("{summary}");
        assert!(success > 0, "all real candidate files failed to parse");
        assert_eq!(
            multiple_group_members, 0,
            "one canonical expense must not belong to multiple groups"
        );
        assert_eq!(
            membership_counts.len(),
            normalized.canonical_count,
            "every canonical expense must have one grouping destination"
        );
    }

    #[test]
    #[ignore = "rebuilds only the explicitly selected ledger under INVOICE_ASSISTANT_HOME"]
    fn real_private_rebuilds_unreviewed_batch_from_cached_collection() {
        let ledger_path = PathBuf::from(
            std::env::var_os("INVOICE_REAL_REBUILD_LEDGER")
                .expect("INVOICE_REAL_REBUILD_LEDGER must be set"),
        );
        let batch_id = std::env::var("INVOICE_REAL_REBUILD_BATCH_ID")
            .expect("INVOICE_REAL_REBUILD_BATCH_ID must be set")
            .parse::<i64>()
            .expect("batch id must be an integer");
        let import_id = std::env::var("INVOICE_REAL_REBUILD_IMPORT_ID")
            .expect("INVOICE_REAL_REBUILD_IMPORT_ID must be set")
            .parse::<i64>()
            .expect("import id must be an integer");
        let pipeline_id = std::env::var("INVOICE_REAL_REBUILD_PIPELINE_ID")
            .expect("INVOICE_REAL_REBUILD_PIPELINE_ID must be set");
        let expected_invoices = std::env::var("INVOICE_REAL_REBUILD_EXPECTED_INVOICES")
            .expect("INVOICE_REAL_REBUILD_EXPECTED_INVOICES must be set")
            .parse::<usize>()
            .expect("expected invoice count must be an integer");
        let expected_total = std::env::var("INVOICE_REAL_REBUILD_EXPECTED_TOTAL")
            .expect("INVOICE_REAL_REBUILD_EXPECTED_TOTAL must be set");
        let data_root = crate::paths::data_root().expect("data root must be available");
        assert_eq!(
            std::fs::canonicalize(&ledger_path).unwrap(),
            std::fs::canonicalize(data_root.join("ledger.db")).unwrap(),
            "private rebuild may only target the active product ledger"
        );
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        assert!(!ledger_path.starts_with(repo_root));

        let db = invoice_store::LedgerDb::new(&ledger_path).expect("ledger must open");
        let batch = db.get_batch(batch_id).expect("target batch must exist");
        db.reset_draft_batch_automatic_analysis(batch_id)
            .expect("unreviewed draft analysis must reset");
        let task_dir = data_root.join("temp").join(&pipeline_id);
        assert!(!task_dir.exists(), "rebuild pipeline id must be new");
        db.create_pipeline_run(
            &pipeline_id,
            "{}",
            "collection_import",
            task_dir.to_str().expect("task path must be UTF-8"),
        )
        .expect("rebuild pipeline must be created");
        db.link_batch_collection_import_pipeline(import_id, &pipeline_id)
            .expect("frozen collection import must link to rebuild pipeline");
        let roots = db
            .collection_import_file_paths(import_id, batch_id)
            .expect("frozen collection paths must remain available")
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let roots = resolve_managed_material_paths(&data_root, &roots)
            .expect("cached collection files must stay inside the product library");
        let collected =
            crate::local_source::collect_local_inputs(&roots, &task_dir.join("collection-import"))
                .expect("cached collection files must stage");
        assert_eq!(collected.files.len(), 115);
        crate::pipeline_checkpoint::write_collected(&task_dir, &collected.files)
            .expect("collected checkpoint must persist");

        let hints = builtin_hints();
        let mut parsed = Vec::new();
        let mut pending = Vec::new();
        for path in &collected.files {
            match parse_single_invoice(path, &hints) {
                Ok(invoice) => parsed.push(invoice),
                Err(error) => pending.push(classify_pending_document(path, &error)),
            }
        }
        assert_eq!(parsed.len() + pending.len(), collected.files.len());
        crate::pipeline_checkpoint::write_parsed(&task_dir, &parsed)
            .expect("parsed checkpoint must persist");
        crate::pipeline_checkpoint::write_json_checkpoint(&task_dir, "materials", &pending)
            .expect("materials checkpoint must persist");

        let normalized = canonicalize_parsed_invoices(parsed);
        assert_eq!(normalized.canonical_count, expected_invoices);
        let checked = DedupeResult {
            duplicate_reasons: HashMap::new(),
            duplicate_reasons_by_index: vec![None; normalized.invoices.len()],
            canonical_count: normalized.canonical_count,
            canonicalization_version: CANONICALIZATION_VERSION,
            invoices: normalized.invoices,
        };
        for invoice in &checked.invoices[..checked.canonical_count] {
            assert!(
                db.find_potential_duplicates(
                    &invoice.invoice_number,
                    &invoice.total_amount,
                    &invoice.issue_date,
                    to_store_ticket_type(invoice.ticket_type).to_str(),
                    None,
                )
                .unwrap()
                .is_empty(),
                "reset batch must not retain historical duplicate dependencies"
            );
        }
        crate::pipeline_checkpoint::write_json_checkpoint(&task_dir, "deduped", &checked)
            .expect("dedupe checkpoint must persist");

        struct RebuildResolver;
        impl AmbiguityResolver for RebuildResolver {
            fn resolve(
                &self,
                _ambiguities: &[Ambiguity],
            ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
                Ok(Vec::new())
            }
        }
        let grouped = group_invoices(
            &checked.invoices[..checked.canonical_count],
            &GroupingConfig {
                home_cities: vec!["北京".to_string()],
                home_station_aliases: None,
                ambiguity_handler: Box::new(RebuildResolver),
            },
        )
        .expect("canonical expenses must group");
        let grouped_checkpoint = GroupedCheckpoint {
            home_city: "北京".to_string(),
            result: grouped.clone(),
            canonicalization_version: CANONICALIZATION_VERSION,
        };
        crate::pipeline_checkpoint::write_json_checkpoint(
            &task_dir,
            "grouped",
            &grouped_checkpoint,
        )
        .expect("grouping checkpoint must persist");
        db.update_pipeline_checkpoint(&pipeline_id, "grouped")
            .expect("pipeline stage must advance");

        let stored_paths = crate::pipeline_checkpoint::prepare_originals(
            &data_root,
            &pipeline_id,
            &checked.invoices,
        )
        .expect("normalized originals must persist");
        let pending_paths = crate::pipeline_checkpoint::prepare_pending_documents(
            &data_root,
            &pipeline_id,
            &pending
                .iter()
                .map(|candidate| candidate.source_path.clone())
                .collect::<Vec<_>>(),
        )
        .expect("pending originals must persist");
        let mut verification_by_invoice = HashMap::<String, String>::new();
        for (invoice, path) in checked.invoices.iter().zip(stored_paths.iter()) {
            let Some(status) = verification_for_path(path).unwrap() else {
                continue;
            };
            let key = canonical_identity_key(invoice);
            if verification_by_invoice.get(&key).map_or(true, |current| {
                verification_rank(&status) > verification_rank(current)
            }) {
                verification_by_invoice.insert(key, status);
            }
        }
        let reported = checked
            .invoices
            .iter()
            .zip(stored_paths.iter())
            .map(|(invoice, path)| ReportedInvoice {
                id: 0,
                batch_id: 0,
                invoice_number: invoice.invoice_number.clone(),
                issue_date: invoice.issue_date,
                amount: invoice.total_amount,
                tax_amount: invoice.tax_amount,
                buyer_name: invoice.buyer_name.clone(),
                seller_name: invoice.seller_name.clone(),
                ticket_type: to_store_ticket_type(invoice.ticket_type),
                city: invoice.city.clone(),
                departure_time: invoice.departure_time,
                checkin_date: invoice.checkin_date,
                file_path: path.to_string_lossy().into_owned(),
                created_at: chrono::Utc::now().naive_utc(),
                updated_at: chrono::Utc::now().naive_utc(),
                verification_result: verification_by_invoice
                    .get(&canonical_identity_key(invoice))
                    .cloned(),
                is_duplicate: false,
                duplicate_reason: None,
            })
            .collect::<Vec<_>>();
        let groups = grouped
            .trips
            .iter()
            .enumerate()
            .map(|(group_index, trip)| {
                let (kind, title, requires_review) = grouping_kind_metadata(&trip.kind);
                IndexedInvoiceGroup {
                    group_index,
                    kind,
                    title,
                    start_date: trip.start_date.to_string(),
                    end_date: trip.end_date.to_string(),
                    confidence: trip.confidence,
                    requires_review,
                    evidence_json: serde_json::json!({
                        "tripKind": trip.kind,
                        "homeCity": "北京",
                        "inputIndexes": trip.invoice_ids,
                    })
                    .to_string(),
                    members: trip
                        .invoice_ids
                        .iter()
                        .map(|input_index| IndexedInvoiceGroupMember {
                            input_index: *input_index,
                            match_reason: format!("{GROUPING_RULE_VERSION}：唯一费用自动归组"),
                        })
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        let indexed_grouping = IndexedBatchGrouping {
            rule_version: GROUPING_RULE_VERSION.to_string(),
            home_cities_json: "[\"北京\"]".to_string(),
            overall_confidence: grouped.overall_confidence,
            ambiguities_json: serde_json::to_string(&grouped.ambiguities).unwrap(),
            groups,
        };
        let pending_records = pending
            .iter()
            .zip(pending_paths.iter())
            .map(|(candidate, path)| NewPendingInvoiceDocument {
                proposed_role: candidate.proposed_role.clone(),
                file_path: path.to_string_lossy().into_owned(),
                original_name: staged_original_name(&candidate.source_path),
                mime_type: mime_type_for_pipeline_path(path).map(str::to_string),
                sha256: Some(dedupe::sha256_hex(
                    &std::fs::read(path).expect("pending file must read"),
                )),
                detection_reason: candidate.detection_reason.clone(),
                auto_assign_invoice_index: None,
            })
            .collect::<Vec<_>>();
        let stored_batch_id = db
            .store_pipeline_batch_atomic_with_documents(
                &pipeline_id,
                &batch.name,
                &batch.month,
                Some(batch_id),
                &reported,
                &indexed_grouping,
                &pending_records,
            )
            .expect("rebuilt batch must store atomically");
        assert_eq!(stored_batch_id, batch_id);
        db.mark_batch_collection_import_completed(&pipeline_id)
            .expect("collection import must complete");
        let rebuilt = db.get_batch(batch_id).unwrap();
        assert_eq!(rebuilt.invoice_count as usize, expected_invoices);
        assert_eq!(rebuilt.total_amount.to_string(), expected_total);
        let saved_grouping = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert_eq!(saved_grouping.groups.len(), 5);
        assert_eq!(
            saved_grouping
                .groups
                .iter()
                .filter(|group| group.kind == "business_trip")
                .count(),
            3
        );
        assert_eq!(
            saved_grouping
                .groups
                .iter()
                .filter(|group| group.kind == "local_month")
                .count(),
            2
        );
        assert!(saved_grouping
            .groups
            .iter()
            .filter(|group| group.kind == "business_trip")
            .all(|group| group.members.len() <= 2));
        let grouped_invoice_ids = saved_grouping
            .groups
            .iter()
            .flat_map(|group| group.members.iter().map(|member| member.invoice_id))
            .collect::<Vec<_>>();
        assert_eq!(grouped_invoice_ids.len(), expected_invoices);
        assert_eq!(
            grouped_invoice_ids
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len(),
            expected_invoices
        );
        let rebuilt_invoices = db.list_invoices_by_batch(batch_id).unwrap();
        assert_eq!(rebuilt_invoices.len(), expected_invoices);
        assert_eq!(
            rebuilt_invoices
                .iter()
                .filter(|invoice| invoice.ticket_type.to_str() == "rail")
                .count(),
            14
        );
        assert!(rebuilt_invoices.iter().all(|invoice| !invoice.is_duplicate));
        let expense_items = db.list_expense_items_by_batch(batch_id).unwrap();
        assert_eq!(expense_items.len(), expected_invoices);
        assert_eq!(
            expense_items
                .iter()
                .map(|expense| expense.documents.len())
                .sum::<usize>(),
            checked.invoices.len()
        );
        assert_eq!(
            db.list_pending_invoice_documents(batch_id).unwrap().len(),
            pending_records.len()
        );
        let complete = PipelineComplete {
            batch_id,
            invoice_count: expected_invoices,
            total_amount: rebuilt.total_amount.to_string(),
            excel_path: None,
            link_only_email_count: 0,
            pending_document_count: pending_records.len(),
            source_file_count: collected.files.len(),
            parsed_document_count: checked.invoices.len(),
            canonical_invoice_count: checked.canonical_count,
            duplicate_document_count: checked
                .invoices
                .len()
                .saturating_sub(checked.canonical_count),
        };
        crate::pipeline_checkpoint::write_json_checkpoint(&task_dir, "complete", &complete)
            .expect("complete checkpoint must persist");
        print!(
            "verification=real-private-rebuild-v1\nbatch_id={}\nsource_files={}\nparsed_documents={}\ncanonical_invoices={}\nduplicate_documents={}\npending_documents={}\ngroups={}\ntotal_amount={}\nprivate_fields_logged=false\n",
            batch_id,
            complete.source_file_count,
            complete.parsed_document_count,
            complete.canonical_invoice_count,
            complete.duplicate_document_count,
            complete.pending_document_count,
            saved_grouping.groups.len(),
            complete.total_amount,
        );
    }
}
