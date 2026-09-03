//! 独立邮件收集工作台。
//!
//! 本模块只读取邮件、形成来源级台账并持久化原始附件。它刻意不依赖
//! `invoice_parse` 和 `invoice_grouping`，发票字段解析只会在批次导入后发生。

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::NaiveDate;
use invoice_collect::config::{DateRange, ImapConfig};
use invoice_collect::{classify, dedupe, extract, imap_client};
use invoice_store::models::{
    BatchCollectionImport, CollectedEmailMessage, CollectedEmailReviewSnapshot,
    EmailCollectionTask, NewCollectedEmailAttachment, NewCollectedEmailLink,
    NewCollectedEmailMessage, NewCollectedEmailReviewSnapshot,
};
use serde::Serialize;
use tauri::{ipc::Response, AppHandle, Emitter, Manager, State};

use crate::error::{AppError, AppResult};
use crate::AppState;

const MAX_COLLECTION_LIBRARY_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MAX_COLLECTION_LIBRARY_ENTRIES: usize = 100_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectionProgress {
    task_id: i64,
    current: usize,
    total: usize,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectionCompleted {
    task_id: i64,
    message_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectionFailed {
    task_id: i64,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectedEmailReviewLink {
    pub id: i64,
    pub label: String,
    pub host: String,
    pub scheme: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectedEmailReviewDetail {
    pub available: bool,
    pub sender_name: Option<String>,
    pub sender_address: Option<String>,
    pub body_text: String,
    pub body_truncated: bool,
    pub analyzed_at: Option<String>,
    pub links: Vec<CollectedEmailReviewLink>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectedQrExtractionResult {
    pub detected: bool,
    pub qr_dominant: bool,
    pub browser_link_count: usize,
    pub review: CollectedEmailReviewDetail,
}

fn new_review_snapshot(content: extract::EmailReviewContent) -> NewCollectedEmailReviewSnapshot {
    NewCollectedEmailReviewSnapshot {
        sender_name: content.sender_name,
        sender_address: content.sender_address,
        body_text: content.body_text,
        body_truncated: content.body_truncated,
        links: content
            .links
            .into_iter()
            .map(|link| NewCollectedEmailLink {
                scheme: if link.url.starts_with("https://") {
                    "https".to_string()
                } else {
                    "http".to_string()
                },
                label: link.label,
                host: link.host,
                url: link.url,
            })
            .collect(),
    }
}

fn review_detail(snapshot: Option<CollectedEmailReviewSnapshot>) -> CollectedEmailReviewDetail {
    let Some(snapshot) = snapshot else {
        return CollectedEmailReviewDetail {
            available: false,
            sender_name: None,
            sender_address: None,
            body_text: String::new(),
            body_truncated: false,
            analyzed_at: None,
            links: Vec::new(),
        };
    };
    CollectedEmailReviewDetail {
        available: true,
        sender_name: snapshot.sender_name,
        sender_address: snapshot.sender_address,
        body_text: snapshot.body_text,
        body_truncated: snapshot.body_truncated,
        analyzed_at: Some(snapshot.analyzed_at),
        links: snapshot
            .links
            .into_iter()
            .map(|link| CollectedEmailReviewLink {
                id: link.id,
                label: link.label,
                host: link.host,
                scheme: link.scheme,
            })
            .collect(),
    }
}

#[tauri::command]
pub fn create_email_collection_task(
    name: String,
    date_start: String,
    date_end: String,
    state: State<Mutex<AppState>>,
) -> AppResult<i64> {
    let start = NaiveDate::parse_from_str(&date_start, "%Y-%m-%d")
        .map_err(|_| AppError::validation("开始日期格式必须为 YYYY-MM-DD"))?;
    let end = NaiveDate::parse_from_str(&date_end, "%Y-%m-%d")
        .map_err(|_| AppError::validation("结束日期格式必须为 YYYY-MM-DD"))?;
    if start >= end {
        return Err(AppError::validation("结束日期必须晚于开始日期"));
    }
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    let account_email = app_state
        .session_email()
        .ok_or_else(|| AppError::validation("请先在设置中输入本次会话的邮箱授权码"))?;
    app_state
        .ledger_db()?
        .create_email_collection_task(&name, account_email, &date_start, &date_end)
        .map_err(|error| map_store_error("创建邮件收集任务失败", error))
}

#[tauri::command]
pub fn list_email_collection_tasks(
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<EmailCollectionTask>> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .list_email_collection_tasks()
        .map_err(|error| map_store_error("读取邮件收集任务失败", error))
}

/// 删除本地收集任务及其受控材料目录，不会连接邮箱或修改服务器邮件。
/// 已形成批次导入快照的任务由存储层拒绝删除，以保留来源追溯关系。
#[tauri::command]
pub fn delete_email_collection_task(task_id: i64, state: State<Mutex<AppState>>) -> AppResult<()> {
    validate_positive_id(task_id, "邮件收集任务")?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    let task = app_state
        .ledger_db()?
        .get_email_collection_task(task_id)
        .map_err(|error| map_store_error("读取邮件收集任务失败", error))?;
    if task.status == "collecting" {
        return Err(AppError::validation(
            "正在收集的任务不能删除，请等待完成或重启后再试",
        ));
    }
    let staged_materials = stage_collection_task_materials(task_id)?;
    if let Err(error) = app_state.ledger_db()?.delete_email_collection_task(task_id) {
        if let Some((original, staged)) = staged_materials.as_ref() {
            if let Err(restore_error) = fs::rename(staged, original) {
                return Err(AppError::io(format!(
                    "任务未删除，但恢复本地材料目录失败: {restore_error}"
                )));
            }
        }
        return Err(map_store_error("删除邮件收集任务失败", error));
    }
    drop(app_state);

    if let Some((_, staged)) = staged_materials {
        if let Err(error) = fs::remove_dir_all(&staged) {
            tracing::warn!(
                task_id,
                path = %staged.display(),
                %error,
                "收集任务已删除，但暂存材料目录清理失败"
            );
        }
    }
    Ok(())
}

#[tauri::command]
pub fn get_email_collection_task(
    task_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<EmailCollectionTask> {
    validate_positive_id(task_id, "邮件收集任务")?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .get_email_collection_task(task_id)
        .map_err(|error| map_store_error("读取邮件收集任务失败", error))
}

#[tauri::command]
pub fn list_collected_email_messages(
    task_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<CollectedEmailMessage>> {
    validate_positive_id(task_id, "邮件收集任务")?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .list_collected_email_messages(task_id)
        .map_err(|error| map_store_error("读取邮件材料台账失败", error))
}

/// 只从本地台账读取收集阶段已经生成的正文和链接，不访问邮箱。
#[tauri::command]
pub fn get_collected_email_review_detail(
    message_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<CollectedEmailReviewDetail> {
    validate_positive_id(message_id, "邮件材料")?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    let snapshot = app_state
        .ledger_db()?
        .get_collected_email_review_snapshot(message_id)
        .map_err(|error| map_store_error("读取邮件本地审核数据失败", error))?;
    Ok(review_detail(snapshot))
}

/// 直接打开收集阶段持久化的链接。不会连接邮箱，WebView 也不会收到完整 URL。
#[tauri::command]
pub fn open_collected_email_link(
    message_id: i64,
    link_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    validate_positive_id(message_id, "邮件材料")?;
    validate_positive_id(link_id, "邮件下载链接")?;
    let link = {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        app_state
            .ledger_db()?
            .get_collected_email_link(message_id, link_id)
            .map_err(|error| map_store_error("读取邮件下载链接失败", error))?
    };
    let validated = extract::validated_review_link(&link.url, &link.label)
        .ok_or_else(|| AppError::validation("该邮件链接未通过安全校验，请重新分析此邮件"))?;
    if validated.host != link.host || validated.url != link.url {
        return Err(AppError::validation(
            "邮件链接校验结果已经变化，请重新分析此邮件",
        ));
    }
    open_web_link_with_windows_default(&validated.url)
}

/// 只读取收集材料库中的本地图片并提取二维码；不连接邮箱、不自动访问二维码地址。
/// 完整 URL 只在 Rust 后端持久化，WebView 仅收到域名和链接编号。
#[tauri::command]
pub async fn extract_collected_attachment_qr_links(
    attachment_id: i64,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<CollectedQrExtractionResult> {
    validate_positive_id(attachment_id, "邮件附件")?;
    let attachment = {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        app_state
            .ledger_db()?
            .get_collected_email_attachment(attachment_id)
            .map_err(|error| map_store_error("读取邮件附件失败", error))?
    };
    if !is_image_path(Path::new(&attachment.original_name)) {
        return Err(AppError::validation("只有图片附件可以提取二维码地址"));
    }
    let path = collected_attachment_path(attachment_id, &state)?;
    let original_name = attachment.original_name.clone();
    let content_type = attachment
        .mime_type
        .clone()
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let analysis = tauri::async_runtime::spawn_blocking(move || {
        let data = fs::read(&path)
            .map_err(|error| AppError::io(format!("读取二维码图片失败（{}）", error.kind())))?;
        Ok::<_, AppError>(extract::analyze_qr_attachment(&extract::RawAttachment {
            filename: original_name,
            content_type,
            data,
        }))
    })
    .await
    .map_err(|_| AppError::internal("二维码识别线程异常"))??;

    if analysis.detected {
        let links = analysis
            .links
            .iter()
            .map(|link| NewCollectedEmailLink {
                scheme: if link.url.starts_with("https://") {
                    "https".to_string()
                } else {
                    "http".to_string()
                },
                label: link.label.clone(),
                host: link.host.clone(),
                url: link.url.clone(),
            })
            .collect::<Vec<_>>();
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        app_state
            .ledger_db()?
            .store_collected_attachment_qr_analysis(attachment_id, &links, analysis.qr_dominant)
            .map_err(|error| map_store_error("保存二维码分析结果失败", error))?;
    }
    let review = get_collected_email_review_detail(attachment.message_id, state)?;
    Ok(CollectedQrExtractionResult {
        detected: analysis.detected,
        qr_dominant: analysis.qr_dominant,
        browser_link_count: analysis.links.len(),
        review,
    })
}

/// 只有用户明确请求时才重新连接邮箱、下载这一封邮件并替换本地审核快照。
#[tauri::command]
pub async fn reanalyze_collected_email_message(
    message_id: i64,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<CollectedEmailReviewDetail> {
    let source = fetch_collected_email_raw(message_id, &state).await?;
    let extracted = extract::extract_email(&source.raw)
        .map_err(|error| AppError::parse(format!("邮件 MIME 重新解析失败: {error}")))?;
    let review_content = extract::extract_review_content(&source.raw)
        .map_err(|error| AppError::parse(format!("邮件正文重新解析失败: {error}")))?;
    let material_dir = collection_material_dir(source.task.id)?;
    let existing_library_bytes = collection_library_bytes(
        material_dir
            .parent()
            .ok_or_else(|| AppError::internal("邮件材料库路径缺少父目录"))?,
    )?;
    let mut stored_hashes = HashSet::new();
    let mut stored_total = 0;
    let analysis = analyze_collection_attachments(
        &extracted,
        !review_content.links.is_empty(),
        false,
        &material_dir,
        existing_library_bytes,
        &mut stored_hashes,
        &mut stored_total,
    )?;
    let preserved_manual_material = source.message.attachments.iter().any(|attachment| {
        attachment.manual_import
            && attachment.stored_path.is_some()
            && !attachment.user_excluded
            && matches!(
                attachment.status.as_str(),
                "candidate" | "supporting_candidate"
            )
    });
    let replacement_status = if preserved_manual_material && analysis.status != "failed" {
        "has_candidates".to_string()
    } else {
        analysis.status
    };
    let replacement = NewCollectedEmailMessage {
        mailbox_folder: source.message.mailbox_folder,
        uid: source.message.uid,
        message_id_sha256: extracted
            .message_id
            .as_deref()
            .map(|value| dedupe::sha256_hex(value.as_bytes())),
        sender: ledger_text(&extracted.from, 500),
        subject: ledger_text(&extracted.subject, 1_000),
        received_at: source.message.received_at,
        status: replacement_status,
        error_category: analysis
            .has_failure
            .then(|| "attachment_problem".to_string()),
        review: Some(new_review_snapshot(review_content)),
        attachments: analysis.attachments,
    };
    {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        app_state
            .ledger_db()?
            .replace_collected_email_analysis(message_id, &replacement)
            .map_err(|error| map_store_error("替换邮件本地分析结果失败", error))?;
    }
    get_collected_email_review_detail(message_id, state)
}

#[tauri::command]
pub fn get_collected_attachment_preview_metadata(
    attachment_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<super::review::InvoicePreviewMetadata> {
    let path = collected_attachment_path(attachment_id, &state)?;
    super::review::inspect_preview_path(&path)
}

#[tauri::command]
pub fn read_collected_attachment_preview(
    attachment_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Response> {
    let path = collected_attachment_path(attachment_id, &state)?;
    let metadata = super::review::inspect_preview_path(&path)?;
    if !matches!(
        metadata.preview_kind.as_str(),
        "image" | "pdf" | "ofd" | "text"
    ) {
        return Err(AppError::validation("该邮件附件当前不能在应用内预览"));
    }
    let bytes = fs::read(&path)
        .map_err(|error| AppError::io(format!("读取邮件附件失败（{}）", error.kind())))?;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub async fn render_collected_pdf_preview_page(
    attachment_id: i64,
    page: u32,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<Response> {
    if page == 0 {
        return Err(AppError::validation("PDF 页码必须从 1 开始"));
    }
    let path = collected_attachment_path(attachment_id, &state)?;
    let metadata = super::review::inspect_preview_path(&path)?;
    if metadata.preview_kind != "pdf" {
        return Err(AppError::validation("所选邮件附件不是可预览的 PDF"));
    }
    let rendered = tauri::async_runtime::spawn_blocking(move || {
        invoice_parse::pdf_ocr::render_pdf_preview_page(&path, page - 1, 2_200)
    })
    .await
    .map_err(|_| AppError::internal("PDF 预览线程异常"))?
    .map_err(|error| AppError::io(format!("PDF 页面渲染失败: {error}")))?;
    Ok(Response::new(rendered))
}

#[tauri::command]
pub async fn render_collected_pdf_text_preview_page(
    attachment_id: i64,
    page: u32,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<invoice_parse::pdf_preview::PdfPreviewPage> {
    if page == 0 {
        return Err(AppError::validation("PDF 页码必须从 1 开始"));
    }
    let path = collected_attachment_path(attachment_id, &state)?;
    let metadata = super::review::inspect_preview_path(&path)?;
    if metadata.preview_kind != "pdf" {
        return Err(AppError::validation("所选邮件附件不是可预览的 PDF"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = fs::read(&path)
            .map_err(|error| AppError::io(format!("读取 PDF 附件失败（{}）", error.kind())))?;
        invoice_parse::pdf_preview::render_text_preview_page(&bytes, &path, page)
            .map_err(|error| AppError::parse(format!("PDF 兼容版式预览失败: {error}")))
    })
    .await
    .map_err(|_| AppError::internal("PDF 兼容预览线程异常"))?
}

#[tauri::command]
pub async fn render_collected_ofd_preview_page(
    attachment_id: i64,
    page: u32,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<invoice_parse::ofd_preview::OfdPreviewPage> {
    if page == 0 {
        return Err(AppError::validation("OFD 页码必须从 1 开始"));
    }
    let path = collected_attachment_path(attachment_id, &state)?;
    let metadata = super::review::inspect_preview_path(&path)?;
    if metadata.preview_kind != "ofd" {
        return Err(AppError::validation("所选邮件附件不是可预览的 OFD"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = fs::read(&path)
            .map_err(|error| AppError::io(format!("读取 OFD 附件失败（{}）", error.kind())))?;
        invoice_parse::ofd_preview::render_preview_page(&bytes, &path, page)
            .map_err(|error| AppError::parse(format!("OFD 版式预览生成失败: {error}")))
    })
    .await
    .map_err(|_| AppError::internal("OFD 预览线程异常"))?
}

#[tauri::command]
pub fn open_collected_attachment(
    attachment_id: i64,
    reveal: bool,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    let path = collected_attachment_path(attachment_id, &state)?;
    super::review::inspect_preview_path(&path)?;
    let target = if reveal {
        path.parent()
            .ok_or_else(|| AppError::validation("无法定位附件所在文件夹"))?
            .to_path_buf()
    } else {
        path
    };
    super::review::open_with_windows_default(&target)
}

/// 启动一次性 INBOX 收集。返回独立运行标识，实际工作在后台执行。
#[tauri::command]
pub async fn start_email_collection_task(
    app: AppHandle,
    task_id: i64,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<String> {
    validate_positive_id(task_id, "邮件收集任务")?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let (task, email, password) = {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        let task = app_state
            .ledger_db()?
            .get_email_collection_task(task_id)
            .map_err(|error| map_store_error("读取邮件收集任务失败", error))?;
        let (email, password) = app_state
            .session_credential_copy()
            .ok_or_else(|| AppError::validation("本次会话尚未输入邮箱授权码，请先到设置中配置"))?;
        if !email.eq_ignore_ascii_case(&task.account_email) {
            return Err(AppError::validation(
                "当前会话邮箱与该收集任务不一致，请先切换邮箱账号",
            ));
        }
        app_state
            .ledger_db()?
            .mark_email_collection_started(task_id, &run_id)
            .map_err(|error| map_store_error("启动邮件收集任务失败", error))?;
        (task, email, password)
    };

    let task_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = collect_task_messages(&task_app, &task, &email, &password).await;
        let state = task_app.state::<Mutex<AppState>>();
        match result {
            Ok(messages) => {
                let stored = state
                    .lock()
                    .map_err(|_| AppError::internal("状态锁错误"))
                    .and_then(|app_state| {
                        app_state
                            .ledger_db()?
                            .store_email_collection_results(task_id, &messages)
                            .map_err(|error| map_store_error("保存邮件材料台账失败", error))
                    });
                match stored {
                    Ok(()) => {
                        let _ = task_app.emit(
                            &format!("email-collection:complete:{task_id}"),
                            CollectionCompleted {
                                task_id,
                                message_count: messages.len(),
                            },
                        );
                    }
                    Err(error) => record_collection_failure(&task_app, task_id, &error),
                }
            }
            Err(error) => record_collection_failure(&task_app, task_id, &error),
        }
    });
    Ok(run_id)
}

#[tauri::command]
pub fn resolve_collected_email_message(
    message_id: i64,
    action: String,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    validate_positive_id(message_id, "邮件材料")?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .resolve_collected_email_message(message_id, &action)
        .map_err(|error| map_store_error("更新邮件审核状态失败", error))
}

#[tauri::command]
pub fn set_collected_email_attachment_excluded(
    attachment_id: i64,
    excluded: bool,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    validate_positive_id(attachment_id, "邮件附件")?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .set_collected_email_attachment_excluded(attachment_id, excluded)
        .map_err(|error| map_store_error("更新邮件附件有效性失败", error))
}

/// 把用户手工下载的发票文件补充到指定邮件材料包。仅接收发票文件，不接收 EML。
#[tauri::command]
pub fn supplement_collected_email_message(
    message_id: i64,
    paths: Vec<String>,
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<i64>> {
    validate_positive_id(message_id, "邮件材料")?;
    if paths.is_empty() || paths.len() > 100 || paths.iter().any(|path| path.trim().is_empty()) {
        return Err(AppError::validation("请选择 1 至 100 个本地发票文件"));
    }
    let task_id = {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        app_state
            .ledger_db()?
            .collected_email_message_task_id(message_id)
            .map_err(|error| map_store_error("读取邮件材料失败", error))?
    };
    let material_dir = collection_material_dir(task_id)?;
    let roots = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    let preview = crate::local_source::preview_local_inputs(&roots)
        .map_err(|error| AppError::validation(format!("补充文件检查失败: {error}")))?;
    let existing_library_bytes = collection_library_bytes(
        material_dir
            .parent()
            .ok_or_else(|| AppError::internal("邮件材料库路径缺少父目录"))?,
    )?;
    if existing_library_bytes.saturating_add(preview.total_bytes) > MAX_COLLECTION_LIBRARY_BYTES {
        return Err(AppError::validation(
            "邮件材料库将超过 5 GiB；请先导出备份并在“设置与数据”中清理",
        ));
    }
    let collected = crate::local_source::collect_local_inputs(&roots, &material_dir)
        .map_err(|error| AppError::validation(format!("补充文件读取失败: {error}")))?;
    if collected.files.is_empty() {
        return Err(AppError::validation(
            "没有可补充的 PDF、OFD、XML 或发票图片文件",
        ));
    }

    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    let mut ids = Vec::with_capacity(collected.files.len());
    for path in collected.files {
        let (hash, original_name) = staged_file_identity(&path)?;
        let role = role_hint(&original_name, "");
        let status = if role == "invoice" || role == "unknown" {
            "candidate"
        } else {
            "supporting_candidate"
        };
        let metadata = fs::metadata(&path)?;
        let input = NewCollectedEmailAttachment {
            content_sha256: Some(hash),
            original_name,
            container_name: None,
            mime_type: mime_type_for_path(&path).map(str::to_string),
            byte_len: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            status: status.to_string(),
            role_hint: role.to_string(),
            reason: "user_manual_supplement".to_string(),
            stored_path: Some(collection_path_locator(&path)?),
            manual_import: true,
        };
        ids.push(
            app_state
                .ledger_db()?
                .add_collected_email_attachment(message_id, &input)
                .map_err(|error| map_store_error("保存补充材料失败", error))?,
        );
    }
    Ok(ids)
}

#[tauri::command]
pub fn complete_email_collection_review(
    task_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    validate_positive_id(task_id, "邮件收集任务")?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .complete_email_collection_review(task_id)
        .map_err(|error| map_store_error("完成来源审核失败", error))
}

#[tauri::command]
pub fn create_batch_collection_import(
    batch_id: i64,
    task_id: i64,
    attachment_ids: Vec<i64>,
    state: State<Mutex<AppState>>,
) -> AppResult<i64> {
    validate_positive_id(batch_id, "报销批次")?;
    validate_positive_id(task_id, "邮件收集任务")?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .create_batch_collection_import(batch_id, task_id, &attachment_ids)
        .map_err(|error| map_store_error("建立批次来源快照失败", error))
}

#[tauri::command]
pub fn list_batch_collection_sources(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<BatchCollectionImport>> {
    validate_positive_id(batch_id, "报销批次")?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .list_batch_collection_imports(batch_id)
        .map_err(|error| map_store_error("读取批次来源失败", error))
}

struct CollectedEmailRawSource {
    raw: Vec<u8>,
    message: CollectedEmailMessage,
    task: EmailCollectionTask,
}

async fn fetch_collected_email_raw(
    message_id: i64,
    state: &State<'_, Mutex<AppState>>,
) -> AppResult<CollectedEmailRawSource> {
    validate_positive_id(message_id, "邮件材料")?;
    let (message, task, email, password) = {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        let message = app_state
            .ledger_db()?
            .get_collected_email_message(message_id)
            .map_err(|error| map_store_error("读取邮件材料失败", error))?;
        let task = app_state
            .ledger_db()?
            .get_email_collection_task(message.task_id)
            .map_err(|error| map_store_error("读取邮件收集任务失败", error))?;
        let (email, password) = app_state
            .session_credential_copy()
            .ok_or_else(|| AppError::validation("本次会话尚未输入邮箱授权码，请先到设置中配置"))?;
        if !email.eq_ignore_ascii_case(&task.account_email) {
            return Err(AppError::validation(
                "当前会话邮箱与该收集任务不一致，请先切换邮箱账号",
            ));
        }
        (message, task, email, password)
    };
    let uid = u32::try_from(message.uid).map_err(|_| AppError::validation("邮件 UID 无效"))?;
    let date_range = DateRange::parse(&task.date_start, &task.date_end)
        .map_err(|error| AppError::validation(format!("日期范围格式错误: {error}")))?;
    let imap_config = ImapConfig::from_credentials(&email, password.as_str().to_string())
        .map_err(|error| AppError::validation(format!("邮箱配置错误: {error}")))?;
    let mailbox = task.mailbox_folder.clone();
    let raw = tauri::async_runtime::spawn_blocking(move || {
        let mut session = imap_client::Session::connect(&imap_config)
            .map_err(|error| AppError::network(format!("IMAP 连接失败: {error}")))?;
        let uids = session
            .search_range(&mailbox, &date_range)
            .map_err(|error| AppError::network(format!("建立邮件只读指纹失败: {error}")))?;
        if !uids.contains(&uid) {
            return Err(AppError::validation(
                "该邮件已不在收集任务日期范围内，请返回任务列表后重试",
            ));
        }
        let raw = session
            .fetch_raw(uid)
            .map_err(|error| AppError::network(format!("读取邮件正文失败: {error}")))?;
        session
            .verify_read_only_unchanged(&mailbox)
            .map_err(|error| AppError::validation(format!("邮箱只读复核失败: {error}")))?;
        Ok(raw)
    })
    .await
    .map_err(|_| AppError::internal("邮件重新读取线程异常"))??;
    Ok(CollectedEmailRawSource { raw, message, task })
}

fn collected_attachment_path(
    attachment_id: i64,
    state: &State<'_, Mutex<AppState>>,
) -> AppResult<PathBuf> {
    validate_positive_id(attachment_id, "邮件附件")?;
    let locator = {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        app_state
            .ledger_db()?
            .get_collected_email_attachment(attachment_id)
            .map_err(|error| map_store_error("读取邮件附件失败", error))?
            .stored_path
            .ok_or_else(|| AppError::validation("该附件未保存到本地材料库"))?
    };
    let data_root = crate::paths::data_root()
        .map_err(|error| AppError::io(format!("无法定位数据目录: {error}")))?;
    let library = data_root.join("collection-files");
    let library_metadata = fs::symlink_metadata(&library)
        .map_err(|_| AppError::validation("邮件材料库不存在，请重新收集或恢复完整备份"))?;
    if !library_metadata.is_dir() || is_reparse_point(&library_metadata) {
        return Err(AppError::validation("邮件材料库必须是普通本地目录"));
    }
    let canonical_library =
        fs::canonicalize(&library).map_err(|_| AppError::validation("邮件材料库路径无效"))?;
    let path = resolve_collection_locator(&data_root, Path::new(&locator))?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| AppError::validation("邮件附件不存在，请重新收集或恢复完整备份"))?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err(AppError::validation("邮件附件必须是普通本地文件"));
    }
    let canonical = fs::canonicalize(path).map_err(|_| AppError::validation("邮件附件路径无效"))?;
    if !canonical.starts_with(canonical_library) {
        return Err(AppError::validation("邮件附件越过受控材料库，已拒绝访问"));
    }
    Ok(canonical)
}

fn resolve_collection_locator(data_root: &Path, locator: &Path) -> AppResult<PathBuf> {
    if locator.is_relative() {
        let mut components = locator.components();
        if !matches!(components.next(), Some(std::path::Component::Normal(name)) if name == "collection-files")
            || components.any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(AppError::validation("邮件附件定位符无效"));
        }
        return Ok(data_root.join(locator));
    }
    if locator.exists() {
        return Ok(locator.to_path_buf());
    }
    let components = locator.components().collect::<Vec<_>>();
    let Some(marker) = components.iter().position(|component| {
        matches!(component, std::path::Component::Normal(name) if *name == "collection-files")
    }) else {
        return Err(AppError::validation("邮件附件绝对路径无法迁移"));
    };
    let mut rebased = data_root.to_path_buf();
    for component in &components[marker..] {
        let std::path::Component::Normal(part) = component else {
            return Err(AppError::validation("邮件附件绝对路径无法迁移"));
        };
        rebased.push(part);
    }
    Ok(rebased)
}

#[cfg(target_os = "windows")]
fn open_web_link_with_windows_default(url: &str) -> AppResult<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide_url = OsStr::new(url)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: the persisted URL was parsed as HTTP(S) without userinfo immediately before this
    // call, and the NUL-terminated buffer remains alive for the duration of ShellExecuteW.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            wide_url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result <= 32 {
        return Err(AppError::io(format!(
            "调用 Windows 系统浏览器失败（ShellExecuteW={result}）"
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn open_web_link_with_windows_default(_url: &str) -> AppResult<()> {
    Err(AppError::io("打开邮件下载链接仅支持 Windows"))
}

async fn collect_task_messages(
    app: &AppHandle,
    task: &EmailCollectionTask,
    email: &str,
    password: &str,
) -> AppResult<Vec<NewCollectedEmailMessage>> {
    let date_range = DateRange::parse(&task.date_start, &task.date_end)
        .map_err(|error| AppError::validation(format!("日期范围格式错误: {error}")))?;
    let imap_config = ImapConfig::from_credentials(email, password)
        .map_err(|error| AppError::validation(format!("邮箱配置错误: {error}")))?;
    emit_progress(app, task.id, 0, 0, "正在连接只读邮箱...");
    let mut session = imap_client::Session::connect(&imap_config)
        .map_err(|error| AppError::network(format!("IMAP 连接失败: {error}")))?;
    let uids = session
        .search_range("INBOX", &date_range)
        .map_err(|error| AppError::network(format!("搜索邮件失败: {error}")))?;
    let summaries = session
        .fetch_summaries(&uids)
        .map_err(|error| AppError::network(format!("读取邮件概要失败: {error}")))?;
    let summary_by_uid = summaries
        .into_iter()
        .map(|summary| (summary.uid, summary))
        .collect::<HashMap<_, _>>();
    let material_dir = collection_material_dir(task.id)?;
    let collection_root = material_dir
        .parent()
        .ok_or_else(|| AppError::internal("邮件材料库路径缺少父目录"))?;
    let existing_library_bytes = collection_library_bytes(collection_root)?;
    let mut stored_hashes = HashSet::new();
    let mut stored_total = 0u64;
    let mut messages = Vec::with_capacity(uids.len());

    for (index, uid) in uids.iter().enumerate() {
        emit_progress(
            app,
            task.id,
            index,
            uids.len(),
            &format!("正在检查邮件 {}/{}", index + 1, uids.len()),
        );
        let raw = match session.fetch_raw(*uid) {
            Ok(raw) => raw,
            Err(error) => {
                let summary = summary_by_uid.get(uid);
                messages.push(failed_message(
                    *uid,
                    summary.map(|value| value.from.as_str()),
                    summary.map(|value| value.subject.as_str()),
                    summary.map(|value| value.internal_date.clone()),
                    "fetch_failed",
                ));
                tracing::warn!(uid, error = %error, "收集任务读取邮件失败");
                continue;
            }
        };
        let extracted = match extract::extract_email(&raw) {
            Ok(extracted) => extracted,
            Err(error) => {
                let summary = summary_by_uid.get(uid);
                messages.push(failed_message(
                    *uid,
                    summary.map(|value| value.from.as_str()),
                    summary.map(|value| value.subject.as_str()),
                    summary.map(|value| value.internal_date.clone()),
                    "mime_parse_failed",
                ));
                tracing::warn!(uid, error = %error, "收集任务解析 MIME 失败");
                continue;
            }
        };
        let review_content = match extract::extract_review_content(&raw) {
            Ok(content) => Some(content),
            Err(error) => {
                tracing::warn!(uid, error = %error, "收集任务生成邮件审核快照失败");
                None
            }
        };
        let review_has_links = review_content
            .as_ref()
            .is_some_and(|content| !content.links.is_empty());
        let review_failed = review_content.is_none();

        let analysis = analyze_collection_attachments(
            &extracted,
            review_has_links,
            review_failed,
            &material_dir,
            existing_library_bytes,
            &mut stored_hashes,
            &mut stored_total,
        )?;
        let summary = summary_by_uid.get(uid);
        messages.push(NewCollectedEmailMessage {
            mailbox_folder: "INBOX".to_string(),
            uid: i64::from(*uid),
            message_id_sha256: extracted
                .message_id
                .as_deref()
                .map(|value| dedupe::sha256_hex(value.as_bytes())),
            sender: ledger_text(&extracted.from, 500),
            subject: ledger_text(&extracted.subject, 1_000),
            received_at: summary.map(|value| value.internal_date.clone()),
            status: analysis.status,
            error_category: analysis.has_failure.then(|| {
                if review_failed {
                    "review_snapshot_failed".to_string()
                } else {
                    "attachment_problem".to_string()
                }
            }),
            review: review_content.map(new_review_snapshot),
            attachments: analysis.attachments,
        });
    }

    session
        .verify_read_only_unchanged("INBOX")
        .map_err(|error| AppError::network(format!("邮箱只读 FLAGS 复核失败: {error}")))?;
    emit_progress(
        app,
        task.id,
        uids.len(),
        uids.len(),
        "邮件只读收集完成，等待来源审核",
    );
    Ok(messages)
}

struct CollectedAttachmentAnalysis {
    status: String,
    has_failure: bool,
    attachments: Vec<NewCollectedEmailAttachment>,
}

#[allow(clippy::too_many_arguments)]
fn analyze_collection_attachments(
    extracted: &extract::ExtractedEmail,
    review_has_links: bool,
    review_failed: bool,
    material_dir: &Path,
    existing_library_bytes: u64,
    stored_hashes: &mut HashSet<String>,
    stored_total: &mut u64,
) -> AppResult<CollectedAttachmentAnalysis> {
    let mut attachments = Vec::new();
    let mut has_main_candidate = false;
    let mut has_supporting_candidate = false;
    let mut has_unknown_candidate = false;
    let mut has_qr_attachment = false;
    let mut has_failure = review_failed;
    for attachment in &extracted.attachments {
        let expanded = extract::extract_zip_if_needed(attachment);
        if expanded.is_empty() {
            has_failure = true;
            attachments.push(NewCollectedEmailAttachment {
                content_sha256: Some(dedupe::sha256_hex(&attachment.data)),
                original_name: ledger_text(&attachment.filename, 500),
                container_name: None,
                mime_type: Some(ledger_text(&attachment.content_type, 200)),
                byte_len: i64::try_from(attachment.data.len()).unwrap_or(i64::MAX),
                status: "failed".to_string(),
                role_hint: "unknown".to_string(),
                reason: "archive_invalid_or_unsafe".to_string(),
                stored_path: None,
                manual_import: false,
            });
            continue;
        }
        for item in expanded {
            let hash = dedupe::sha256_hex(&item.data);
            let container_name = attachment
                .filename
                .to_ascii_lowercase()
                .ends_with(".zip")
                .then(|| ledger_text(&attachment.filename, 500));
            let role = role_hint(&item.filename, &extracted.subject);
            let item_bytes = item.data.len() as u64;
            if item_bytes == 0 || item_bytes > crate::local_source::MAX_FILE_BYTES {
                attachments.push(NewCollectedEmailAttachment {
                    content_sha256: Some(hash),
                    original_name: ledger_text(&item.filename, 500),
                    container_name,
                    mime_type: Some(ledger_text(&item.content_type, 200)),
                    byte_len: i64::try_from(item.data.len()).unwrap_or(i64::MAX),
                    status: "unsupported".to_string(),
                    role_hint: role.to_string(),
                    reason: if item_bytes == 0 {
                        "empty_attachment"
                    } else {
                        "attachment_too_large"
                    }
                    .to_string(),
                    stored_path: None,
                    manual_import: false,
                });
                continue;
            }
            let structure = classify::validate_attachment_structure(&item);
            let detected_mime = match structure {
                classify::AttachmentStructure::Valid { detected_mime } => Some(detected_mime),
                classify::AttachmentStructure::Invalid { reason } => {
                    has_failure = true;
                    let stored_path = persist_collection_item(
                        material_dir,
                        existing_library_bytes,
                        stored_hashes,
                        stored_total,
                        &hash,
                        &item,
                    )?;
                    attachments.push(NewCollectedEmailAttachment {
                        content_sha256: Some(hash),
                        original_name: ledger_text(&item.filename, 500),
                        container_name,
                        mime_type: Some(ledger_text(&item.content_type, 200)),
                        byte_len: i64::try_from(item.data.len()).unwrap_or(i64::MAX),
                        status: "failed".to_string(),
                        role_hint: role.to_string(),
                        reason: reason.to_string(),
                        stored_path,
                        manual_import: false,
                    });
                    continue;
                }
                classify::AttachmentStructure::Unsupported => None,
            };
            let classification = classify::classify_attachment(extracted, &item);
            let qr_analysis = extract::analyze_qr_attachment(&item);
            has_qr_attachment |= qr_analysis.detected;
            let is_download_instruction = qr_analysis.qr_dominant;
            let is_known_supporting =
                detected_mime.is_some() && role != "invoice" && role != "unknown";
            if classification.is_none() && !is_known_supporting {
                let stored_path = if detected_mime.is_some() {
                    persist_collection_item(
                        material_dir,
                        existing_library_bytes,
                        stored_hashes,
                        stored_total,
                        &hash,
                        &item,
                    )?
                } else {
                    None
                };
                attachments.push(NewCollectedEmailAttachment {
                    content_sha256: Some(hash),
                    original_name: ledger_text(&item.filename, 500),
                    container_name,
                    mime_type: Some(ledger_text(
                        detected_mime.unwrap_or(&item.content_type),
                        200,
                    )),
                    byte_len: i64::try_from(item.data.len()).unwrap_or(i64::MAX),
                    status: "filtered".to_string(),
                    role_hint: role.to_string(),
                    reason: if qr_analysis.qr_dominant {
                        "attachment_qr_manual_download"
                    } else if qr_analysis.detected {
                        "attachment_contains_qr_link"
                    } else {
                        "source_classifier_rejected"
                    }
                    .to_string(),
                    stored_path,
                    manual_import: false,
                });
                continue;
            }
            if !stored_hashes.contains(&hash) {
                let next_total = match crate::local_source::checked_staged_total(
                    stored_hashes.len(),
                    *stored_total,
                    item_bytes,
                ) {
                    Ok(total) => total,
                    Err(_) => {
                        has_failure = true;
                        attachments.push(NewCollectedEmailAttachment {
                            content_sha256: Some(hash),
                            original_name: ledger_text(&item.filename, 500),
                            container_name,
                            mime_type: Some(ledger_text(&item.content_type, 200)),
                            byte_len: i64::try_from(item.data.len()).unwrap_or(i64::MAX),
                            status: "unsupported".to_string(),
                            role_hint: role.to_string(),
                            reason: "collection_size_limit".to_string(),
                            stored_path: None,
                            manual_import: false,
                        });
                        continue;
                    }
                };
                if existing_library_bytes.saturating_add(next_total) > MAX_COLLECTION_LIBRARY_BYTES
                {
                    has_failure = true;
                    attachments.push(NewCollectedEmailAttachment {
                        content_sha256: Some(hash),
                        original_name: ledger_text(&item.filename, 500),
                        container_name,
                        mime_type: Some(ledger_text(&item.content_type, 200)),
                        byte_len: i64::try_from(item.data.len()).unwrap_or(i64::MAX),
                        status: "unsupported".to_string(),
                        role_hint: role.to_string(),
                        reason: "collection_library_limit".to_string(),
                        stored_path: None,
                        manual_import: false,
                    });
                    continue;
                }
                stored_hashes.insert(hash.clone());
                *stored_total = next_total;
            }
            let stored_path = persist_material(material_dir, &hash, &item.filename, &item.data)?;
            let is_main = role == "invoice" && !is_download_instruction;
            let is_unknown = role == "unknown" && !is_download_instruction;
            has_main_candidate |= is_main;
            has_unknown_candidate |= is_unknown;
            has_supporting_candidate |= !is_main && !is_unknown && !is_download_instruction;
            attachments.push(NewCollectedEmailAttachment {
                content_sha256: Some(hash),
                original_name: ledger_text(&item.filename, 500),
                container_name,
                mime_type: Some(ledger_text(
                    detected_mime.unwrap_or(&item.content_type),
                    200,
                )),
                byte_len: i64::try_from(item.data.len()).unwrap_or(i64::MAX),
                status: if is_main || is_unknown {
                    "candidate"
                } else if is_download_instruction {
                    "filtered"
                } else {
                    "supporting_candidate"
                }
                .to_string(),
                role_hint: if is_download_instruction {
                    "supporting".to_string()
                } else {
                    role.to_string()
                },
                reason: if is_download_instruction {
                    "attachment_qr_manual_download".to_string()
                } else if qr_analysis.detected {
                    "attachment_contains_qr_link".to_string()
                } else {
                    classification
                        .map(|value| classification_reason(value.reason).to_string())
                        .unwrap_or_else(|| "supporting_material_keyword".to_string())
                },
                stored_path: Some(collection_path_locator(&stored_path)?),
                manual_import: false,
            });
        }
    }
    let status = if has_failure {
        "failed"
    } else if has_main_candidate {
        "has_candidates"
    } else if review_has_links || extracted.invoice_link_hint {
        "manual_download"
    } else if has_qr_attachment || has_unknown_candidate {
        "needs_confirmation"
    } else if has_supporting_candidate {
        "materials_only"
    } else if extracted.invoice_notice_hint {
        "needs_confirmation"
    } else {
        "not_relevant"
    };
    Ok(CollectedAttachmentAnalysis {
        status: status.to_string(),
        has_failure,
        attachments,
    })
}

fn failed_message(
    uid: u32,
    sender: Option<&str>,
    subject: Option<&str>,
    received_at: Option<String>,
    category: &str,
) -> NewCollectedEmailMessage {
    NewCollectedEmailMessage {
        mailbox_folder: "INBOX".to_string(),
        uid: i64::from(uid),
        message_id_sha256: None,
        sender: ledger_text(sender.unwrap_or("(未知发件人)"), 500),
        subject: ledger_text(subject.unwrap_or("(无主题)"), 1_000),
        received_at,
        status: "failed".to_string(),
        error_category: Some(category.to_string()),
        review: None,
        attachments: Vec::new(),
    }
}

fn collection_material_dir(task_id: i64) -> AppResult<PathBuf> {
    validate_positive_id(task_id, "邮件收集任务")?;
    let root = crate::paths::data_root()
        .map_err(|error| AppError::io(format!("无法定位数据目录: {error}")))?;
    let directory = root
        .join("collection-files")
        .join(format!("task-{task_id}"));
    fs::create_dir_all(&directory)?;
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::validation("邮件材料目录必须是普通本地目录"));
    }
    Ok(directory)
}

fn stage_collection_task_materials(task_id: i64) -> AppResult<Option<(PathBuf, PathBuf)>> {
    let data_root = crate::paths::data_root()
        .map_err(|error| AppError::io(format!("无法定位数据目录: {error}")))?;
    let library = data_root.join("collection-files");
    if !library.exists() {
        return Ok(None);
    }
    let library_metadata = fs::symlink_metadata(&library)?;
    if !library_metadata.is_dir() || is_reparse_point(&library_metadata) {
        return Err(AppError::validation("邮件材料库必须是普通本地目录"));
    }
    let target = library.join(format!("task-{task_id}"));
    if !target.exists() {
        return Ok(None);
    }
    let target_metadata = fs::symlink_metadata(&target)?;
    if !target_metadata.is_dir() || is_reparse_point(&target_metadata) {
        return Err(AppError::validation(
            "邮件收集任务材料目录不安全，未执行删除",
        ));
    }
    let canonical_library =
        fs::canonicalize(&library).map_err(|_| AppError::validation("邮件材料库路径无效"))?;
    let canonical_target =
        fs::canonicalize(&target).map_err(|_| AppError::validation("邮件收集任务材料路径无效"))?;
    if canonical_target.parent() != Some(canonical_library.as_path()) {
        return Err(AppError::validation("邮件收集任务材料目录超出受控范围"));
    }
    let staged =
        canonical_library.join(format!(".deleting-task-{task_id}-{}", uuid::Uuid::new_v4()));
    fs::rename(&canonical_target, &staged)?;
    Ok(Some((canonical_target, staged)))
}

fn collection_library_bytes(root: &Path) -> AppResult<u64> {
    if !root.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    let mut entries = 0usize;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let directory_metadata = fs::symlink_metadata(&directory)?;
        if !directory_metadata.is_dir() || is_reparse_point(&directory_metadata) {
            return Err(AppError::validation("邮件材料库包含不安全的链接目录"));
        }
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if is_reparse_point(&metadata) {
                return Err(AppError::validation("邮件材料库包含不安全的链接文件"));
            }
            entries = entries
                .checked_add(1)
                .ok_or_else(|| AppError::validation("邮件材料库条目数量溢出"))?;
            if entries > MAX_COLLECTION_LIBRARY_ENTRIES {
                return Err(AppError::validation(
                    "邮件材料库条目超过 100000 个，请先备份并清理",
                ));
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                total = total
                    .checked_add(metadata.len())
                    .ok_or_else(|| AppError::validation("邮件材料库大小溢出"))?;
                if total > MAX_COLLECTION_LIBRARY_BYTES {
                    return Ok(total);
                }
            } else {
                return Err(AppError::validation("邮件材料库包含不支持的文件类型"));
            }
        }
    }
    Ok(total)
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(target_os = "windows"))]
    {
        metadata.file_type().is_symlink()
    }
}

fn persist_material(
    directory: &Path,
    hash: &str,
    original_name: &str,
    bytes: &[u8],
) -> AppResult<PathBuf> {
    let safe_name = sanitize_filename(original_name);
    let target = directory.join(format!("{hash}-{safe_name}"));
    if target.exists() {
        let metadata = fs::symlink_metadata(&target)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || fs::read(&target)? != bytes {
            return Err(AppError::validation("邮件材料库存在内容哈希冲突"));
        }
        return Ok(target);
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    Ok(target)
}

fn persist_collection_item(
    directory: &Path,
    existing_library_bytes: u64,
    stored_hashes: &mut HashSet<String>,
    stored_total: &mut u64,
    hash: &str,
    item: &extract::RawAttachment,
) -> AppResult<Option<String>> {
    if !stored_hashes.contains(hash) {
        let Ok(next_total) = crate::local_source::checked_staged_total(
            stored_hashes.len(),
            *stored_total,
            item.data.len() as u64,
        ) else {
            return Ok(None);
        };
        if existing_library_bytes.saturating_add(next_total) > MAX_COLLECTION_LIBRARY_BYTES {
            return Ok(None);
        }
        stored_hashes.insert(hash.to_string());
        *stored_total = next_total;
    }
    let path = persist_material(directory, hash, &item.filename, &item.data)?;
    Ok(Some(collection_path_locator(&path)?))
}

/// Store paths relative to the product data root so an approved backup remains usable after it is
/// restored on another Windows computer. The parser resolves and revalidates this locator before
/// every batch import.
fn collection_path_locator(path: &Path) -> AppResult<String> {
    let data_root = crate::paths::data_root()
        .map_err(|error| AppError::io(format!("无法定位数据目录: {error}")))?;
    let relative = path
        .strip_prefix(&data_root)
        .map_err(|_| AppError::validation("邮件材料不在应用数据目录内"))?;
    if !relative.starts_with("collection-files")
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(AppError::validation("邮件材料定位符无效"));
    }
    Ok(relative.to_string_lossy().to_string())
}

fn sanitize_filename(name: &str) -> String {
    const MAX_COLLECTION_SOURCE_NAME_CHARS: usize = 80;
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
    let stem_budget = MAX_COLLECTION_SOURCE_NAME_CHARS
        .saturating_sub(suffix.chars().count())
        .max(1);
    let value: String = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("invoice")
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(stem_budget)
        .collect();
    let value = value.trim_matches(['.', ' ']);
    if value.is_empty() {
        format!("invoice{suffix}")
    } else {
        format!("{value}{suffix}")
    }
}

fn staged_file_identity(path: &Path) -> AppResult<(String, String)> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AppError::validation("补充材料文件名无效"))?;
    let (hash, original_name) = name
        .split_once('-')
        .ok_or_else(|| AppError::validation("补充材料暂存文件名无效"))?;
    if hash.len() != 64 || !hash.chars().all(|value| value.is_ascii_hexdigit()) {
        return Err(AppError::validation("补充材料内容哈希无效"));
    }
    Ok((hash.to_string(), original_name.to_string()))
}

fn role_hint(filename: &str, subject: &str) -> &'static str {
    let lower = format!("{filename} {subject}").to_ascii_lowercase();
    if lower.contains("行程单")
        || lower.contains("行程报销单")
        || lower.contains("行程明细")
        || lower.contains("itinerary")
    {
        "itinerary"
    } else if lower.contains("informationfolio")
        || lower.contains("checkoutbill")
        || lower.contains("folio")
        || lower.contains("结账单")
        || lower.contains("账单")
        || lower.contains("明细")
        || lower.contains("结算单")
        || lower.contains("detail")
    {
        "detail"
    } else if lower.contains("发票")
        || lower.contains("电子票据")
        || lower.contains("财政票据")
        || lower.contains("invoice")
        || lower.contains("报销凭证")
    {
        "invoice"
    } else if lower.contains("附件") || lower.contains("support") {
        "supporting"
    } else {
        "unknown"
    }
}

fn classification_reason(reason: classify::MatchReason) -> &'static str {
    match reason {
        classify::MatchReason::SenderWhitelist => "trusted_sender_candidate",
        classify::MatchReason::AttachmentFeature => "filename_or_subject_candidate",
        classify::MatchReason::SupportedDocumentContent => "supported_content_candidate",
    }
}

fn mime_type_for_path(path: &Path) -> Option<&'static str> {
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

fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "tif" | "tiff"
    )
}

fn ledger_text(value: &str, limit: usize) -> String {
    let filtered: String = value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\t' | '\n'))
        .take(limit)
        .collect();
    let trimmed = filtered.trim();
    if trimmed.is_empty() {
        "(空)".to_string()
    } else {
        trimmed.to_string()
    }
}

fn emit_progress(app: &AppHandle, task_id: i64, current: usize, total: usize, message: &str) {
    let _ = app.emit(
        &format!("email-collection:progress:{task_id}"),
        CollectionProgress {
            task_id,
            current,
            total,
            message: message.to_string(),
        },
    );
}

fn record_collection_failure(app: &AppHandle, task_id: i64, error: &AppError) {
    if let Ok(app_state) = app.state::<Mutex<AppState>>().lock() {
        if let Ok(db) = app_state.ledger_db() {
            let _ = db.mark_email_collection_failed(task_id, "collection_failed");
        }
    }
    let _ = app.emit(
        &format!("email-collection:error:{task_id}"),
        CollectionFailed {
            task_id,
            message: error.message().to_string(),
        },
    );
    tracing::error!(task_id, error_kind = ?error.kind(), "独立邮件收集任务失败");
}

fn map_store_error(prefix: &str, error: invoice_store::StoreError) -> AppError {
    match error {
        invoice_store::StoreError::Validation(message)
        | invoice_store::StoreError::NotFound(message) => AppError::validation(message),
        other => AppError::database(format!("{prefix}: {other}")),
    }
}

fn validate_positive_id(value: i64, label: &str) -> AppResult<()> {
    if value <= 0 {
        Err(AppError::validation(format!("{label}标识无效")))
    } else {
        Ok(())
    }
}
