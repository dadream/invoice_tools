//! User-confirmed Concur receipt email workflow.
//!
//! Credentials stay in `AppState` memory. The reviewed attachment plan and delivery state are
//! persisted in ledger.db so a crash cannot silently turn an ambiguous delivery into a retry.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use invoice_store::models::{
    ConcurReserveOutcome, ConcurSendItem, ConcurSendSession, NewConcurSendItem, ReportedInvoice,
    TicketType,
};
use invoice_store::{LedgerDb, StoreError};
use lettre::Address;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::State;

use crate::concur_smtp::{
    self, DeliveryErrorKind, ReceiptAttachment, ReceiptMessage, MAX_ATTACHMENTS_PER_MESSAGE,
    MAX_ATTACHMENT_BYTES, MAX_MESSAGE_ATTACHMENT_BYTES,
};
use crate::error::{AppError, AppResult};
use crate::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConcurCapabilityDto {
    pub enabled: bool,
    pub manual_send_only: bool,
    pub max_attachments_per_message: usize,
    pub max_attachment_mib: usize,
    pub max_message_attachment_mib: usize,
    pub supported_formats: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConcurSessionDto {
    pub batch_id: i64,
    pub sender_email: String,
    pub recipient_email: String,
    pub trial_invoice_id: i64,
    pub trial_status: String,
    pub confirmed_behavior: Option<String>,
    pub confirmed_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConcurItemDto {
    pub invoice_id: i64,
    pub attachment_name: String,
    pub attachment_bytes: Option<u64>,
    pub status: String,
    pub attempt_count: i64,
    pub last_error: Option<String>,
    pub sent_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConcurStatusDto {
    pub enabled: bool,
    pub session: Option<ConcurSessionDto>,
    pub items: Vec<ConcurItemDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConcurSendResultDto {
    pub outcome: String,
    pub sent_count: usize,
    pub failed_count: usize,
    pub unknown_count: usize,
    pub skipped_count: usize,
    pub message_ids: Vec<String>,
    pub message: String,
}

struct LoadedDelivery {
    request: ReceiptMessage,
    invoice_ids: Vec<i64>,
}

#[tauri::command]
pub fn get_concur_capability() -> ConcurCapabilityDto {
    ConcurCapabilityDto {
        enabled: concur_smtp::is_send_enabled(),
        manual_send_only: true,
        max_attachments_per_message: MAX_ATTACHMENTS_PER_MESSAGE,
        max_attachment_mib: MAX_ATTACHMENT_BYTES / 1024 / 1024,
        max_message_attachment_mib: MAX_MESSAGE_ATTACHMENT_BYTES / 1024 / 1024,
        supported_formats: vec!["PDF", "PNG", "JPG/JPEG", "TIF/TIFF"],
    }
}

#[tauri::command]
pub fn get_concur_send_status(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<ConcurStatusDto> {
    let app_state = lock_state(&state)?;
    status_from_db(app_state.ledger_db()?, batch_id)
}

#[tauri::command]
pub fn prepare_concur_send(
    batch_id: i64,
    recipient_email: String,
    trial_invoice_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<ConcurStatusDto> {
    let recipient_email = validate_plain_email(&recipient_email, "Concur 收件地址")?;
    let app_state = lock_state(&state)?;
    let sender_email = app_state
        .session_email()
        .ok_or_else(|| AppError::validation("请先在设置中输入本次会话的邮箱授权码"))?
        .to_string();
    validate_plain_email(&sender_email, "发件邮箱")?;
    let db = app_state.ledger_db()?;
    let invoices = db
        .list_reimbursable_invoices_by_batch(batch_id)
        .map_err(map_store_error)?;
    if invoices.is_empty() {
        return Err(AppError::validation("批次没有可发送的已审核收据"));
    }
    if !invoices
        .iter()
        .any(|invoice| invoice.id == trial_invoice_id)
    {
        return Err(AppError::validation("试发收据不属于当前可报销批次"));
    }

    let mut items = Vec::with_capacity(invoices.len());
    for invoice in &invoices {
        let descriptor = describe_attachment(invoice)?;
        let idempotency_key =
            receipt_idempotency_key(&sender_email, &recipient_email, invoice, &descriptor.sha256);
        items.push(NewConcurSendItem {
            invoice_id: invoice.id,
            idempotency_key,
            attachment_name: descriptor.name,
            attachment_sha256: descriptor.sha256,
        });
    }
    db.initialize_concur_send_session(
        batch_id,
        &sender_email,
        &recipient_email,
        trial_invoice_id,
        &items,
    )
    .map_err(map_store_error)?;
    tracing::info!(
        batch_id,
        receipt_count = items.len(),
        "Concur reviewed send plan initialized; no network request was made"
    );
    status_from_db(db, batch_id)
}

#[tauri::command]
pub async fn send_concur_trial(
    batch_id: i64,
    user_confirmed: bool,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<ConcurSendResultDto> {
    require_real_send(user_confirmed)?;
    let (delivery, password) = {
        let app_state = lock_state(&state)?;
        let db = app_state.ledger_db()?;
        let session = require_session(db, batch_id)?;
        let (session_email, password) = require_matching_credential(&app_state, &session)?;
        let items = db
            .list_concur_send_items(batch_id)
            .map_err(map_store_error)?;
        let trial_item = items
            .iter()
            .find(|item| item.invoice_id == session.trial_invoice_id)
            .ok_or_else(|| AppError::validation("试发收据状态缺失"))?;
        if trial_item.status == "sent" || session.trial_status == "confirmed" {
            return Ok(ConcurSendResultDto {
                outcome: "skipped".to_string(),
                sent_count: 0,
                failed_count: 0,
                unknown_count: 0,
                skipped_count: 1,
                message_ids: Vec::new(),
                message: "试发收据已成功发送，不会重复发送".to_string(),
            });
        }
        let delivery = load_delivery(db, &session, std::slice::from_ref(trial_item), true)?;
        match db
            .reserve_concur_send_item(batch_id, session.trial_invoice_id, true)
            .map_err(map_store_error)?
        {
            ConcurReserveOutcome::Reserved(_) => {}
            ConcurReserveOutcome::AlreadySent(_) => {
                return Ok(ConcurSendResultDto {
                    outcome: "skipped".to_string(),
                    sent_count: 0,
                    failed_count: 0,
                    unknown_count: 0,
                    skipped_count: 1,
                    message_ids: Vec::new(),
                    message: "试发收据已成功发送，不会重复发送".to_string(),
                });
            }
            ConcurReserveOutcome::InProgress => {
                return Err(AppError::validation("试发正在进行，请勿重复点击"));
            }
        }
        debug_assert_eq!(session_email, session.sender_email);
        (delivery, password)
    };

    let LoadedDelivery {
        request,
        invoice_ids,
    } = delivery;
    let result = transmit(request, password).await?;
    finish_delivery(&state, batch_id, &invoice_ids, result)
}

#[tauri::command]
pub fn confirm_concur_trial(
    batch_id: i64,
    behavior: String,
    state: State<Mutex<AppState>>,
) -> AppResult<ConcurStatusDto> {
    let app_state = lock_state(&state)?;
    let db = app_state.ledger_db()?;
    db.confirm_concur_trial(batch_id, &behavior)
        .map_err(map_store_error)?;
    tracing::info!(batch_id, behavior = %behavior, "Concur trial behavior confirmed by user");
    status_from_db(db, batch_id)
}

#[tauri::command]
pub async fn send_concur_remaining(
    batch_id: i64,
    user_confirmed: bool,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<ConcurSendResultDto> {
    require_real_send(user_confirmed)?;
    let pending_ids = {
        let app_state = lock_state(&state)?;
        let db = app_state.ledger_db()?;
        let session = require_session(db, batch_id)?;
        if session.trial_status != "confirmed" {
            return Err(AppError::validation(
                "请先完成一张试发并确认 Concur 中的结果",
            ));
        }
        let items = db
            .list_concur_send_items(batch_id)
            .map_err(map_store_error)?;
        if items.iter().any(|item| item.status == "unknown") {
            return Err(AppError::validation(
                "存在送达结果未知的收据，请先在 Concur 核对并逐项标记",
            ));
        }
        if items.iter().any(|item| item.status == "sending") {
            return Err(AppError::validation("已有 Concur 发送正在进行"));
        }
        items
            .into_iter()
            .filter(|item| matches!(item.status.as_str(), "pending" | "failed"))
            .map(|item| item.invoice_id)
            .collect::<Vec<_>>()
    };
    if pending_ids.is_empty() {
        return Ok(ConcurSendResultDto {
            outcome: "complete".to_string(),
            sent_count: 0,
            failed_count: 0,
            unknown_count: 0,
            skipped_count: 0,
            message_ids: Vec::new(),
            message: "没有待发送或可重试的收据".to_string(),
        });
    }

    let groups = plan_groups(&state, &pending_ids)?;
    let mut summary = ConcurSendResultDto {
        outcome: "complete".to_string(),
        sent_count: 0,
        failed_count: 0,
        unknown_count: 0,
        skipped_count: 0,
        message_ids: Vec::new(),
        message: String::new(),
    };
    for group in groups {
        let (delivery, password) = {
            let app_state = lock_state(&state)?;
            let db = app_state.ledger_db()?;
            let session = require_session(db, batch_id)?;
            let (_, password) = require_matching_credential(&app_state, &session)?;
            let all_items = db
                .list_concur_send_items(batch_id)
                .map_err(map_store_error)?;
            let by_id = all_items
                .into_iter()
                .map(|item| (item.invoice_id, item))
                .collect::<HashMap<_, _>>();
            let selected = group
                .iter()
                .map(|invoice_id| {
                    by_id
                        .get(invoice_id)
                        .cloned()
                        .ok_or_else(|| AppError::validation("Concur 批量发送项目在执行前发生变化"))
                })
                .collect::<AppResult<Vec<_>>>()?;
            let delivery = load_delivery(db, &session, &selected, false)?;
            db.reserve_concur_send_group(batch_id, &group)
                .map_err(map_store_error)?;
            (delivery, password)
        };
        let LoadedDelivery {
            request,
            invoice_ids,
        } = delivery;
        let result = transmit(request, password).await?;
        let group_result = finish_delivery(&state, batch_id, &invoice_ids, result)?;
        summary.sent_count += group_result.sent_count;
        summary.failed_count += group_result.failed_count;
        summary.unknown_count += group_result.unknown_count;
        summary.skipped_count += group_result.skipped_count;
        summary.message_ids.extend(group_result.message_ids);
        if group_result.failed_count > 0 || group_result.unknown_count > 0 {
            summary.outcome = group_result.outcome;
            summary.message = group_result.message;
            break;
        }
    }
    if summary.message.is_empty() {
        summary.message = format!(
            "已发送 {} 张收据；成功项不会在重试中重复发送",
            summary.sent_count
        );
    }
    Ok(summary)
}

#[tauri::command]
pub fn resolve_concur_unknown(
    batch_id: i64,
    invoice_id: i64,
    delivered: bool,
    state: State<Mutex<AppState>>,
) -> AppResult<ConcurStatusDto> {
    let app_state = lock_state(&state)?;
    let db = app_state.ledger_db()?;
    db.resolve_concur_unknown_item(batch_id, invoice_id, delivered)
        .map_err(map_store_error)?;
    tracing::info!(
        batch_id,
        invoice_id,
        delivered,
        "Concur unknown delivery resolved by user"
    );
    status_from_db(db, batch_id)
}

fn require_real_send(user_confirmed: bool) -> AppResult<()> {
    if !concur_smtp::is_send_enabled() {
        return Err(AppError::validation(
            "当前内部 Alpha 构建未启用 Concur 真实发送；不会连接 SMTP",
        ));
    }
    if !user_confirmed {
        return Err(AppError::validation("必须明确确认本次发送后才能连接 SMTP"));
    }
    Ok(())
}

fn lock_state<'a>(state: &'a Mutex<AppState>) -> AppResult<MutexGuard<'a, AppState>> {
    state
        .lock()
        .map_err(|_| AppError::internal("应用状态锁不可用"))
}

fn require_session(db: &LedgerDb, batch_id: i64) -> AppResult<ConcurSendSession> {
    db.get_concur_send_session(batch_id)
        .map_err(map_store_error)?
        .ok_or_else(|| AppError::validation("请先建立 Concur 试发计划"))
}

fn require_matching_credential(
    app_state: &AppState,
    session: &ConcurSendSession,
) -> AppResult<(String, zeroize::Zeroizing<String>)> {
    let (email, password) = app_state
        .session_credential_copy()
        .ok_or_else(|| AppError::validation("邮箱授权码已失效，请在设置中重新输入"))?;
    if !email.eq_ignore_ascii_case(&session.sender_email) {
        return Err(AppError::validation(
            "当前会话邮箱与已审核的 Concur 发件邮箱不一致",
        ));
    }
    Ok((email, password))
}

async fn transmit(
    request: ReceiptMessage,
    password: zeroize::Zeroizing<String>,
) -> AppResult<Result<String, concur_smtp::DeliveryError>> {
    tauri::async_runtime::spawn_blocking(move || {
        concur_smtp::send_receipt_message(&request, password.as_str())
    })
    .await
    .map_err(|_| AppError::internal("SMTP 工作线程异常终止"))
}

fn finish_delivery(
    state: &Mutex<AppState>,
    batch_id: i64,
    invoice_ids: &[i64],
    result: Result<String, concur_smtp::DeliveryError>,
) -> AppResult<ConcurSendResultDto> {
    let app_state = lock_state(state)?;
    let db = app_state.ledger_db()?;
    match result {
        Ok(message_id) => {
            db.mark_concur_items_sent(batch_id, invoice_ids, &message_id)
                .map_err(map_store_error)?;
            tracing::info!(
                batch_id,
                receipt_count = invoice_ids.len(),
                "Concur SMTP relay accepted receipt message"
            );
            Ok(ConcurSendResultDto {
                outcome: "sent".to_string(),
                sent_count: invoice_ids.len(),
                failed_count: 0,
                unknown_count: 0,
                skipped_count: 0,
                message_ids: vec![message_id],
                message: "SMTP 已接受邮件；请到 Concur 核对收据处理结果".to_string(),
            })
        }
        Err(error) if error.kind == DeliveryErrorKind::BeforeSend => {
            let reason = error.to_string();
            db.mark_concur_items_failed(batch_id, invoice_ids, &reason)
                .map_err(map_store_error)?;
            Ok(ConcurSendResultDto {
                outcome: "failed".to_string(),
                sent_count: 0,
                failed_count: invoice_ids.len(),
                unknown_count: 0,
                skipped_count: 0,
                message_ids: Vec::new(),
                message: reason,
            })
        }
        Err(error) => {
            let reason = error.to_string();
            db.mark_concur_items_unknown(batch_id, invoice_ids, &reason)
                .map_err(map_store_error)?;
            Ok(ConcurSendResultDto {
                outcome: "unknown".to_string(),
                sent_count: 0,
                failed_count: 0,
                unknown_count: invoice_ids.len(),
                skipped_count: 0,
                message_ids: Vec::new(),
                message: reason,
            })
        }
    }
}

fn load_delivery(
    db: &LedgerDb,
    session: &ConcurSendSession,
    items: &[ConcurSendItem],
    is_trial: bool,
) -> AppResult<LoadedDelivery> {
    if items.is_empty() || items.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(AppError::validation("单封邮件的收据数量必须为 1 至 5 张"));
    }
    let mut attachments = Vec::with_capacity(items.len());
    let mut invoice_ids = Vec::with_capacity(items.len());
    let mut keys = Vec::with_capacity(items.len());
    for item in items {
        let invoice = db
            .get_invoice(item.invoice_id)
            .map_err(map_store_error)?
            .ok_or_else(|| AppError::validation("发送计划中的发票已不存在"))?;
        attachments.push(load_reviewed_attachment(&invoice, item)?);
        invoice_ids.push(item.invoice_id);
        keys.push(item.idempotency_key.as_str());
    }
    let message_id = group_message_id(&keys);
    Ok(LoadedDelivery {
        request: ReceiptMessage {
            sender_email: session.sender_email.clone(),
            recipient_email: session.recipient_email.clone(),
            message_id,
            is_trial,
            attachments,
        },
        invoice_ids,
    })
}

fn plan_groups(state: &Mutex<AppState>, invoice_ids: &[i64]) -> AppResult<Vec<Vec<i64>>> {
    let app_state = lock_state(state)?;
    let db = app_state.ledger_db()?;
    let mut groups: Vec<Vec<i64>> = Vec::new();
    let mut current: Vec<i64> = Vec::new();
    let mut current_bytes = 0usize;
    for invoice_id in invoice_ids {
        let invoice = db
            .get_invoice(*invoice_id)
            .map_err(map_store_error)?
            .ok_or_else(|| AppError::validation("发送计划中的发票已不存在"))?;
        let bytes = checked_file_size(Path::new(&invoice.file_path))?;
        if !current.is_empty()
            && (current.len() == MAX_ATTACHMENTS_PER_MESSAGE
                || current_bytes + bytes > MAX_MESSAGE_ATTACHMENT_BYTES)
        {
            groups.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current.push(*invoice_id);
        current_bytes += bytes;
    }
    if !current.is_empty() {
        groups.push(current);
    }
    Ok(groups)
}

struct AttachmentDescriptor {
    name: String,
    sha256: String,
    mime_type: &'static str,
    bytes: usize,
}

fn describe_attachment(invoice: &ReportedInvoice) -> AppResult<AttachmentDescriptor> {
    let path = Path::new(&invoice.file_path);
    let bytes = checked_file_size(path)?;
    let mime_type = supported_mime(path)?;
    let sha256 = hash_file(path, bytes)?;
    let name = stable_attachment_name(invoice, path, &sha256)?;
    Ok(AttachmentDescriptor {
        name,
        sha256,
        mime_type,
        bytes,
    })
}

fn load_reviewed_attachment(
    invoice: &ReportedInvoice,
    item: &ConcurSendItem,
) -> AppResult<ReceiptAttachment> {
    let path = Path::new(&invoice.file_path);
    let descriptor = describe_attachment(invoice)?;
    if descriptor.name != item.attachment_name
        || !descriptor
            .sha256
            .eq_ignore_ascii_case(&item.attachment_sha256)
    {
        return Err(AppError::validation(
            "收据文件在审核后发生变化；已阻止发送，请重新建立计划",
        ));
    }
    let mut file = File::open(path).map_err(|_| AppError::io("无法读取已审核收据文件"))?;
    let mut bytes = Vec::with_capacity(descriptor.bytes);
    file.by_ref()
        .take((MAX_ATTACHMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| AppError::io("读取已审核收据文件失败"))?;
    if bytes.len() != descriptor.bytes {
        return Err(AppError::validation(
            "收据文件在读取期间发生变化，已阻止发送",
        ));
    }
    let read_sha256 = hex_upper(&Sha256::digest(&bytes));
    if !read_sha256.eq_ignore_ascii_case(&item.attachment_sha256) {
        return Err(AppError::validation(
            "收据文件在读取期间发生变化，已阻止发送",
        ));
    }
    Ok(ReceiptAttachment {
        name: descriptor.name,
        mime_type: descriptor.mime_type,
        bytes,
    })
}

fn checked_file_size(path: &Path) -> AppResult<usize> {
    let metadata =
        std::fs::metadata(path).map_err(|_| AppError::io("已审核收据文件不存在或无法读取"))?;
    if !metadata.is_file() {
        return Err(AppError::validation("收据路径不是普通文件"));
    }
    let bytes =
        usize::try_from(metadata.len()).map_err(|_| AppError::validation("收据文件过大"))?;
    if bytes == 0 || bytes > MAX_ATTACHMENT_BYTES {
        return Err(AppError::validation(
            "单张收据必须大于 0 字节且不超过 15 MiB",
        ));
    }
    Ok(bytes)
}

fn supported_mime(path: &Path) -> AppResult<&'static str> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("pdf") => Ok("application/pdf"),
        Some("png") => Ok("image/png"),
        Some("jpg" | "jpeg") => Ok("image/jpeg"),
        Some("tif" | "tiff") => Ok("image/tiff"),
        _ => Err(AppError::validation(
            "Concur 邮件仅支持 PDF、PNG、JPG/JPEG、TIF/TIFF 收据；XML/OFD 需先生成受支持的可视文件",
        )),
    }
}

fn hash_file(path: &Path, expected_bytes: usize) -> AppResult<String> {
    let mut file = File::open(path).map_err(|_| AppError::io("无法读取已审核收据文件"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0usize;
    let mut limited = file.by_ref().take((MAX_ATTACHMENT_BYTES + 1) as u64);
    loop {
        let read = limited
            .read(&mut buffer)
            .map_err(|_| AppError::io("计算收据哈希失败"))?;
        if read == 0 {
            break;
        }
        total += read;
        hasher.update(&buffer[..read]);
    }
    if total != expected_bytes {
        return Err(AppError::validation(
            "收据文件在读取期间发生变化，已阻止发送",
        ));
    }
    Ok(hex_upper(&hasher.finalize()))
}

fn stable_attachment_name(
    invoice: &ReportedInvoice,
    path: &Path,
    sha256: &str,
) -> AppResult<String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AppError::validation("收据文件缺少扩展名"))?
        .to_ascii_lowercase();
    let invoice_suffix = invoice
        .invoice_number
        .chars()
        .filter(|value| value.is_ascii_alphanumeric())
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    if invoice_suffix.is_empty() {
        return Err(AppError::validation("发票号码无法生成稳定附件名"));
    }
    Ok(format!(
        "{}_{}_{}_{}_{}.{}",
        invoice.issue_date.format("%Y-%m-%d"),
        ticket_type_slug(invoice.ticket_type),
        invoice.amount,
        invoice_suffix,
        &sha256[..8],
        extension
    ))
}

fn ticket_type_slug(ticket_type: TicketType) -> &'static str {
    match ticket_type {
        TicketType::Rail => "rail",
        TicketType::Flight => "flight",
        TicketType::Hotel => "hotel",
        TicketType::CityTransport => "city_transport",
        TicketType::Meal => "meal",
        TicketType::CourierLogistics => "courier_logistics",
        TicketType::Other => "other",
    }
}

fn receipt_idempotency_key(
    sender: &str,
    recipient: &str,
    invoice: &ReportedInvoice,
    attachment_sha256: &str,
) -> String {
    let mut hasher = Sha256::new();
    for value in [
        "invoice-assistant-concur-v1",
        &sender.to_ascii_lowercase(),
        &recipient.to_ascii_lowercase(),
        &invoice.invoice_number,
        attachment_sha256,
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    hex_lower(&hasher.finalize())
}

fn group_message_id(keys: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"invoice-assistant-concur-message-v1");
    for key in keys {
        hasher.update((key.len() as u64).to_be_bytes());
        hasher.update(key.as_bytes());
    }
    format!(
        "<invoice-assistant-{}@local.invalid>",
        hex_lower(&hasher.finalize())
    )
}

fn validate_plain_email(value: &str, label: &str) -> AppResult<String> {
    let trimmed = value.trim();
    if trimmed != value || trimmed.parse::<Address>().is_err() {
        return Err(AppError::validation(format!("{label}格式不正确")));
    }
    Ok(trimmed.to_ascii_lowercase())
}

fn status_from_db(db: &LedgerDb, batch_id: i64) -> AppResult<ConcurStatusDto> {
    let session = db
        .get_concur_send_session(batch_id)
        .map_err(map_store_error)?;
    let items = if session.is_some() {
        db.list_concur_send_items(batch_id)
            .map_err(map_store_error)?
            .into_iter()
            .map(|item| {
                let attachment_bytes = db
                    .get_invoice(item.invoice_id)
                    .ok()
                    .flatten()
                    .and_then(|invoice| std::fs::metadata(invoice.file_path).ok())
                    .map(|metadata| metadata.len());
                ConcurItemDto {
                    invoice_id: item.invoice_id,
                    attachment_name: item.attachment_name,
                    attachment_bytes,
                    status: item.status,
                    attempt_count: item.attempt_count,
                    last_error: item.last_error,
                    sent_at: item.sent_at,
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(ConcurStatusDto {
        enabled: concur_smtp::is_send_enabled(),
        session: session.map(|session| ConcurSessionDto {
            batch_id: session.batch_id,
            sender_email: session.sender_email,
            recipient_email: session.recipient_email,
            trial_invoice_id: session.trial_invoice_id,
            trial_status: session.trial_status,
            confirmed_behavior: session.confirmed_behavior,
            confirmed_at: session.confirmed_at,
        }),
        items,
    })
}

fn map_store_error(error: StoreError) -> AppError {
    match error {
        StoreError::Validation(message)
        | StoreError::NotFound(message)
        | StoreError::InvalidStateTransition {
            from: message,
            to: _,
        } => AppError::validation(message),
        StoreError::Io(_) => AppError::io("Concur 状态文件操作失败"),
        StoreError::Database(_) => AppError::database("Concur 发送状态数据库操作失败"),
        StoreError::Crypto(_) | StoreError::Keychain(_) | StoreError::Internal(_) => {
            AppError::internal("Concur 发送状态处理失败")
        }
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;
    use std::str::FromStr;

    use super::*;

    fn synthetic_invoice(path: &Path) -> ReportedInvoice {
        ReportedInvoice {
            id: 7,
            batch_id: 3,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("100.50").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Rail,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: path.to_string_lossy().into_owned(),
            created_at: chrono::Utc::now().naive_utc(),
            updated_at: chrono::Utc::now().naive_utc(),
            verification_result: Some("valid".to_string()),
            is_duplicate: false,
            duplicate_reason: None,
        }
    }

    #[test]
    fn attachment_descriptor_is_stable_and_supported_only() {
        let temp = tempfile::tempdir().unwrap();
        let pdf = temp.path().join("source.pdf");
        std::fs::write(&pdf, b"synthetic pdf").unwrap();
        let invoice = synthetic_invoice(&pdf);
        let first = describe_attachment(&invoice).unwrap();
        let second = describe_attachment(&invoice).unwrap();
        assert_eq!(first.name, second.name);
        assert_eq!(first.sha256, second.sha256);
        assert!(first.name.starts_with("2026-07-15_rail_100.50_567890_"));

        let xml = temp.path().join("source.xml");
        std::fs::write(&xml, b"synthetic xml").unwrap();
        assert!(describe_attachment(&synthetic_invoice(&xml)).is_err());
    }

    #[test]
    fn idempotency_and_message_ids_are_deterministic() {
        let temp = tempfile::tempdir().unwrap();
        let pdf = temp.path().join("source.pdf");
        std::fs::write(&pdf, b"synthetic pdf").unwrap();
        let invoice = synthetic_invoice(&pdf);
        let hash = describe_attachment(&invoice).unwrap().sha256;
        let key = receipt_idempotency_key(
            "Sender@Example.test",
            "Receipts@Concur.Example",
            &invoice,
            &hash,
        );
        assert_eq!(key.len(), 64);
        assert_eq!(group_message_id(&[&key]), group_message_id(&[&key]));
    }

    #[test]
    fn real_send_requires_build_gate_and_confirmation() {
        if !concur_smtp::is_send_enabled() {
            assert!(require_real_send(true)
                .unwrap_err()
                .to_string()
                .contains("未启用"));
        }
        assert!(require_real_send(false).is_err());
    }
}
