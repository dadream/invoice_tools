//! 草稿批次人工审核命令。
//!
//! 所有写操作由 `invoice-store` 在同一事务中写入前后快照，支持顺序撤销。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

use chrono::{NaiveDate, NaiveDateTime, Utc};
use invoice_grouping::types::{
    Ambiguity, AmbiguityKind, AmbiguityResolution, AmbiguityResolver, GroupingConfig,
    StationCityAlias, TripKind,
};
use invoice_grouping::{group_invoices, GROUPING_RULE_VERSION};
use invoice_parse::model::{
    ParseLevel, ParsedInvoice, TicketType as ParseTicketType, TransportDocumentKind,
};
use invoice_store::models::{
    BatchReviewSnapshot, ConcurMappingProfile, ConcurMappingProfileInput, ConcurUploadPreflight,
    ConcurUploadSession, ConcurUploadStatus, DeliveryTask, ExpenseCategoryDetection, ExpenseItem,
    ExpenseItemUpdate, ExpenseLocation, ExpenseTaxDetail, InvoiceDocument, InvoiceReviewUpdate,
    NewBatchGrouping, NewInvoiceGroup, NewInvoiceGroupMember, PendingInvoiceDocument,
    ReportedInvoice, ReviewAction, TicketType,
};
use invoice_store::{LedgerDb, StoreError};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{ipc::Response, AppHandle, Manager, State};

use super::invoice::InvoiceDto;
use crate::error::{AppError, AppResult};
use crate::AppState;

const DATE_FMT: &str = "%Y-%m-%d";
const DATETIME_LOCAL_FMT: &str = "%Y-%m-%dT%H:%M";
const DATETIME_FMT: &str = "%Y-%m-%d %H:%M:%S";
const MAX_PREVIEW_BYTES: u64 = 20 * 1024 * 1024;
const MAX_ATTACHED_DOCUMENT_BYTES: u64 = 100 * 1024 * 1024;
type GroupingRecomputeInputs = (
    Vec<ReportedInvoice>,
    Vec<ExpenseItem>,
    String,
    Vec<StationCityAlias>,
);

#[cfg(target_os = "windows")]
pub(crate) fn open_with_windows_default(path: &Path) -> AppResult<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide_path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: every pointer is either null or points to a NUL-terminated UTF-16 buffer that
    // remains alive for the duration of ShellExecuteW. The path has already been resolved from
    // ledger.db and validated as an existing ordinary file or its containing directory.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            wide_path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result <= 32 {
        return Err(AppError::io(format!(
            "调用 Windows 默认程序失败（ShellExecuteW={result}）"
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn open_with_windows_default(_path: &Path) -> AppResult<()> {
    Err(AppError::io("系统打开原件仅支持 Windows"))
}

#[derive(Debug, Clone, Serialize)]
pub struct InvoicePreviewMetadata {
    pub file_name: String,
    pub extension: String,
    pub mime_type: Option<String>,
    /// image / pdf / ofd / text / unsupported / too_large
    pub preview_kind: String,
    pub bytes: u64,
    pub page_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConcurDraftCapability {
    pub enabled: bool,
    pub adapter_status: String,
    pub reason: String,
    pub required_confirmations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupingRecomputeResult {
    pub invoice_count: usize,
    pub group_count: usize,
    pub business_trip_count: usize,
    pub unresolved_transport_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExpenseCategoryReanalysisResult {
    pub scanned_count: usize,
    pub changed_count: usize,
    pub confirmed_count: usize,
    pub suggestion_count: usize,
    pub remaining_unclassified_count: usize,
}

/// Read-only proof that the automatic part of a batch can be recreated from its managed
/// originals. IDs are reported instead of invoice field values so the audit output does not
/// disclose private invoice contents.
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct BatchSourceRebuildAuditResult {
    pub batch_id: i64,
    pub source_invoice_count: usize,
    pub reparsed_invoice_count: usize,
    pub parse_failure_invoice_ids: Vec<i64>,
    pub core_field_mismatch_invoice_ids: Vec<i64>,
    pub automatic_category_mismatch_invoice_ids: Vec<i64>,
    pub manual_decision_expense_count: usize,
    pub supporting_document_count: usize,
    pub recognized_supporting_document_count: usize,
    pub source_city_invoice_ids: Vec<i64>,
    pub source_city_grouped_invoice_ids: Vec<i64>,
    pub recreated_business_trip_invoice_ids: Vec<i64>,
    pub current_business_trip_invoice_ids: Vec<i64>,
    pub recreated_group_count: usize,
    pub recreated_business_trip_count: usize,
    pub current_grouping_matches_rebuild: bool,
    pub reproducible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GroupEvidenceKey {
    title: String,
    start_date: String,
    end_date: String,
}

fn preserved_transport_evidence(
    grouping: Option<&invoice_store::models::BatchGrouping>,
) -> HashMap<GroupEvidenceKey, String> {
    grouping
        .into_iter()
        .flat_map(|grouping| &grouping.groups)
        .filter(|group| group.kind == "business_trip")
        .filter_map(|group| {
            let evidence = serde_json::from_str::<serde_json::Value>(&group.evidence_json).ok()?;
            let status = evidence.get("transportEvidenceStatus")?.as_str()?;
            if !matches!(status, "company_paid" | "not_required") {
                return None;
            }
            Some((
                GroupEvidenceKey {
                    title: group.title.clone(),
                    start_date: group.start_date.clone(),
                    end_date: group.end_date.clone(),
                },
                status.to_string(),
            ))
        })
        .collect()
}

fn supporting_facts_from_managed_file(
    path: &Path,
) -> Option<invoice_parse::pdf::SupportingDocumentFacts> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "pdf" | "png" | "jpg" | "jpeg" | "webp" | "bmp"
    ) {
        return None;
    }
    let text_facts = (extension == "pdf")
        .then(|| fs::read(path).ok())
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

fn enrich_reparsed_invoices_from_attached_documents(
    parsed: &mut [(i64, ParsedInvoice)],
    expenses: &[ExpenseItem],
) -> (usize, usize) {
    let mut supporting_document_count = 0usize;
    let mut recognized_supporting_document_count = 0usize;
    for expense in expenses {
        for document in expense
            .documents
            .iter()
            .filter(|document| !matches!(document.role.as_str(), "main_invoice" | "duplicate_copy"))
        {
            supporting_document_count += 1;
            let Some(facts) = supporting_facts_from_managed_file(Path::new(&document.file_path))
            else {
                continue;
            };
            recognized_supporting_document_count += 1;
            let Some(target_index) = parsed
                .iter()
                .position(|(invoice_id, _)| *invoice_id == expense.primary_invoice_id)
            else {
                continue;
            };
            match facts.kind.as_str() {
                "ride_hailing_itinerary" => {
                    let target = &mut parsed[target_index].1;
                    target.ticket_type = ParseTicketType::CityTransport;
                    target.city = facts.cities.first().cloned().or(target.city.clone());
                    if let Some(start) = facts.start_date {
                        target.departure_time = start.and_hms_opt(0, 0, 0);
                    }
                    for (index, (_, hotel)) in parsed.iter_mut().enumerate() {
                        if index == target_index || hotel.ticket_type != ParseTicketType::Hotel {
                            continue;
                        }
                        let seller = hotel.seller_name.as_deref().unwrap_or_default();
                        if facts
                            .hotel_mentions
                            .iter()
                            .any(|mention| seller.contains(mention))
                        {
                            hotel.city = facts.cities.first().cloned().or(hotel.city.clone());
                            hotel.checkin_date = facts.start_date.or(hotel.checkin_date);
                        }
                    }
                }
                "hotel_folio" => {
                    let target = &mut parsed[target_index].1;
                    target.ticket_type = ParseTicketType::Hotel;
                    target.checkin_date = facts.start_date.or(target.checkin_date);
                    target.city = facts.cities.first().cloned().or(target.city.clone());
                }
                _ => {}
            }
        }
    }
    (
        supporting_document_count,
        recognized_supporting_document_count,
    )
}

fn detected_category_code(ticket_type: ParseTicketType) -> Option<&'static str> {
    match ticket_type {
        ParseTicketType::Rail => Some("rail"),
        ParseTicketType::Flight => Some("flight"),
        ParseTicketType::Hotel => Some("hotel"),
        ParseTicketType::CityTransport => Some("city_transport"),
        ParseTicketType::Meal => Some("meal"),
        ParseTicketType::CourierLogistics => Some("courier_logistics"),
        ParseTicketType::Other => None,
    }
}

fn category_from_invoice_file(path: &Path) -> Option<ParseTicketType> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let detected_from_text = fs::read(path).ok().and_then(|bytes| {
        let text = match extension.as_str() {
            "pdf" => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                invoice_parse::pdf::extract_text(&bytes, path)
            }))
            .ok()?
            .ok()?,
            "xml" => invoice_parse::xml::collect_leaf_elements(&bytes)
                .ok()?
                .into_iter()
                .map(|leaf| leaf.text)
                .collect::<Vec<_>>()
                .join("\n"),
            _ => return None,
        };
        invoice_parse::expense_classifier::classify_invoice_text(&text)
    });
    detected_from_text.or_else(|| {
        crate::commands::invoice::do_parse(path.to_str()?, None)
            .ok()
            .map(|invoice| invoice.ticket_type)
            .filter(|ticket_type| *ticket_type != ParseTicketType::Other)
    })
}

pub fn reanalyze_expense_categories_for_ledger(
    db: &LedgerDb,
    batch_id: i64,
) -> AppResult<ExpenseCategoryReanalysisResult> {
    let invoices = db
        .list_invoices_by_batch(batch_id)
        .map_err(|error| map_store_error("读取发票", error))?;
    let expenses = db
        .list_expense_items_by_batch(batch_id)
        .map_err(|error| map_store_error("读取费用", error))?;
    let invoice_map = invoices
        .iter()
        .map(|invoice| (invoice.id, invoice))
        .collect::<std::collections::HashMap<_, _>>();
    let candidates = expenses
        .iter()
        .filter(|expense| expense.category_source != "manual_review")
        .collect::<Vec<_>>();
    let scanned_count = candidates.len();
    let mut detections = Vec::new();
    for expense in candidates {
        let Some(invoice) = invoice_map.get(&expense.primary_invoice_id) else {
            continue;
        };
        let detected = category_from_invoice_file(Path::new(&invoice.file_path))
            .and_then(detected_category_code)
            .map(|code| (code, "parser.reanalysis", true))
            .or_else(|| {
                invoice
                    .seller_name
                    .as_deref()
                    .and_then(invoice_parse::expense_classifier::classify_merchant_name)
                    .and_then(detected_category_code)
                    .map(|code| (code, "merchant_name.suggestion", false))
            });
        let detected = detected.or_else(|| {
            matches!(
                expense.category_source.as_str(),
                "parser.reanalysis" | "merchant_name.suggestion"
            )
            .then_some(("other", "parser.reanalysis", false))
        });
        let Some((category_code, source, confirmed)) = detected else {
            continue;
        };
        if expense.category_code == category_code
            && expense.category_source == source
            && expense.category_confirmed == confirmed
        {
            continue;
        }
        detections.push(ExpenseCategoryDetection {
            expense_item_id: expense.id,
            category_code: category_code.to_string(),
            source: source.to_string(),
            confirmed,
        });
    }
    let confirmed_count = detections
        .iter()
        .filter(|detection| detection.confirmed)
        .count();
    let suggestion_count = detections.len() - confirmed_count;
    let changed_count = if detections.is_empty() {
        0
    } else {
        db.apply_detected_expense_categories_with_audit(batch_id, &detections)
            .map_err(|error| map_store_error("保存费用类型识别结果", error))?
    };
    let remaining_unclassified_count = db
        .list_expense_items_by_batch(batch_id)
        .map_err(|error| map_store_error("复核费用类型识别结果", error))?
        .into_iter()
        .filter(|expense| expense.category_code == "other")
        .count();
    Ok(ExpenseCategoryReanalysisResult {
        scanned_count,
        changed_count,
        confirmed_count,
        suggestion_count,
        remaining_unclassified_count,
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConcurVerificationResolutionInput {
    pub session_id: i64,
    /// report / expense / attachment
    pub object_kind: String,
    pub object_id: i64,
    /// True only after the user has found the object in Concur.
    pub exists_in_concur: bool,
    pub external_id: Option<String>,
}

fn preview_format(extension: &str) -> (Option<&'static str>, &'static str) {
    match extension {
        "png" => (Some("image/png"), "image"),
        "jpg" | "jpeg" => (Some("image/jpeg"), "image"),
        "webp" => (Some("image/webp"), "image"),
        "gif" => (Some("image/gif"), "image"),
        "bmp" => (Some("image/bmp"), "image"),
        "pdf" => (Some("application/pdf"), "pdf"),
        "ofd" => (Some("application/ofd"), "ofd"),
        "xml" => (Some("application/xml"), "text"),
        _ => (None, "unsupported"),
    }
}

pub(crate) fn inspect_preview_path(path: &Path) -> AppResult<InvoicePreviewMetadata> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| AppError::io(format!("读取原件状态失败（{}）", error.kind())))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::validation("原件不是可安全预览的普通文件"));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (mime_type, mut preview_kind) = preview_format(&extension);
    if metadata.len() > MAX_PREVIEW_BYTES {
        preview_kind = "too_large";
    }
    let page_count = match (extension.as_str(), preview_kind) {
        ("pdf", "pdf") => invoice_parse::pdf_ocr::pdf_page_count(path).ok(),
        ("ofd", "ofd") => fs::read(path)
            .ok()
            .and_then(|bytes| invoice_parse::ofd_preview::preview_page_count(&bytes, path).ok()),
        _ => None,
    };
    Ok(InvoicePreviewMetadata {
        file_name: path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("原件")
            .to_string(),
        extension,
        mime_type: mime_type.map(str::to_string),
        preview_kind: preview_kind.to_string(),
        bytes: metadata.len(),
        page_count,
    })
}

fn selected_preview_path(
    invoice_id: Option<i64>,
    document_id: Option<i64>,
    pending_document_id: Option<i64>,
    state: &State<'_, Mutex<AppState>>,
) -> AppResult<PathBuf> {
    let selected = [
        invoice_id.is_some(),
        document_id.is_some(),
        pending_document_id.is_some(),
    ]
    .into_iter()
    .filter(|selected| *selected)
    .count();
    if selected != 1 {
        return Err(AppError::validation("必须且只能选择一个原件对象"));
    }
    if let Some(id) = pending_document_id {
        pending_document_preview_path(id, state)
    } else if let Some(id) = document_id {
        document_preview_path(id, state)
    } else {
        invoice_preview_path(invoice_id.expect("validated invoice id"), state)
    }
}

fn invoice_preview_path(invoice_id: i64, state: &State<'_, Mutex<AppState>>) -> AppResult<PathBuf> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    let invoice = app_state
        .ledger_db()?
        .get_invoice(invoice_id)
        .map_err(|error| map_store_error("读取原件", error))?
        .ok_or_else(|| AppError::validation("发票不存在"))?;
    Ok(PathBuf::from(invoice.file_path))
}

fn document_preview_path(
    document_id: i64,
    state: &State<'_, Mutex<AppState>>,
) -> AppResult<PathBuf> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    let document = app_state
        .ledger_db()?
        .get_invoice_document(document_id)
        .map_err(|error| map_store_error("读取费用材料", error))?
        .ok_or_else(|| AppError::validation("费用材料不存在"))?;
    Ok(PathBuf::from(document.file_path))
}

fn pending_document_preview_path(
    document_id: i64,
    state: &State<'_, Mutex<AppState>>,
) -> AppResult<PathBuf> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    let document = app_state
        .ledger_db()?
        .get_pending_invoice_document(document_id)
        .map_err(|error| map_store_error("读取待挂载材料", error))?
        .ok_or_else(|| AppError::validation("待挂载材料不存在"))?;
    Ok(PathBuf::from(document.file_path))
}

fn recovery_file_name(original_name: &str, sha256: &str) -> String {
    let safe_name = Path::new(original_name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("original")
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(120)
        .collect::<String>();
    format!("{}-{}", &sha256[..16], safe_name)
}

fn prepare_relinked_original(
    batch_id: i64,
    current_path: PathBuf,
    original_name: String,
    expected_sha256: Option<String>,
    associated_invoice: Option<ReportedInvoice>,
    replacement_path: PathBuf,
) -> AppResult<(PathBuf, String, bool)> {
    if current_path.is_file() {
        return Err(AppError::validation(
            "原件仍然存在，请先使用“重新加载”或“系统打开”",
        ));
    }
    let metadata = fs::symlink_metadata(&replacement_path)
        .map_err(|error| AppError::io(format!("读取替代文件失败（{}）", error.kind())))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::validation("替代原件必须是普通文件"));
    }
    if metadata.len() == 0 || metadata.len() > MAX_ATTACHED_DOCUMENT_BYTES {
        return Err(AppError::validation("替代原件必须大于 0 且不超过 100 MiB"));
    }
    let old_extension = current_path
        .extension()
        .or_else(|| Path::new(&original_name).extension())
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let new_extension = replacement_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        new_extension.as_str(),
        "xml" | "ofd" | "pdf" | "png" | "jpg" | "jpeg" | "webp" | "bmp"
    ) {
        return Err(AppError::validation("替代原件格式不受支持"));
    }
    if !old_extension.is_empty() && old_extension != new_extension {
        return Err(AppError::validation(format!(
            "替代原件格式必须与原记录一致（.{old_extension}）"
        )));
    }

    let bytes = fs::read(&replacement_path)
        .map_err(|error| AppError::io(format!("读取替代原件失败（{}）", error.kind())))?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    if let Some(expected) = expected_sha256.filter(|value| !value.trim().is_empty()) {
        if !expected.eq_ignore_ascii_case(&sha256) {
            return Err(AppError::validation(
                "所选文件与原记录的 SHA-256 不一致，未重新关联",
            ));
        }
    } else if let Some(invoice) = associated_invoice {
        let reparsed = super::invoice::do_parse(
            &replacement_path.to_string_lossy(),
            Some(invoice.ticket_type.to_str()),
        )?;
        if reparsed.invoice_number.trim() != invoice.invoice_number.trim()
            || reparsed.issue_date != invoice.issue_date
            || reparsed.total_amount != invoice.amount
        {
            return Err(AppError::validation(
                "所选文件的发票号、开票日期或金额与原记录不一致，未重新关联",
            ));
        }
    }

    let stable_dir = crate::paths::data_root()
        .map_err(AppError::from)?
        .join("files")
        .join("recovered")
        .join(format!("batch-{batch_id}"));
    fs::create_dir_all(&stable_dir)?;
    let stable_path = stable_dir.join(recovery_file_name(&original_name, &sha256));
    if stable_path.exists() {
        let stable_bytes = fs::read(&stable_path)?;
        if stable_bytes != bytes {
            return Err(AppError::validation("稳定原件目录发生文件名冲突"));
        }
        return Ok((stable_path, sha256, false));
    }

    let temporary = stable_dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let write_result = (|| -> AppResult<()> {
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        output.write_all(&bytes)?;
        output.sync_all()?;
        fs::rename(&temporary, &stable_path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result?;
    Ok((stable_path, sha256, true))
}

#[derive(Debug, Clone, Deserialize)]
pub struct InvoiceReviewInput {
    pub invoice_number: String,
    pub issue_date: String,
    pub amount: String,
    pub tax_amount: Option<String>,
    pub buyer_name: Option<String>,
    pub seller_name: Option<String>,
    pub ticket_type: String,
    pub city: Option<String>,
    pub departure_time: Option<String>,
    pub checkin_date: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExpenseLocationInput {
    pub city_name: Option<String>,
    pub city_code: Option<String>,
    pub province_name: Option<String>,
    pub province_code: Option<String>,
    pub country_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExpenseTaxInput {
    pub amount: String,
    pub rate: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExpenseItemReviewInput {
    pub category_code: String,
    #[serde(default)]
    pub category_confirmed: bool,
    pub transaction_date: String,
    pub transaction_date_confirmed: bool,
    pub description: String,
    pub counterparty_name: String,
    pub location: ExpenseLocationInput,
    pub payment_method: String,
    pub gross_amount: String,
    pub currency_code: String,
    pub tax_details: Vec<ExpenseTaxInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConcurPreflightInput {
    pub profile_id: i64,
    pub report_name: String,
    pub report_date: String,
    pub comment: String,
    pub upload_overrides_json: String,
}

fn parse_optional_decimal(value: Option<String>, label: &str) -> AppResult<Option<Decimal>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    Decimal::from_str(value)
        .map(Some)
        .map_err(|_| AppError::validation(format!("{label}格式无效")))
}

fn parse_optional_date(value: Option<String>, label: &str) -> AppResult<Option<NaiveDate>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    NaiveDate::parse_from_str(value, DATE_FMT)
        .map(Some)
        .map_err(|_| AppError::validation(format!("{label}格式必须为 YYYY-MM-DD")))
}

fn parse_optional_datetime(value: Option<String>) -> AppResult<Option<NaiveDateTime>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    for format in [DATETIME_LOCAL_FMT, DATETIME_FMT] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(value, format) {
            return Ok(Some(parsed));
        }
    }
    Err(AppError::validation("出发时间格式无效"))
}

fn ticket_type_from_wire(value: &str) -> AppResult<TicketType> {
    match value {
        "rail" => Ok(TicketType::Rail),
        "flight" => Ok(TicketType::Flight),
        "hotel" => Ok(TicketType::Hotel),
        "city_transport" => Ok(TicketType::CityTransport),
        "meal" => Ok(TicketType::Meal),
        "courier_logistics" => Ok(TicketType::CourierLogistics),
        "other" => Ok(TicketType::Other),
        _ => Err(AppError::validation("票据类型无效")),
    }
}

fn parse_review_update(input: InvoiceReviewInput) -> AppResult<InvoiceReviewUpdate> {
    let issue_date = NaiveDate::parse_from_str(input.issue_date.trim(), DATE_FMT)
        .map_err(|_| AppError::validation("开票日期格式必须为 YYYY-MM-DD"))?;
    let amount = Decimal::from_str(input.amount.trim())
        .map_err(|_| AppError::validation("含税金额格式无效"))?;
    Ok(InvoiceReviewUpdate {
        invoice_number: input.invoice_number,
        issue_date,
        amount,
        tax_amount: parse_optional_decimal(input.tax_amount, "税额")?,
        buyer_name: input.buyer_name,
        seller_name: input.seller_name,
        ticket_type: ticket_type_from_wire(&input.ticket_type)?,
        city: input.city,
        departure_time: parse_optional_datetime(input.departure_time)?,
        checkin_date: parse_optional_date(input.checkin_date, "入住日期")?,
    })
}

fn parse_expense_item_update(input: ExpenseItemReviewInput) -> AppResult<ExpenseItemUpdate> {
    let transaction_date = NaiveDate::parse_from_str(input.transaction_date.trim(), DATE_FMT)
        .map_err(|_| AppError::validation("实际发生日期格式必须为 YYYY-MM-DD"))?;
    let gross_amount = Decimal::from_str(input.gross_amount.trim())
        .map_err(|_| AppError::validation("实际金额格式无效"))?;
    let mut tax_details = Vec::with_capacity(input.tax_details.len());
    for tax in input.tax_details {
        let amount = Decimal::from_str(tax.amount.trim())
            .map_err(|_| AppError::validation("票面税额格式无效"))?;
        let rate = parse_optional_decimal(tax.rate, "票面税率")?;
        tax_details.push(ExpenseTaxDetail {
            amount,
            rate,
            source: tax
                .source
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("manual_review")
                .to_string(),
        });
    }
    let optional = |value: Option<String>| {
        value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    Ok(ExpenseItemUpdate {
        category_code: input.category_code.trim().to_string(),
        category_confirmed: input.category_confirmed,
        transaction_date,
        transaction_date_confirmed: input.transaction_date_confirmed,
        description: input.description,
        counterparty_name: input.counterparty_name,
        location: ExpenseLocation {
            city_name: optional(input.location.city_name),
            city_code: optional(input.location.city_code),
            province_name: optional(input.location.province_name),
            province_code: optional(input.location.province_code),
            country_code: optional(input.location.country_code),
        },
        payment_method: input.payment_method.trim().to_string(),
        gross_amount,
        currency_code: input.currency_code.trim().to_ascii_uppercase(),
        tax_details,
    })
}

fn safe_attachment_name(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\0'..='\u{1f}' => '_',
            _ => character,
        })
        .collect::<String>();
    let sanitized = sanitized.trim().trim_matches('.');
    if sanitized.is_empty() {
        "document".to_string()
    } else {
        sanitized.chars().take(120).collect()
    }
}

fn attachment_digest(path: &Path) -> AppResult<String> {
    let mut file = fs::File::open(path)
        .map_err(|error| AppError::io(format!("读取配套材料失败（{}）", error.kind())))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| AppError::io(format!("读取配套材料失败（{}）", error.kind())))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn map_store_error(action: &str, error: StoreError) -> AppError {
    match error {
        StoreError::Validation(message) => AppError::validation(format!("{action}失败：{message}")),
        StoreError::NotFound(_) => AppError::validation(format!("{action}失败：记录不存在")),
        StoreError::InvalidStateTransition { .. } => {
            AppError::validation(format!("{action}失败：批次状态不允许此操作"))
        }
        StoreError::Io(_) => AppError::io(format!("{action}失败")),
        _ => AppError::database(format!("{action}失败")),
    }
}

fn parse_ticket_type(ticket_type: TicketType) -> ParseTicketType {
    match ticket_type {
        TicketType::Rail => ParseTicketType::Rail,
        TicketType::Flight => ParseTicketType::Flight,
        TicketType::Hotel => ParseTicketType::Hotel,
        TicketType::CityTransport => ParseTicketType::CityTransport,
        TicketType::Meal => ParseTicketType::Meal,
        TicketType::CourierLogistics => ParseTicketType::CourierLogistics,
        TicketType::Other => ParseTicketType::Other,
    }
}

fn parse_expense_category_code(category_code: &str) -> ParseTicketType {
    match category_code {
        "rail" => ParseTicketType::Rail,
        "flight" => ParseTicketType::Flight,
        "hotel" => ParseTicketType::Hotel,
        "city_transport" => ParseTicketType::CityTransport,
        "meal" => ParseTicketType::Meal,
        "courier_logistics" => ParseTicketType::CourierLogistics,
        _ => ParseTicketType::Other,
    }
}

fn grouping_source_invoice(invoice: &ReportedInvoice) -> ParsedInvoice {
    ParsedInvoice {
        invoice_number: invoice.invoice_number.clone(),
        issue_date: invoice.issue_date,
        total_amount: invoice.amount,
        tax_amount: invoice.tax_amount,
        tax_rate: None,
        buyer_name: invoice.buyer_name.clone(),
        seller_name: invoice.seller_name.clone(),
        ticket_type: parse_ticket_type(invoice.ticket_type),
        transport_document_kind: TransportDocumentKind::Unknown,
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        city: invoice.city.clone(),
        travel_route: None,
        departure_time: invoice.departure_time,
        checkin_date: invoice.checkin_date,
        source_path: PathBuf::from(&invoice.file_path),
    }
}

fn regroup_kind(kind: &TripKind) -> (String, String) {
    match kind {
        TripKind::BusinessTrip { start, end, cities } => {
            let destination = if cities.is_empty() {
                "目的地待确认".to_string()
            } else {
                cities.join("、")
            };
            let _ = (start, end);
            ("business_trip".to_string(), format!("{destination}出差"))
        }
        TripKind::LocalMonth { month, .. } => {
            ("local_month".to_string(), format!("{month} 月市内消费"))
        }
        TripKind::CourierMonth { month, .. } => {
            ("courier_month".to_string(), format!("{month} 月快递物流"))
        }
        TripKind::Excluded => ("excluded".to_string(), "已排除票据".to_string()),
        TripKind::NeedsReview { reason } => {
            ("needs_review".to_string(), format!("待人工复核：{reason}"))
        }
    }
}

fn grouping_member_reason(invoice: &ParsedInvoice) -> String {
    let nature = match invoice.transport_document_kind {
        TransportDocumentKind::Refund => "；交通票性质：退票费，不作为路线节点",
        TransportDocumentKind::Change => "；交通票性质：改签费，不作为路线节点",
        TransportDocumentKind::Sale => "；交通票性质：有效售票",
        TransportDocumentKind::Unknown => "",
    };
    format!("{GROUPING_RULE_VERSION}：按类型、实际日期和地点归组{nature}")
}

#[tauri::command]
pub fn update_invoice_review(
    invoice_id: i64,
    input: InvoiceReviewInput,
    state: State<Mutex<AppState>>,
) -> AppResult<InvoiceDto> {
    let update = parse_review_update(input)?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    let db = app_state.ledger_db()?;
    db.update_invoice_review_fields(invoice_id, &update)
        .map_err(|error| map_store_error("保存发票修改", error))?;
    let invoice = db
        .get_invoice(invoice_id)
        .map_err(|error| map_store_error("读取发票", error))?
        .ok_or_else(|| AppError::validation("保存后未找到发票"))?;
    Ok(InvoiceDto::from(invoice))
}

#[tauri::command]
pub fn list_expense_items(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<ExpenseItem>> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .list_expense_items_by_batch(batch_id)
        .map_err(|error| map_store_error("读取费用清单", error))
}

#[tauri::command]
pub fn update_expense_item(
    expense_item_id: i64,
    input: ExpenseItemReviewInput,
    state: State<Mutex<AppState>>,
) -> AppResult<ExpenseItem> {
    let update = parse_expense_item_update(input)?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .update_expense_item_with_audit(expense_item_id, &update)
        .map_err(|error| map_store_error("保存费用信息", error))
}

#[tauri::command]
pub fn attach_expense_document(
    expense_item_id: i64,
    role: String,
    source_path: String,
    state: State<Mutex<AppState>>,
) -> AppResult<InvoiceDocument> {
    let source = PathBuf::from(source_path);
    let metadata = fs::symlink_metadata(&source)
        .map_err(|error| AppError::io(format!("读取配套材料失败（{}）", error.kind())))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::validation(
            "配套材料必须是普通文件，不能是链接或目录",
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAX_ATTACHED_DOCUMENT_BYTES {
        return Err(AppError::validation("配套材料必须大于 0 且不超过 100 MiB"));
    }
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "pdf" | "ofd" | "xml" | "png" | "jpg" | "jpeg" | "webp" | "bmp"
    ) {
        return Err(AppError::validation(
            "配套材料仅支持 PDF、OFD、XML 和常见图片格式",
        ));
    }
    let expense = {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        app_state
            .ledger_db()?
            .get_expense_item(expense_item_id)
            .map_err(|error| map_store_error("读取费用", error))?
            .ok_or_else(|| AppError::validation("费用不存在"))?
    };
    let digest = attachment_digest(&source)?;
    let original_name = source
        .file_name()
        .and_then(|value| value.to_str())
        .map(safe_attachment_name)
        .unwrap_or_else(|| format!("document.{extension}"));
    let directory = crate::paths::data_root()
        .map_err(|error| AppError::io(format!("定位数据目录失败：{error}")))?
        .join("files")
        .join("expense-documents")
        .join(format!("batch-{}", expense.batch_id))
        .join(format!("expense-{}", expense.id));
    fs::create_dir_all(&directory)?;
    let destination = directory.join(format!("{}-{}", digest, original_name));
    if !destination.exists() {
        fs::copy(&source, &destination)
            .map_err(|error| AppError::io(format!("保存配套材料失败（{}）", error.kind())))?;
        fs::OpenOptions::new()
            .read(true)
            .open(&destination)?
            .sync_all()?;
    }
    let mime_type = match extension.as_str() {
        "pdf" => Some("application/pdf"),
        "ofd" => Some("application/ofd"),
        "xml" => Some("application/xml"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    };
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .add_expense_document_with_audit(
            expense_item_id,
            &role,
            &destination.to_string_lossy(),
            &original_name,
            mime_type,
            Some(&digest),
        )
        .map_err(|error| map_store_error("挂载配套材料", error))
}

#[tauri::command]
pub fn remove_expense_document(document_id: i64, state: State<Mutex<AppState>>) -> AppResult<()> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .remove_expense_document_with_audit(document_id)
        .map_err(|error| map_store_error("移除材料挂载", error))
}

#[tauri::command]
pub fn link_duplicate_invoice_to_expense(
    source_invoice_id: i64,
    target_expense_item_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<InvoiceDocument> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .link_duplicate_invoice_to_expense_with_audit(source_invoice_id, target_expense_item_id)
        .map_err(|error| map_store_error("归并重复来源副本", error))
}

#[tauri::command]
pub fn list_pending_invoice_documents(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<PendingInvoiceDocument>> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .list_pending_invoice_documents(batch_id)
        .map_err(|error| map_store_error("读取待挂载材料", error))
}

#[tauri::command]
pub fn assign_pending_invoice_document(
    pending_document_id: i64,
    expense_item_id: i64,
    role: String,
    state: State<Mutex<AppState>>,
) -> AppResult<InvoiceDocument> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .assign_pending_invoice_document_with_audit(pending_document_id, expense_item_id, &role)
        .map_err(|error| map_store_error("挂载批次材料", error))
}

#[tauri::command]
pub async fn convert_didi_itinerary_to_expense(
    pending_document_id: i64,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<ExpenseItem> {
    let pending = {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        app_state
            .ledger_db()?
            .get_pending_invoice_document(pending_document_id)
            .map_err(|error| map_store_error("读取滴滴行程单", error))?
            .ok_or_else(|| AppError::validation("待处理材料不存在"))?
    };
    if pending.status != "pending" || pending.proposed_role != "itinerary" {
        return Err(AppError::validation("该材料不是可转换的待处理行程单"));
    }

    let path = PathBuf::from(&pending.file_path);
    let facts =
        tauri::async_runtime::spawn_blocking(move || supporting_facts_from_managed_file(&path))
            .await
            .map_err(|_| AppError::internal("滴滴行程单分析线程异常"))?
            .ok_or_else(|| AppError::validation("无法从该材料提取可靠的行程金额和日期"))?;
    if facts.kind != "ride_hailing_itinerary" || facts.provider != "didi" {
        return Err(AppError::validation(
            "仅支持将明确识别的滴滴出行行程单转为费用",
        ));
    }
    if facts.total_amount <= Decimal::ZERO {
        return Err(AppError::validation("滴滴行程单金额无效，不能创建费用"));
    }
    let start_date = facts
        .start_date
        .ok_or_else(|| AppError::validation("滴滴行程单缺少可靠的行程开始日期"))?;
    let end_date = facts.end_date.unwrap_or(start_date);
    let now = Utc::now().naive_utc();
    let invoice = ReportedInvoice {
        id: 0,
        batch_id: pending.batch_id,
        // 行程单不是税务发票，不伪造发票号码。
        invoice_number: String::new(),
        issue_date: start_date,
        amount: facts.total_amount,
        tax_amount: None,
        buyer_name: None,
        seller_name: Some("滴滴出行".to_string()),
        ticket_type: TicketType::CityTransport,
        city: facts.cities.first().cloned(),
        departure_time: start_date.and_hms_opt(0, 0, 0),
        checkin_date: None,
        file_path: pending.file_path,
        created_at: now,
        updated_at: now,
        verification_result: None,
        is_duplicate: false,
        duplicate_reason: None,
    };
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .convert_pending_itinerary_to_expense(pending_document_id, &invoice, end_date)
        .map_err(|error| map_store_error("从滴滴行程单创建费用", error))
}

#[tauri::command]
pub fn ignore_pending_invoice_document(
    pending_document_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .ignore_pending_invoice_document_with_audit(pending_document_id)
        .map_err(|error| map_store_error("忽略无关文件", error))
}

#[tauri::command]
pub fn list_concur_mapping_profiles(
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<ConcurMappingProfile>> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .list_concur_mapping_profiles()
        .map_err(|error| map_store_error("读取 Concur 映射配置", error))
}

#[tauri::command]
pub fn save_concur_mapping_profile(
    input: ConcurMappingProfileInput,
    state: State<Mutex<AppState>>,
) -> AppResult<ConcurMappingProfile> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .save_concur_mapping_profile(&input)
        .map_err(|error| map_store_error("保存 Concur 映射配置", error))
}

#[tauri::command]
pub fn prepare_concur_upload(
    batch_id: i64,
    input: ConcurPreflightInput,
    state: State<Mutex<AppState>>,
) -> AppResult<ConcurUploadPreflight> {
    let report_date = NaiveDate::parse_from_str(input.report_date.trim(), DATE_FMT)
        .map_err(|_| AppError::validation("报销单日期格式必须为 YYYY-MM-DD"))?;
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .prepare_concur_upload(
            batch_id,
            input.profile_id,
            &input.report_name,
            report_date,
            &input.comment,
            &input.upload_overrides_json,
        )
        .map_err(|error| map_store_error("执行 Concur 上传预检", error))
}

#[tauri::command]
pub fn list_concur_upload_sessions(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<ConcurUploadSession>> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .list_concur_upload_sessions(batch_id)
        .map_err(|error| map_store_error("读取 Concur 上传会话", error))
}

#[tauri::command]
pub fn get_concur_upload_status(
    session_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Option<ConcurUploadStatus>> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .get_concur_upload_status(session_id)
        .map_err(|error| map_store_error("读取 Concur 上传状态", error))
}

/// The upload workflow is intentionally capability-gated. A real adapter may only be enabled
/// after its tenant, auth scope, field semantics, and draft-only behavior have been validated.
#[tauri::command]
pub fn get_concur_draft_capability(
    state: State<Mutex<AppState>>,
) -> AppResult<ConcurDraftCapability> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    let session = app_state.concur_session();
    let enabled = session.is_some_and(|value| value.draft_workflow_verified);
    Ok(ConcurDraftCapability {
        enabled,
        adapter_status: if enabled {
            "verified"
        } else if session.is_some_and(|value| value.read_verified) {
            "read_only_verified"
        } else {
            "not_configured"
        }
        .to_string(),
        reason: if enabled {
            "当前程序会话已完成 Concur 草稿、费用和附件闭环测试；只会创建未提交草稿".to_string()
        } else if session.is_some_and(|value| value.read_verified) {
            "Concur 只读连接已通过，请到“设置 → Concur 能力”完成草稿闭环测试".to_string()
        } else {
            "请先到“设置 → Concur 能力”输入本次 OAuth 访问令牌并执行能力测试".to_string()
        },
        required_confirmations: vec![
            "OAuth 令牌具有 EXPRPT 与 IMAGE 权限".to_string(),
            "目标费用类型代码和付款类型 ID 可创建费用".to_string(),
            "附件能够关联到指定费用并回读".to_string(),
            "报销单始终保持未提交草稿".to_string(),
        ],
    })
}

#[tauri::command]
pub fn resolve_concur_upload_verification(
    input: ConcurVerificationResolutionInput,
    state: State<Mutex<AppState>>,
) -> AppResult<ConcurUploadStatus> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    let db = app_state.ledger_db()?;
    let before = db
        .get_concur_upload_status(input.session_id)
        .map_err(|error| map_store_error("读取待核对 Concur 会话", error))?
        .ok_or_else(|| AppError::validation("Concur 上传会话不存在"))?;
    let external_id = input
        .external_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if input.exists_in_concur && external_id.is_none() {
        return Err(AppError::validation(
            "确认对象已存在时必须填写从 Concur 核对得到的外部 ID",
        ));
    }
    let resolved_external_id = if input.exists_in_concur {
        external_id
    } else {
        None
    };
    match input.object_kind.as_str() {
        "report" => {
            if input.object_id != input.session_id || before.session.external_report_id.is_some() {
                return Err(AppError::validation("待核对对象不是当前会话的报销单"));
            }
            db.resolve_concur_report_verification(input.session_id, resolved_external_id)
        }
        "expense" => {
            if !before
                .items
                .iter()
                .any(|item| item.id == input.object_id && item.status == "needs_verification")
            {
                return Err(AppError::validation("待核对费用不属于当前会话"));
            }
            db.resolve_concur_expense_verification(input.object_id, resolved_external_id)
        }
        "attachment" => {
            if !before.items.iter().any(|item| {
                item.attachments.iter().any(|attachment| {
                    attachment.id == input.object_id && attachment.status == "needs_verification"
                })
            }) {
                return Err(AppError::validation("待核对附件不属于当前会话"));
            }
            db.resolve_concur_attachment_verification(input.object_id, resolved_external_id)
        }
        _ => return Err(AppError::validation("不支持的 Concur 核对对象类型")),
    }
    .map_err(|error| map_store_error("确认 Concur 外部结果", error))?;
    db.get_concur_upload_status(input.session_id)
        .map_err(|error| map_store_error("刷新 Concur 上传状态", error))?
        .ok_or_else(|| AppError::validation("Concur 上传会话不存在"))
}

#[tauri::command]
pub fn get_invoice_preview_metadata(
    invoice_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<InvoicePreviewMetadata> {
    let path = invoice_preview_path(invoice_id, &state)?;
    inspect_preview_path(&path)
}

#[tauri::command]
pub fn read_invoice_preview(invoice_id: i64, state: State<Mutex<AppState>>) -> AppResult<Response> {
    let path = invoice_preview_path(invoice_id, &state)?;
    let metadata = inspect_preview_path(&path)?;
    if !matches!(
        metadata.preview_kind.as_str(),
        "image" | "pdf" | "ofd" | "text"
    ) {
        return Err(AppError::validation("该原件格式当前不能在应用内预览"));
    }
    let bytes = fs::read(&path)
        .map_err(|error| AppError::io(format!("读取原件失败（{}）", error.kind())))?;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub fn get_expense_document_preview_metadata(
    document_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<InvoicePreviewMetadata> {
    let path = document_preview_path(document_id, &state)?;
    inspect_preview_path(&path)
}

#[tauri::command]
pub fn read_expense_document_preview(
    document_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Response> {
    let path = document_preview_path(document_id, &state)?;
    let metadata = inspect_preview_path(&path)?;
    if !matches!(
        metadata.preview_kind.as_str(),
        "image" | "pdf" | "ofd" | "text"
    ) {
        return Err(AppError::validation("该材料格式当前不能在应用内预览"));
    }
    let bytes = fs::read(&path)
        .map_err(|error| AppError::io(format!("读取费用材料失败（{}）", error.kind())))?;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub fn get_pending_document_preview_metadata(
    pending_document_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<InvoicePreviewMetadata> {
    let path = pending_document_preview_path(pending_document_id, &state)?;
    inspect_preview_path(&path)
}

#[tauri::command]
pub fn read_pending_document_preview(
    pending_document_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Response> {
    let path = pending_document_preview_path(pending_document_id, &state)?;
    let metadata = inspect_preview_path(&path)?;
    if !matches!(
        metadata.preview_kind.as_str(),
        "image" | "pdf" | "ofd" | "text"
    ) {
        return Err(AppError::validation("该待挂载材料当前不能在应用内预览"));
    }
    let bytes = fs::read(&path)
        .map_err(|error| AppError::io(format!("读取待挂载材料失败（{}）", error.kind())))?;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub async fn render_pdf_preview_page(
    invoice_id: Option<i64>,
    document_id: Option<i64>,
    pending_document_id: Option<i64>,
    page: u32,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<Response> {
    if page == 0 {
        return Err(AppError::validation("PDF 页码必须从 1 开始"));
    }
    let path = selected_preview_path(invoice_id, document_id, pending_document_id, &state)?;
    let metadata = inspect_preview_path(&path)?;
    if metadata.preview_kind != "pdf" {
        return Err(AppError::validation("所选原件不是可预览的 PDF"));
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
pub async fn render_pdf_text_preview_page(
    invoice_id: Option<i64>,
    document_id: Option<i64>,
    pending_document_id: Option<i64>,
    page: u32,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<invoice_parse::pdf_preview::PdfPreviewPage> {
    if page == 0 {
        return Err(AppError::validation("PDF 页码必须从 1 开始"));
    }
    let path = selected_preview_path(invoice_id, document_id, pending_document_id, &state)?;
    let metadata = inspect_preview_path(&path)?;
    if metadata.preview_kind != "pdf" {
        return Err(AppError::validation("所选原件不是可预览的 PDF"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = fs::read(&path)
            .map_err(|error| AppError::io(format!("读取 PDF 原件失败（{}）", error.kind())))?;
        invoice_parse::pdf_preview::render_text_preview_page(&bytes, &path, page)
            .map_err(|error| AppError::parse(format!("PDF 兼容版式预览失败: {error}")))
    })
    .await
    .map_err(|_| AppError::internal("PDF 兼容预览线程异常"))?
}

#[tauri::command]
pub async fn render_ofd_preview_page(
    invoice_id: Option<i64>,
    document_id: Option<i64>,
    pending_document_id: Option<i64>,
    page: u32,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<invoice_parse::ofd_preview::OfdPreviewPage> {
    if page == 0 {
        return Err(AppError::validation("OFD 页码必须从 1 开始"));
    }
    let path = selected_preview_path(invoice_id, document_id, pending_document_id, &state)?;
    let metadata = inspect_preview_path(&path)?;
    if metadata.preview_kind != "ofd" {
        return Err(AppError::validation("所选原件不是可预览的 OFD"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = fs::read(&path)
            .map_err(|error| AppError::io(format!("读取 OFD 原件失败（{}）", error.kind())))?;
        invoice_parse::ofd_preview::render_preview_page(&bytes, &path, page)
            .map_err(|error| AppError::parse(format!("OFD 版式预览生成失败: {error}")))
    })
    .await
    .map_err(|_| AppError::internal("OFD 预览线程异常"))?
}

/// Open only a path resolved from ledger.db. The webview never supplies an arbitrary filesystem
/// path, so the external fallback cannot be repurposed as a generic shell launcher.
#[tauri::command]
pub fn open_preview_path(
    invoice_id: Option<i64>,
    document_id: Option<i64>,
    pending_document_id: Option<i64>,
    reveal: bool,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    let path = selected_preview_path(invoice_id, document_id, pending_document_id, &state)?;
    inspect_preview_path(&path)?;
    let target = if reveal {
        path.parent()
            .ok_or_else(|| AppError::validation("无法定位原件所在文件夹"))?
            .to_path_buf()
    } else {
        path
    };
    open_with_windows_default(&target)
}

/// 仅用于恢复已丢失的原件引用。所选文件会先校验哈希或主发票三要素，
/// 再复制到 DataRoot；不会把任意外部路径直接写入数据库。
#[tauri::command]
pub async fn repair_missing_preview_file(
    invoice_id: Option<i64>,
    document_id: Option<i64>,
    pending_document_id: Option<i64>,
    replacement_path: String,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<()> {
    let selected = [
        invoice_id.is_some(),
        document_id.is_some(),
        pending_document_id.is_some(),
    ]
    .into_iter()
    .filter(|selected| *selected)
    .count();
    if selected != 1 {
        return Err(AppError::validation("必须且只能选择一个待恢复原件"));
    }
    let replacement_path = PathBuf::from(replacement_path);
    if replacement_path.as_os_str().is_empty() {
        return Err(AppError::validation("请选择替代原件"));
    }

    let (
        batch_id,
        current_path,
        original_name,
        expected_sha256,
        associated_invoice,
        target_kind,
        target_id,
    ) = {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        let db = app_state.ledger_db()?;
        if let Some(id) = invoice_id {
            let invoice = db
                .get_invoice(id)
                .map_err(|error| map_store_error("读取待恢复发票", error))?
                .ok_or_else(|| AppError::validation("发票不存在"))?;
            let original_name = Path::new(&invoice.file_path)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("invoice")
                .to_string();
            (
                invoice.batch_id,
                PathBuf::from(&invoice.file_path),
                original_name,
                None,
                Some(invoice),
                "invoice",
                id,
            )
        } else if let Some(id) = document_id {
            let document = db
                .get_invoice_document(id)
                .map_err(|error| map_store_error("读取待恢复材料", error))?
                .ok_or_else(|| AppError::validation("费用材料不存在"))?;
            let associated = match document.source_invoice_id {
                Some(invoice_id) => db
                    .get_invoice(invoice_id)
                    .map_err(|error| map_store_error("读取材料所属发票", error))?,
                None => None,
            };
            (
                document.batch_id,
                PathBuf::from(&document.file_path),
                document.original_name,
                document.sha256,
                associated,
                "document",
                id,
            )
        } else {
            let id = pending_document_id.expect("validated pending document id");
            let document = db
                .get_pending_invoice_document(id)
                .map_err(|error| map_store_error("读取待恢复材料", error))?
                .ok_or_else(|| AppError::validation("待挂载材料不存在"))?;
            (
                document.batch_id,
                PathBuf::from(&document.file_path),
                document.original_name,
                document.sha256,
                None,
                "pending",
                id,
            )
        }
    };

    let original_name_for_copy = original_name.clone();
    let (stable_path, sha256, created) = tauri::async_runtime::spawn_blocking(move || {
        prepare_relinked_original(
            batch_id,
            current_path,
            original_name_for_copy,
            expected_sha256,
            associated_invoice,
            replacement_path,
        )
    })
    .await
    .map_err(|_| AppError::internal("原件恢复线程异常终止"))??;
    let stable_path_text = stable_path.to_string_lossy().into_owned();
    let update_result = {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        let db = app_state.ledger_db()?;
        match target_kind {
            "invoice" => db.repair_invoice_original_file(
                target_id,
                &stable_path_text,
                &original_name,
                &sha256,
            ),
            "document" => db.repair_invoice_document_file(
                target_id,
                &stable_path_text,
                &original_name,
                &sha256,
            ),
            _ => db.repair_pending_document_file(
                target_id,
                &stable_path_text,
                &original_name,
                &sha256,
            ),
        }
        .map_err(|error| map_store_error("重新关联原件", error))
    };
    if update_result.is_err() && created {
        let _ = fs::remove_file(stable_path);
    }
    update_result
}

#[tauri::command]
pub fn set_invoice_excluded(
    invoice_id: i64,
    excluded: bool,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .set_invoice_excluded_with_audit(invoice_id, excluded)
        .map_err(|error| {
            map_store_error(
                if excluded {
                    "排除发票"
                } else {
                    "恢复发票"
                },
                error,
            )
        })
}

#[tauri::command]
pub fn create_manual_group(
    batch_id: i64,
    kind: String,
    title: String,
    start_date: String,
    end_date: String,
    state: State<Mutex<AppState>>,
) -> AppResult<i64> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .create_manual_invoice_group(batch_id, &kind, &title, &start_date, &end_date)
        .map_err(|error| map_store_error("新建归组", error))
}

#[tauri::command]
pub fn move_invoice_group(
    batch_id: i64,
    invoice_id: i64,
    target_group_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .move_invoice_to_group(batch_id, invoice_id, target_group_id)
        .map_err(|error| map_store_error("调整归组", error))
}

#[tauri::command]
pub fn merge_groups(
    batch_id: i64,
    source_group_id: i64,
    target_group_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .merge_invoice_groups(batch_id, source_group_id, target_group_id)
        .map_err(|error| map_store_error("合并归组", error))
}

#[tauri::command]
pub fn set_group_transport_evidence(
    batch_id: i64,
    group_id: i64,
    status: String,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .set_invoice_group_transport_evidence(batch_id, group_id, &status)
        .map_err(|error| map_store_error("更新交通凭证情况", error))
}

fn load_grouping_recompute_inputs(
    db: &LedgerDb,
    batch_id: i64,
) -> AppResult<GroupingRecomputeInputs> {
    let expenses = db
        .list_expense_items_by_batch(batch_id)
        .map_err(|error| map_store_error("读取费用清单", error))?
        .into_iter()
        .filter(|expense| expense.inclusion_status == "included")
        .collect::<Vec<_>>();
    let included_ids = expenses
        .iter()
        .map(|expense| expense.primary_invoice_id)
        .collect::<HashSet<_>>();
    let invoices = db
        .list_invoices_by_batch(batch_id)
        .map_err(|error| map_store_error("读取批次票据", error))?
        .into_iter()
        .filter(|invoice| included_ids.contains(&invoice.id))
        .collect::<Vec<_>>();
    if invoices.is_empty() {
        return Err(AppError::validation("批次中没有可计入并重新归组的费用"));
    }
    let home_city = db
        .get_setting("home_city")
        .map_err(|error| map_store_error("读取常驻城市", error))?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::validation("请先在设置中填写常驻城市"))?;
    let home_station_aliases =
        super::settings::load_effective_home_station_aliases(db, &home_city)?;
    Ok((invoices, expenses, home_city, home_station_aliases))
}

/// Re-read every managed primary invoice without mutating the ledger, compare the automatic
/// fields with the stored batch, and feed the reparsed included expenses through the production
/// grouping engine. User choices such as exclusion and manual category overrides are counted
/// separately: they are reproducible review decisions, not facts that should be guessed from a
/// source file.
#[allow(dead_code)]
pub fn audit_batch_source_rebuild_for_ledger(
    db: &LedgerDb,
    batch_id: i64,
) -> AppResult<BatchSourceRebuildAuditResult> {
    struct AuditResolver;
    impl AmbiguityResolver for AuditResolver {
        fn resolve(
            &self,
            _ambiguities: &[Ambiguity],
        ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
            Ok(Vec::new())
        }
    }

    let invoices = db
        .list_invoices_by_batch(batch_id)
        .map_err(|error| map_store_error("读取发票重建审计输入", error))?;
    let expenses = db
        .list_expense_items_by_batch(batch_id)
        .map_err(|error| map_store_error("读取费用重建审计输入", error))?;
    let expenses_by_invoice = expenses
        .iter()
        .map(|expense| (expense.primary_invoice_id, expense))
        .collect::<HashMap<_, _>>();
    let home_city = db
        .get_setting("home_city")
        .map_err(|error| map_store_error("读取常驻城市", error))?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::validation("请先在设置中填写常驻城市"))?;
    let home_station_aliases =
        super::settings::load_effective_home_station_aliases(db, &home_city)?;

    let mut reparsed_invoices = Vec::new();
    let mut parse_failure_invoice_ids = Vec::new();
    let mut core_field_mismatch_invoice_ids = Vec::new();
    let mut automatic_category_mismatch_invoice_ids = Vec::new();
    let mut source_city_invoice_ids = Vec::new();
    let mut manual_decision_expense_count = 0usize;

    for invoice in &invoices {
        let Some(expense) = expenses_by_invoice.get(&invoice.id).copied() else {
            core_field_mismatch_invoice_ids.push(invoice.id);
            continue;
        };
        if expense.inclusion_status != "included" || expense.category_source == "manual_review" {
            manual_decision_expense_count += 1;
        }
        let mut parsed = match super::invoice::do_parse(
            &invoice.file_path,
            Some(invoice.ticket_type.to_str()),
        ) {
            Ok(parsed) => parsed,
            Err(_) => {
                parse_failure_invoice_ids.push(invoice.id);
                continue;
            }
        };
        if parsed.invoice_number != invoice.invoice_number
            || parsed.issue_date != invoice.issue_date
            || parsed.total_amount != invoice.amount
        {
            core_field_mismatch_invoice_ids.push(invoice.id);
        }

        let source_category = category_from_invoice_file(Path::new(&invoice.file_path))
            .and_then(detected_category_code)
            .or_else(|| detected_category_code(parsed.ticket_type))
            .or_else(|| {
                parsed
                    .seller_name
                    .as_deref()
                    .and_then(invoice_parse::expense_classifier::classify_merchant_name)
                    .and_then(detected_category_code)
            })
            .unwrap_or("other");
        if expense.category_source != "manual_review" && source_category != expense.category_code {
            automatic_category_mismatch_invoice_ids.push(invoice.id);
        }
        // 费用清单是归组使用的稳定产品模型。人工确认的分类也是可审计、
        // 可重放的用户决定，重建时必须应用，不能退回 reported_invoices 的旧类型。
        parsed.ticket_type = parse_expense_category_code(&expense.category_code);
        reparsed_invoices.push((invoice.id, parsed));
    }

    let (supporting_document_count, recognized_supporting_document_count) =
        enrich_reparsed_invoices_from_attached_documents(&mut reparsed_invoices, &expenses);
    let stored_invoices = invoices
        .iter()
        .map(|invoice| (invoice.id, invoice))
        .collect::<HashMap<_, _>>();
    for (invoice_id, parsed) in &reparsed_invoices {
        if stored_invoices
            .get(invoice_id)
            .is_some_and(|invoice| invoice.city.is_none() && parsed.city.is_some())
        {
            source_city_invoice_ids.push(*invoice_id);
        }
    }
    let included_ids = expenses
        .iter()
        .filter(|expense| expense.inclusion_status == "included")
        .map(|expense| expense.primary_invoice_id)
        .collect::<HashSet<_>>();
    let parsed_included_invoice_ids = reparsed_invoices
        .iter()
        .filter(|(invoice_id, _)| included_ids.contains(invoice_id))
        .map(|(invoice_id, _)| *invoice_id)
        .collect::<Vec<_>>();
    let parsed_included = reparsed_invoices
        .iter()
        .filter(|(invoice_id, _)| included_ids.contains(invoice_id))
        .map(|(_, parsed)| parsed.clone())
        .collect::<Vec<_>>();

    let grouped = group_invoices(
        &parsed_included,
        &GroupingConfig {
            home_cities: vec![home_city],
            home_station_aliases: Some(home_station_aliases),
            ambiguity_handler: Box::new(AuditResolver),
        },
    )
    .map_err(|_| AppError::internal("重建审计归组失败"))?;
    let recreated_business_trip_count = grouped
        .trips
        .iter()
        .filter(|trip| matches!(trip.kind, TripKind::BusinessTrip { .. }))
        .count();
    let mut recreated_business_trip_invoice_ids = grouped
        .trips
        .iter()
        .filter(|trip| matches!(trip.kind, TripKind::BusinessTrip { .. }))
        .flat_map(|trip| &trip.invoice_ids)
        .filter_map(|input_index| parsed_included_invoice_ids.get(*input_index).copied())
        .collect::<Vec<_>>();
    recreated_business_trip_invoice_ids.sort_unstable();
    let source_city_ids = source_city_invoice_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut source_city_grouped_invoice_ids = grouped
        .trips
        .iter()
        .filter(|trip| matches!(trip.kind, TripKind::BusinessTrip { .. }))
        .flat_map(|trip| &trip.invoice_ids)
        .filter_map(|input_index| parsed_included_invoice_ids.get(*input_index).copied())
        .filter(|invoice_id| source_city_ids.contains(invoice_id))
        .collect::<Vec<_>>();
    source_city_grouped_invoice_ids.sort_unstable();
    source_city_grouped_invoice_ids.dedup();

    let mut rebuilt_member_sets = grouped
        .trips
        .iter()
        .map(|trip| {
            let mut ids = trip
                .invoice_ids
                .iter()
                .filter_map(|input_index| parsed_included_invoice_ids.get(*input_index).copied())
                .collect::<Vec<_>>();
            ids.sort_unstable();
            ids
        })
        .collect::<Vec<_>>();
    rebuilt_member_sets.sort();
    let current_grouping = db
        .get_batch_grouping(batch_id)
        .map_err(|error| map_store_error("读取当前归组用于重建审计", error))?;
    let mut current_business_trip_invoice_ids = current_grouping
        .as_ref()
        .into_iter()
        .flat_map(|grouping| &grouping.groups)
        .filter(|group| group.kind == "business_trip")
        .flat_map(|group| &group.members)
        .map(|member| member.invoice_id)
        .collect::<Vec<_>>();
    current_business_trip_invoice_ids.sort_unstable();
    let mut current_member_sets = current_grouping
        .as_ref()
        .map(|grouping| {
            grouping
                .groups
                .iter()
                .map(|group| {
                    let mut ids = group
                        .members
                        .iter()
                        .map(|member| member.invoice_id)
                        .collect::<Vec<_>>();
                    ids.sort_unstable();
                    ids
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    current_member_sets.sort();
    let current_grouping_matches_rebuild = current_member_sets == rebuilt_member_sets;
    let reproducible = parse_failure_invoice_ids.is_empty()
        && core_field_mismatch_invoice_ids.is_empty()
        && automatic_category_mismatch_invoice_ids.is_empty()
        && current_grouping_matches_rebuild
        && parsed_included.len()
            == expenses
                .iter()
                .filter(|expense| expense.inclusion_status == "included")
                .count();

    Ok(BatchSourceRebuildAuditResult {
        batch_id,
        source_invoice_count: invoices.len(),
        reparsed_invoice_count: invoices.len() - parse_failure_invoice_ids.len(),
        parse_failure_invoice_ids,
        core_field_mismatch_invoice_ids,
        automatic_category_mismatch_invoice_ids,
        manual_decision_expense_count,
        supporting_document_count,
        recognized_supporting_document_count,
        source_city_invoice_ids,
        source_city_grouped_invoice_ids,
        recreated_business_trip_invoice_ids,
        current_business_trip_invoice_ids,
        recreated_group_count: grouped.trips.len(),
        recreated_business_trip_count,
        current_grouping_matches_rebuild,
        reproducible,
    })
}

fn build_grouping_recompute(
    batch_id: i64,
    invoices: Vec<ReportedInvoice>,
    expenses: Vec<ExpenseItem>,
    home_city: String,
    home_station_aliases: Vec<StationCityAlias>,
    preserved_evidence: HashMap<GroupEvidenceKey, String>,
) -> AppResult<(NewBatchGrouping, GroupingRecomputeResult)> {
    struct ManualResolver;
    impl AmbiguityResolver for ManualResolver {
        fn resolve(
            &self,
            _ambiguities: &[Ambiguity],
        ) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
            Ok(Vec::new())
        }
    }

    let mut unresolved_transport_count = 0usize;
    let expense_categories = expenses
        .iter()
        .map(|expense| {
            (
                expense.primary_invoice_id,
                parse_expense_category_code(&expense.category_code),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut parsed_with_ids = invoices
        .iter()
        .map(|invoice| {
            let mut fact = grouping_source_invoice(invoice);
            if let Some(category) = expense_categories.get(&invoice.id).copied() {
                fact.ticket_type = category;
            }
            let is_transport = matches!(
                fact.ticket_type,
                ParseTicketType::Rail | ParseTicketType::Flight
            );
            if is_transport || fact.city.is_none() {
                if let Ok(reparsed) =
                    super::invoice::do_parse(&invoice.file_path, Some(invoice.ticket_type.to_str()))
                {
                    if fact.city.is_none() {
                        fact.city = reparsed.city.clone();
                    }
                    if fact.checkin_date.is_none() {
                        fact.checkin_date = reparsed.checkin_date;
                    }
                    if fact.departure_time.is_none() {
                        fact.departure_time = reparsed.departure_time;
                    }
                    if is_transport {
                        fact.travel_route = reparsed.travel_route;
                        fact.transport_document_kind = reparsed.transport_document_kind;
                    }
                }
            }
            if is_transport && fact.travel_route.is_none() {
                unresolved_transport_count += 1;
                fact.parse_level = ParseLevel::L4;
                fact.confidence = 0.0;
            }
            (invoice.id, fact)
        })
        .collect::<Vec<_>>();
    // 行程单和酒店明细提供的实际日期、城市属于可重复读取的源文件事实。
    // 每次重新归组都从已挂载材料恢复这些事实，不能依赖上一次写入数据库的结果。
    enrich_reparsed_invoices_from_attached_documents(&mut parsed_with_ids, &expenses);
    let parsed = parsed_with_ids
        .into_iter()
        .map(|(_, invoice)| invoice)
        .collect::<Vec<_>>();
    let grouped = group_invoices(
        &parsed,
        &GroupingConfig {
            home_cities: vec![home_city.clone()],
            home_station_aliases: Some(home_station_aliases.clone()),
            ambiguity_handler: Box::new(ManualResolver),
        },
    )
    .map_err(|_| AppError::internal("重新计算归组失败"))?;
    let business_trip_count = grouped
        .trips
        .iter()
        .filter(|trip| matches!(&trip.kind, TripKind::BusinessTrip { .. }))
        .count();
    let groups = grouped
        .trips
        .iter()
        .enumerate()
        .map(|(group_index, trip)| {
            let (kind, title) = regroup_kind(&trip.kind);
            let active_transport_input_indexes = trip
                .invoice_ids
                .iter()
                .copied()
                .filter(|input_index| {
                    let invoice = &parsed[*input_index];
                    matches!(
                        invoice.ticket_type,
                        ParseTicketType::Rail | ParseTicketType::Flight
                    ) && invoice.transport_document_kind.is_route_anchor()
                })
                .collect::<Vec<_>>();
            let adjustment_input_indexes = trip
                .invoice_ids
                .iter()
                .copied()
                .filter(|input_index| {
                    matches!(
                        parsed[*input_index].transport_document_kind,
                        TransportDocumentKind::Refund | TransportDocumentKind::Change
                    )
                })
                .collect::<Vec<_>>();
            let transport_routes = trip
                .invoice_ids
                .iter()
                .filter_map(|input_index| {
                    parsed[*input_index].travel_route.as_ref().map(|route| {
                        serde_json::json!({
                            "inputIndex": input_index,
                            "route": route,
                        })
                    })
                })
                .collect::<Vec<_>>();
            let automatic_transport_evidence_status = if kind == "business_trip" {
                if active_transport_input_indexes.is_empty() {
                    "missing"
                } else {
                    "present"
                }
            } else {
                "not_applicable"
            };
            let evidence_key = GroupEvidenceKey {
                title: title.clone(),
                start_date: trip.start_date.to_string(),
                end_date: trip.end_date.to_string(),
            };
            let transport_evidence_status = preserved_evidence
                .get(&evidence_key)
                .map(String::as_str)
                .unwrap_or(automatic_transport_evidence_status);
            let members = trip
                .invoice_ids
                .iter()
                .map(|input_index| NewInvoiceGroupMember {
                    invoice_id: invoices[*input_index].id,
                    input_index: *input_index,
                    match_reason: grouping_member_reason(&parsed[*input_index]),
                })
                .collect();
            NewInvoiceGroup {
                group_index,
                kind,
                title,
                start_date: trip.start_date.to_string(),
                end_date: trip.end_date.to_string(),
                confidence: trip.confidence,
                // 重新分析属于一次明确的审核动作，用户必须查看结果后确认。
                requires_review: true,
                evidence_json: serde_json::json!({
                    "source": "manual_route_recompute",
                    "ruleVersion": GROUPING_RULE_VERSION,
                    "homeStationAliasCount": home_station_aliases.len(),
                    "tripKind": &trip.kind,
                    "transportEvidenceStatus": transport_evidence_status,
                    "activeTransportInputIndexes": active_transport_input_indexes,
                    "transportAdjustmentInputIndexes": adjustment_input_indexes,
                    "transportRoutes": transport_routes,
                })
                .to_string(),
                members,
            }
        })
        .collect::<Vec<_>>();
    let group_count = groups.len();
    let accepted_without_personal_transport = groups
        .iter()
        .filter(|group| {
            serde_json::from_str::<serde_json::Value>(&group.evidence_json)
                .ok()
                .and_then(|evidence| {
                    evidence
                        .get("transportEvidenceStatus")
                        .and_then(serde_json::Value::as_str)
                        .map(|status| matches!(status, "company_paid" | "not_required"))
                })
                .unwrap_or(false)
        })
        .map(|group| {
            group
                .members
                .iter()
                .map(|member| member.input_index)
                .collect::<HashSet<_>>()
        })
        .collect::<Vec<_>>();
    let effective_ambiguities = grouped
        .ambiguities
        .iter()
        .filter(|ambiguity| {
            !matches!(ambiguity.kind, AmbiguityKind::MissingTransportEvidence)
                || !accepted_without_personal_transport
                    .iter()
                    .any(|member_indexes| {
                        !ambiguity.involved_invoice_ids.is_empty()
                            && ambiguity
                                .involved_invoice_ids
                                .iter()
                                .all(|input_index| member_indexes.contains(input_index))
                    })
        })
        .collect::<Vec<_>>();
    let grouping = NewBatchGrouping {
        batch_id,
        rule_version: format!("{GROUPING_RULE_VERSION}-route-recompute"),
        home_cities_json: serde_json::to_string(&vec![home_city])
            .map_err(|_| AppError::internal("序列化常驻城市失败"))?,
        overall_confidence: grouped.overall_confidence,
        ambiguities_json: serde_json::to_string(&effective_ambiguities)
            .map_err(|_| AppError::internal("序列化归组待确认项失败"))?,
        groups,
    };
    Ok((
        grouping,
        GroupingRecomputeResult {
            invoice_count: parsed.len(),
            group_count,
            business_trip_count,
            unresolved_transport_count,
        },
    ))
}

/// Maintenance entry point that applies the exact same regrouping path as the UI command.
/// The caller is responsible for closing the application and backing up the database first.
#[allow(dead_code)]
pub fn recompute_batch_grouping_for_ledger(
    db: &LedgerDb,
    batch_id: i64,
) -> AppResult<GroupingRecomputeResult> {
    let (invoices, expenses, home_city, home_station_aliases) =
        load_grouping_recompute_inputs(db, batch_id)?;
    let existing_grouping = db
        .get_batch_grouping(batch_id)
        .map_err(|error| map_store_error("读取现有归组证据", error))?;
    let (grouping, result) = build_grouping_recompute(
        batch_id,
        invoices,
        expenses,
        home_city,
        home_station_aliases,
        preserved_transport_evidence(existing_grouping.as_ref()),
    )?;
    db.replace_batch_grouping_with_audit(&grouping)
        .map_err(|error| map_store_error("保存重新分析的归组", error))?;
    Ok(result)
}

#[tauri::command]
pub async fn recompute_batch_grouping(
    batch_id: i64,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<GroupingRecomputeResult> {
    let (invoices, expenses, home_city, home_station_aliases, preserved_evidence) = {
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        let db = app_state.ledger_db()?;
        let (invoices, expenses, home_city, home_station_aliases) =
            load_grouping_recompute_inputs(db, batch_id)?;
        let existing_grouping = db
            .get_batch_grouping(batch_id)
            .map_err(|error| map_store_error("读取现有归组证据", error))?;
        (
            invoices,
            expenses,
            home_city,
            home_station_aliases,
            preserved_transport_evidence(existing_grouping.as_ref()),
        )
    };
    let (grouping, result) = tauri::async_runtime::spawn_blocking(move || {
        build_grouping_recompute(
            batch_id,
            invoices,
            expenses,
            home_city,
            home_station_aliases,
            preserved_evidence,
        )
    })
    .await
    .map_err(|_| AppError::internal("重新分析归组的工作线程异常终止"))??;

    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .replace_batch_grouping_with_audit(&grouping)
        .map_err(|error| map_store_error("保存重新分析的归组", error))?;
    Ok(result)
}

#[tauri::command]
pub fn reanalyze_expense_categories(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<ExpenseCategoryReanalysisResult> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    reanalyze_expense_categories_for_ledger(app_state.ledger_db()?, batch_id)
}

#[tauri::command]
pub fn confirm_grouping(batch_id: i64, state: State<Mutex<AppState>>) -> AppResult<()> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .confirm_batch_grouping(batch_id)
        .map_err(|error| match error {
            StoreError::Validation(message) if message.contains("lacks an intercity") => {
                AppError::validation("差旅行程必须至少包含一张铁路/航空票，或一份已挂载行程单")
            }
            other => map_store_error("确认归组", other),
        })
}

#[tauri::command]
pub fn confirm_invoice_group(
    batch_id: i64,
    group_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .confirm_invoice_group(batch_id, group_id)
        .map_err(|error| match error {
            StoreError::Validation(message) if message.contains("lacks an intercity") => {
                AppError::validation("差旅行程必须至少包含一张铁路/航空票，或一份已挂载行程单")
            }
            other => map_store_error("确认归组", other),
        })
}

#[tauri::command]
pub fn list_review_actions(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<ReviewAction>> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .list_review_actions(batch_id)
        .map_err(|error| map_store_error("读取审核记录", error))
}

#[tauri::command]
pub fn undo_last_review_action(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<ReviewAction> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .undo_last_review_action(batch_id)
        .map_err(|error| map_store_error("撤销上一步", error))
}

/// 完成审核并冻结一个不可变版本。导出与 Concur 交付从此版本读取。
#[tauri::command]
pub fn complete_batch_review(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<BatchReviewSnapshot> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .complete_batch_review(batch_id)
        .map_err(|error| match error {
            StoreError::Validation(message) if message.contains("no reimbursable") => {
                AppError::validation("批次中没有可计入报销的费用")
            }
            StoreError::Validation(message) if message.contains("unconfirmed transaction date") => {
                AppError::validation("仍有计入报销的费用未确认实际发生日期")
            }
            StoreError::Validation(message) if message.contains("unconfirmed expense category") => {
                AppError::validation("仍有计入报销的费用未确认费用类型")
            }
            StoreError::Validation(message) if message.contains("unresolved pending document") => {
                AppError::validation("仍有待挂载材料未处理，请挂载到费用或明确忽略")
            }
            StoreError::Validation(message) if message.contains("unresolved actionable email") => {
                AppError::validation("邮件台账中仍有需要下载、确认或重试的邮件")
            }
            StoreError::Validation(message) if message.contains("lacks an intercity") => {
                AppError::validation(
                    "差旅行程必须至少包含一张已计入的铁路/航空票，或一份已挂载行程单",
                )
            }
            StoreError::Validation(message) if message.contains("grouping") => {
                AppError::validation("归组仍有待确认项，请先完成归组审核")
            }
            other => map_store_error("完成审核", other),
        })
}

#[tauri::command]
pub fn get_active_review_snapshot(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Option<BatchReviewSnapshot>> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .get_active_batch_review_snapshot(batch_id)
        .map_err(|error| map_store_error("读取审核版本", error))
}

#[tauri::command]
pub fn reopen_batch_review(batch_id: i64, state: State<Mutex<AppState>>) -> AppResult<()> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .reopen_batch_review(batch_id)
        .map_err(|error| map_store_error("重新打开审核", error))
}

#[tauri::command]
pub fn list_delivery_tasks(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<DeliveryTask>> {
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .list_delivery_tasks(batch_id)
        .map_err(|error| map_store_error("读取交付记录", error))
}

/// Concur 交付适配器调用前登记幂等任务。Excel 由导出命令内部登记。
#[tauri::command]
pub async fn start_concur_delivery(batch_id: i64, app: AppHandle) -> AppResult<DeliveryTask> {
    let (task, upload_session_id) = {
        let state = app.state::<Mutex<AppState>>();
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        let concur = app_state
            .concur_session()
            .filter(|session| session.draft_workflow_verified)
            .ok_or_else(|| AppError::validation("请先到“设置 → Concur 能力”完成草稿闭环测试"))?;
        let _ = concur;
        let db = app_state.ledger_db()?;
        let upload_session = db
            .list_concur_upload_sessions(batch_id)
            .map_err(|error| map_store_error("读取 Concur 上传会话", error))?
            .into_iter()
            .find(|session| {
                matches!(
                    session.status.as_str(),
                    "ready" | "failed" | "partial" | "draft_created"
                )
            })
            .ok_or_else(|| {
                AppError::validation(
                    "没有可执行的 Concur 上传会话；请先完成映射预检，未知状态需先人工核对",
                )
            })?;
        let task = db
            .start_delivery_task(batch_id, "concur")
            .map_err(|error| map_store_error("准备 Concur 交付", error))?;
        (task, upload_session.id)
    };
    if task.status == "succeeded" {
        return Ok(task);
    }

    let worker_app = app.clone();
    let upload_result = tauri::async_runtime::spawn_blocking(move || {
        execute_concur_upload(&worker_app, upload_session_id)
    })
    .await
    .map_err(|_| AppError::internal("Concur 草稿交付线程异常"))?;

    let state = app.state::<Mutex<AppState>>();
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    let db = app_state.ledger_db()?;
    match upload_result {
        Ok(report_id) => db
            .finish_delivery_task(task.id, Some(&format!("concur:{report_id}")), None)
            .map_err(|error| map_store_error("记录 Concur 交付结果", error)),
        Err(error) => {
            let message = error.message().chars().take(1_900).collect::<String>();
            db.finish_delivery_task(task.id, None, Some(&message))
                .map_err(|store_error| map_store_error("记录 Concur 交付失败", store_error))?;
            Err(error)
        }
    }
}

fn execute_concur_upload(app: &AppHandle, upload_session_id: i64) -> AppResult<String> {
    let (api_session, mut upload_status, expenses, invoices) = {
        let state = app.state::<Mutex<AppState>>();
        let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
        let api_session = app_state
            .concur_session_copy()
            .filter(|session| session.draft_workflow_verified)
            .ok_or_else(|| AppError::validation("Concur 能力验证已失效，请重新连接"))?;
        let db = app_state.ledger_db()?;
        let upload_status = db
            .get_concur_upload_status(upload_session_id)
            .map_err(|error| map_store_error("读取 Concur 上传计划", error))?
            .ok_or_else(|| AppError::validation("Concur 上传会话不存在"))?;
        let mapped_session =
            serde_json::from_str::<serde_json::Value>(&upload_status.session.mapped_payload_json)
                .map_err(|_| AppError::internal("冻结报销单投影已损坏"))?;
        if mapped_session
            .pointer("/mapping_profile/adapter_kind")
            .and_then(serde_json::Value::as_str)
            != Some("api")
        {
            return Err(AppError::validation(
                "当前预检使用的不是企业 API 映射配置；请选择企业 API 并重新预检",
            ));
        }
        if upload_status.session.status == "needs_verification" {
            return Err(AppError::validation(
                "上次外部写入结果不确定，请先在会话进度中完成 Concur 人工核对",
            ));
        }
        let (snapshot, expenses) = db
            .get_active_snapshot_expenses(upload_status.session.batch_id)
            .map_err(|error| map_store_error("读取冻结费用版本", error))?;
        if snapshot.id != upload_status.session.review_snapshot_id {
            return Err(AppError::validation(
                "上传预检引用的审核版本已失效，请重新执行 Concur 预检",
            ));
        }
        let (_, invoices) = db
            .get_active_snapshot_invoices(upload_status.session.batch_id)
            .map_err(|error| map_store_error("读取冻结发票版本", error))?;
        (api_session, upload_status, expenses, invoices)
    };
    let api = crate::concur_api::ConcurApiClient::from_session(&api_session)?;

    let report_id = if let Some(report_id) = upload_status.session.external_report_id.clone() {
        report_id
    } else {
        {
            let state = app.state::<Mutex<AppState>>();
            let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
            app_state
                .ledger_db()?
                .reserve_concur_report_creation(upload_session_id)
                .map_err(|error| map_store_error("锁定报销单创建步骤", error))?;
        }
        let mapped_session =
            serde_json::from_str::<serde_json::Value>(&upload_status.session.mapped_payload_json)
                .map_err(|_| AppError::internal("冻结报销单投影已损坏"))?;
        match api.create_report(
            &upload_status.session.report_name,
            &upload_status
                .session
                .report_date
                .format(DATE_FMT)
                .to_string(),
            mapped_session.get("report"),
        ) {
            Ok(report_id) => {
                let state = app.state::<Mutex<AppState>>();
                let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
                app_state
                    .ledger_db()?
                    .mark_concur_report_created(upload_session_id, &report_id)
                    .map_err(|error| map_store_error("保存 Concur 报销单 ID", error))?;
                report_id
            }
            Err(error) => {
                let state = app.state::<Mutex<AppState>>();
                let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
                app_state
                    .ledger_db()?
                    .mark_concur_report_attempt_failed(
                        upload_session_id,
                        &error.message,
                        error.result_unknown,
                    )
                    .map_err(|store_error| map_store_error("记录报销单创建失败", store_error))?;
                return Err(concur_call_error(error));
            }
        }
    };
    let report = api.get_report(&report_id).map_err(concur_call_error)?;
    if report
        .get("ApprovalStatusCode")
        .and_then(serde_json::Value::as_str)
        != Some("A_NOTF")
    {
        return Err(AppError::validation(
            "Concur 报销单不是未提交状态，已停止写入；请在 Concur 人工核对",
        ));
    }

    upload_status = current_concur_upload_status(app, upload_session_id)?;
    let expenses_by_id = expenses
        .iter()
        .map(|expense| (expense.id, expense))
        .collect::<HashMap<_, _>>();
    for item in upload_status.items.clone() {
        let expense = expenses_by_id
            .get(&item.expense_item_id)
            .copied()
            .ok_or_else(|| {
                AppError::validation(format!(
                    "冻结审核版本中找不到费用 #{}，请重新执行预检",
                    item.expense_item_id
                ))
            })?;
        let external_expense_id = if let Some(expense_id) = item.external_expense_id.clone() {
            expense_id
        } else {
            {
                let state = app.state::<Mutex<AppState>>();
                let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
                app_state
                    .ledger_db()?
                    .reserve_concur_expense_creation(item.id)
                    .map_err(|error| map_store_error("锁定费用创建步骤", error))?;
            }
            let target_fields =
                serde_json::from_str::<serde_json::Value>(&item.mapped_payload_json)
                    .map_err(|_| AppError::internal("冻结费用投影已损坏"))?;
            match api.create_expense(&report_id, &target_fields) {
                Ok(expense_id) => {
                    let state = app.state::<Mutex<AppState>>();
                    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
                    app_state
                        .ledger_db()?
                        .mark_concur_expense_created(item.id, &expense_id)
                        .map_err(|error| map_store_error("保存 Concur 费用 ID", error))?;
                    expense_id
                }
                Err(error) => {
                    let state = app.state::<Mutex<AppState>>();
                    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
                    app_state
                        .ledger_db()?
                        .mark_concur_expense_attempt_failed(
                            item.id,
                            &error.message,
                            error.result_unknown,
                        )
                        .map_err(|store_error| map_store_error("记录费用创建失败", store_error))?;
                    return Err(concur_call_error(error));
                }
            }
        };
        api.get_expense(&external_expense_id)
            .map_err(concur_call_error)?;

        let has_pending_attachments = item
            .attachments
            .iter()
            .any(|attachment| attachment.status != "uploaded");
        if has_pending_attachments {
            let attachment_pdf = crate::commands::print_export::build_concur_attachment_pdf_bytes(
                expense, &invoices,
            )?;
            let reserved_ids = {
                let state = app.state::<Mutex<AppState>>();
                let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
                app_state
                    .ledger_db()?
                    .reserve_concur_attachment_bundle(item.id)
                    .map_err(|error| map_store_error("锁定费用材料上传步骤", error))?
            };
            if !reserved_ids.is_empty() {
                match api.upload_expense_pdf(&external_expense_id, attachment_pdf) {
                    Ok(external_attachment_id) => {
                        if let Err(error) = api.verify_expense_image(&external_expense_id) {
                            let state = app.state::<Mutex<AppState>>();
                            let app_state =
                                state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
                            app_state
                                .ledger_db()?
                                .mark_concur_attachment_bundle_attempt_failed(
                                    item.id,
                                    &format!(
                                        "附件已返回 ID {external_attachment_id}，但回读失败：{}",
                                        error.message
                                    ),
                                    true,
                                )
                                .map_err(|store_error| {
                                    map_store_error("记录附件回读失败", store_error)
                                })?;
                            return Err(concur_call_error(error));
                        }
                        let state = app.state::<Mutex<AppState>>();
                        let app_state =
                            state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
                        app_state
                            .ledger_db()?
                            .mark_concur_attachment_bundle_uploaded(
                                item.id,
                                &external_attachment_id,
                            )
                            .map_err(|error| map_store_error("保存 Concur 附件 ID", error))?;
                    }
                    Err(error) => {
                        let state = app.state::<Mutex<AppState>>();
                        let app_state =
                            state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
                        app_state
                            .ledger_db()?
                            .mark_concur_attachment_bundle_attempt_failed(
                                item.id,
                                &error.message,
                                error.result_unknown,
                            )
                            .map_err(|store_error| {
                                map_store_error("记录附件上传失败", store_error)
                            })?;
                        return Err(concur_call_error(error));
                    }
                }
            }
        }
    }

    let final_status = current_concur_upload_status(app, upload_session_id)?;
    if final_status.session.status != "draft_created" {
        return Err(AppError::validation(
            "Concur 草稿只完成了部分步骤，请查看本批次上传会话后继续",
        ));
    }
    Ok(report_id)
}

fn current_concur_upload_status(
    app: &AppHandle,
    upload_session_id: i64,
) -> AppResult<ConcurUploadStatus> {
    let state = app.state::<Mutex<AppState>>();
    let app_state = state.lock().map_err(|_| AppError::internal("状态锁错误"))?;
    app_state
        .ledger_db()?
        .get_concur_upload_status(upload_session_id)
        .map_err(|error| map_store_error("刷新 Concur 上传进度", error))?
        .ok_or_else(|| AppError::validation("Concur 上传会话不存在"))
}

fn concur_call_error(error: crate::concur_api::ConcurApiCallError) -> AppError {
    if error.result_unknown {
        AppError::network(format!(
            "{}；外部结果不确定，请先在 Concur 核对后再处理",
            error.message
        ))
    } else {
        AppError::network(error.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use invoice_store::models::{BatchGrouping, InvoiceGroup};

    fn valid_input() -> InvoiceReviewInput {
        InvoiceReviewInput {
            invoice_number: "12345678901234567890".to_string(),
            issue_date: "2026-06-01".to_string(),
            amount: "100.50".to_string(),
            tax_amount: Some("9.50".to_string()),
            buyer_name: None,
            seller_name: None,
            ticket_type: "rail".to_string(),
            city: Some("北京".to_string()),
            departure_time: Some("2026-06-01T08:30".to_string()),
            checkin_date: None,
        }
    }

    #[test]
    fn preview_format_allows_only_bounded_passive_formats() {
        assert_eq!(preview_format("pdf"), (Some("application/pdf"), "pdf"));
        assert_eq!(preview_format("png"), (Some("image/png"), "image"));
        assert_eq!(preview_format("xml"), (Some("application/xml"), "text"));
        assert_eq!(preview_format("svg"), (None, "unsupported"));
        assert_eq!(preview_format("ofd"), (Some("application/ofd"), "ofd"));
    }

    #[test]
    fn parses_review_input_without_float_conversion() {
        let parsed = parse_review_update(valid_input()).unwrap();
        assert_eq!(parsed.amount, Decimal::from_str("100.50").unwrap());
        assert_eq!(parsed.tax_amount, Some(Decimal::from_str("9.50").unwrap()));
        assert_eq!(parsed.ticket_type, TicketType::Rail);
        assert_eq!(
            parsed.departure_time.unwrap(),
            NaiveDateTime::parse_from_str("2026-06-01 08:30:00", DATETIME_FMT).unwrap()
        );
    }

    #[test]
    fn rejects_unknown_ticket_type_and_bad_date() {
        let mut input = valid_input();
        input.ticket_type = "Rail".to_string();
        assert_eq!(
            parse_review_update(input).unwrap_err().kind(),
            ErrorKind::Validation
        );
        let mut input = valid_input();
        input.issue_date = "2026-02-30".to_string();
        assert_eq!(
            parse_review_update(input).unwrap_err().kind(),
            ErrorKind::Validation
        );
    }

    #[test]
    fn preserves_only_explicit_business_trip_transport_decisions() {
        let grouping = BatchGrouping {
            batch_id: 8,
            rule_version: "test".to_string(),
            home_cities_json: "[]".to_string(),
            overall_confidence: 1.0,
            ambiguities_json: "[]".to_string(),
            created_at: "2026-09-02 00:00:00".to_string(),
            groups: vec![
                InvoiceGroup {
                    id: 1,
                    group_index: 0,
                    kind: "business_trip".to_string(),
                    title: "2026-06-01 上海出差".to_string(),
                    start_date: "2026-06-01".to_string(),
                    end_date: "2026-06-02".to_string(),
                    confidence: 1.0,
                    requires_review: true,
                    evidence_json: serde_json::json!({
                        "transportEvidenceStatus": "company_paid"
                    })
                    .to_string(),
                    members: Vec::new(),
                },
                InvoiceGroup {
                    id: 2,
                    group_index: 1,
                    kind: "business_trip".to_string(),
                    title: "2026-06-03 天津出差".to_string(),
                    start_date: "2026-06-03".to_string(),
                    end_date: "2026-06-03".to_string(),
                    confidence: 1.0,
                    requires_review: true,
                    evidence_json: serde_json::json!({
                        "transportEvidenceStatus": "missing"
                    })
                    .to_string(),
                    members: Vec::new(),
                },
            ],
        };

        let preserved = preserved_transport_evidence(Some(&grouping));

        assert_eq!(preserved.len(), 1);
        assert_eq!(
            preserved
                .get(&GroupEvidenceKey {
                    title: "2026-06-01 上海出差".to_string(),
                    start_date: "2026-06-01".to_string(),
                    end_date: "2026-06-02".to_string(),
                })
                .map(String::as_str),
            Some("company_paid")
        );
    }
}
