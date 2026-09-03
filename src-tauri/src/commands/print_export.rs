//! 基于冻结审核快照生成可直接打印的材料合订 PDF。
//!
//! 每条计入费用及其 `invoice_documents` 挂载关系来自审核快照；不重新读取可变草稿。
//! PDF、图片、OFD 和 XML 分别转换为 A4 页面。打印文件只包含材料页面；
//! 无法安全渲染的材料通过命令结果和进度事件反馈，不占用额外打印页。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use image::{self, DynamicImage, GenericImageView};
use invoice_store::models::{
    Batch, BatchReviewSnapshot, ExpenseItem, InvoiceDocument, ReportedInvoice,
};
use printpdf::{
    ColorBits, ColorSpace, Image, ImageFilter, ImageTransform, ImageXObject, IndirectFontRef, Mm,
    PdfDocument, PdfDocumentReference, PdfLayerReference, Px,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::AppState;

const SOURCE_HAN_SANS_CN_VARIABLE: &[u8] =
    include_bytes!("../../assets/fonts/SourceHanSansCN-VF.ttf");
const MAX_SOURCE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 50_000_000;
const MAX_MATERIAL_PAGES: usize = 600;
const PDF_RENDER_SIDE: u32 = 2_480;
const A4_WIDTH_MM: f32 = 210.0;
const A4_HEIGHT_MM: f32 = 297.0;
const CONTENT_LEFT_MM: f32 = 10.0;
const CONTENT_BOTTOM_MM: f32 = 12.0;
const CONTENT_WIDTH_MM: f32 = 190.0;
const CONTENT_HEIGHT_MM: f32 = 240.0;

#[derive(Debug, Clone, Serialize)]
pub struct PrintPdfExportResult {
    pub path: String,
    pub bytes: u64,
    pub review_version: i32,
    pub expense_count: usize,
    pub material_count: usize,
    pub rendered_material_count: usize,
    pub page_count: usize,
    pub warning_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PrintPdfProgress {
    export_id: String,
    batch_id: i64,
    phase: String,
    current: usize,
    total: usize,
    material_name: Option<String>,
    message: String,
}

struct PrintPdfBuild {
    bytes: Vec<u8>,
    material_count: usize,
    rendered_material_count: usize,
    page_count: usize,
    warnings: Vec<String>,
}

struct AppendResult {
    pages: usize,
    rendered: bool,
    warning: Option<String>,
}

impl AppendResult {
    fn success(pages: usize) -> Self {
        Self {
            pages,
            rendered: true,
            warning: None,
        }
    }

    fn warning(pages: usize, message: impl Into<String>) -> Self {
        Self {
            pages,
            rendered: pages > 0,
            warning: Some(message.into()),
        }
    }
}

/// 将当前活动审核快照生成一份包含全部费用材料的 A4 打印 PDF。
#[tauri::command]
pub async fn export_batch_print_pdf_to_path(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    batch_id: i64,
    destination_path: String,
    export_id: String,
) -> AppResult<PrintPdfExportResult> {
    let export_id = Uuid::parse_str(export_id.trim())
        .map_err(|_| AppError::validation("PDF 导出标识无效"))?
        .to_string();
    let destination = validate_pdf_destination(&destination_path)?;
    let output_path = destination.to_string_lossy().into_owned();
    let (batch, snapshot, expenses, invoices, task_id) = {
        let app_state = state
            .lock()
            .map_err(|_| AppError::internal("应用状态锁不可用"))?;
        let db = app_state.ledger_db()?;
        let batch = db
            .get_batch(batch_id)
            .map_err(|error| AppError::database(format!("获取批次失败: {error}")))?;
        let (snapshot, expenses) = db.get_active_snapshot_expenses(batch_id).map_err(|error| {
            AppError::validation(format!("请先完成审核并生成有效版本：{error}"))
        })?;
        let (_, invoices) = db.get_active_snapshot_invoices(batch_id).map_err(|error| {
            AppError::validation(format!("请先完成审核并生成有效版本：{error}"))
        })?;
        let task = db
            .start_delivery_task(batch_id, "pdf")
            .map_err(|error| AppError::database(format!("创建 PDF 交付任务失败: {error}")))?;
        (batch, snapshot, expenses, invoices, task.id)
    };

    let review_version = snapshot.version;
    let expense_count = expenses.len();
    let material_total = expenses
        .iter()
        .flat_map(|expense| expense.documents.iter())
        .filter(|document| document.role != "duplicate_copy")
        .count();
    emit_print_progress(
        &app,
        &export_id,
        batch_id,
        "preparing",
        0,
        material_total,
        None,
        "正在准备审核快照中的有效材料",
    );
    let worker_app = app.clone();
    let worker_export_id = export_id.clone();
    let build_result = match tauri::async_runtime::spawn_blocking(move || {
        let build = build_print_pdf_bytes_with_progress(
            &batch,
            &snapshot,
            &expenses,
            &invoices,
            |current, total, material_name| {
                emit_print_progress(
                    &worker_app,
                    &worker_export_id,
                    batch_id,
                    "converting",
                    current,
                    total,
                    Some(material_name),
                    "正在转换报销材料",
                );
            },
        )?;
        emit_print_progress(
            &worker_app,
            &worker_export_id,
            batch_id,
            "writing",
            material_total,
            material_total,
            None,
            "正在写入打印 PDF",
        );
        write_pdf_atomically(&destination, &build.bytes)?;
        emit_print_progress(
            &worker_app,
            &worker_export_id,
            batch_id,
            "verifying",
            material_total,
            material_total,
            None,
            "正在校验导出的 PDF",
        );
        verify_saved_pdf(&destination, build.page_count)?;
        Ok::<_, AppError>(build)
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(AppError::internal("打印 PDF 生成线程异常")),
    };

    match build_result {
        Ok(build) => {
            let app_state = state
                .lock()
                .map_err(|_| AppError::internal("应用状态锁不可用"))?;
            app_state
                .ledger_db()?
                .finish_delivery_task(task_id, Some(&output_path), None)
                .map_err(|error| AppError::database(format!("记录 PDF 交付结果失败: {error}")))?;
            Ok(PrintPdfExportResult {
                path: output_path,
                bytes: u64::try_from(build.bytes.len())
                    .map_err(|_| AppError::internal("PDF 文件大小超出支持范围"))?,
                review_version,
                expense_count,
                material_count: build.material_count,
                rendered_material_count: build.rendered_material_count,
                page_count: build.page_count,
                warning_count: build.warnings.len(),
                warnings: build.warnings,
            })
            .map(|result| {
                emit_print_progress(
                    &app,
                    &export_id,
                    batch_id,
                    "completed",
                    result.material_count,
                    result.material_count,
                    None,
                    "打印 PDF 已导出并校验完成",
                );
                result
            })
        }
        Err(error) => {
            if let Ok(app_state) = state.lock() {
                if let Ok(db) = app_state.ledger_db() {
                    let _ = db.finish_delivery_task(task_id, None, Some(&error.to_string()));
                }
            }
            emit_print_progress(
                &app,
                &export_id,
                batch_id,
                "failed",
                0,
                material_total,
                None,
                error.message(),
            );
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_print_progress(
    app: &AppHandle,
    export_id: &str,
    batch_id: i64,
    phase: &str,
    current: usize,
    total: usize,
    material_name: Option<String>,
    message: &str,
) {
    let _ = app.emit(
        &format!("print-pdf:progress:{export_id}"),
        PrintPdfProgress {
            export_id: export_id.to_string(),
            batch_id,
            phase: phase.to_string(),
            current,
            total,
            material_name,
            message: message.to_string(),
        },
    );
}

fn validate_pdf_destination(raw: &str) -> AppResult<PathBuf> {
    let path = PathBuf::from(raw.trim());
    if !path.is_absolute()
        || !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("pdf"))
    {
        return Err(AppError::validation("请选择绝对路径并使用 .pdf 扩展名"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| AppError::validation("PDF 保存目录无效"))?;
    let metadata = fs::symlink_metadata(parent)
        .map_err(|error| AppError::io(format!("读取 PDF 保存目录失败（{}）", error.kind())))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::validation("PDF 保存目录必须是本地普通文件夹"));
    }
    if path.exists() {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| AppError::io(format!("读取目标 PDF 失败（{}）", error.kind())))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(AppError::validation("目标 PDF 必须是普通文件"));
        }
    }
    Ok(path)
}

fn write_pdf_atomically(destination: &Path, bytes: &[u8]) -> AppResult<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| AppError::validation("PDF 保存目录无效"))?;
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AppError::validation("PDF 文件名无效"))?;
    let nonce = Uuid::new_v4();
    let staged = parent.join(format!(".{file_name}.{nonce}.tmp"));
    let backup = parent.join(format!(".{file_name}.{nonce}.bak"));
    let result = (|| -> AppResult<()> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)
            .map_err(|error| AppError::io(format!("创建 PDF 临时文件失败（{}）", error.kind())))?;
        file.write_all(bytes)
            .map_err(|error| AppError::io(format!("写入 PDF 失败（{}）", error.kind())))?;
        file.sync_all()
            .map_err(|error| AppError::io(format!("同步 PDF 失败（{}）", error.kind())))?;
        drop(file);
        if destination.exists() {
            fs::rename(destination, &backup).map_err(|error| {
                AppError::io(format!("准备替换旧 PDF 失败（{}）", error.kind()))
            })?;
        }
        if let Err(error) = fs::rename(&staged, destination) {
            if backup.exists() {
                let _ = fs::rename(&backup, destination);
            }
            return Err(AppError::io(format!("保存 PDF 失败（{}）", error.kind())));
        }
        if backup.exists() {
            fs::remove_file(&backup)
                .map_err(|error| AppError::io(format!("清理旧 PDF 失败（{}）", error.kind())))?;
        }
        Ok(())
    })();
    if staged.exists() {
        let _ = fs::remove_file(staged);
    }
    result
}

#[cfg(test)]
fn build_print_pdf_bytes(
    batch: &Batch,
    snapshot: &BatchReviewSnapshot,
    expenses: &[ExpenseItem],
    invoices: &[ReportedInvoice],
) -> AppResult<PrintPdfBuild> {
    build_print_pdf_bytes_with_progress(batch, snapshot, expenses, invoices, |_, _, _| {})
}

fn build_print_pdf_bytes_with_progress(
    _batch: &Batch,
    _snapshot: &BatchReviewSnapshot,
    expenses: &[ExpenseItem],
    invoices: &[ReportedInvoice],
    mut on_progress: impl FnMut(usize, usize, String),
) -> AppResult<PrintPdfBuild> {
    // printpdf 必须先创建一个页面。该初始化页在写出前删除，最终文件第一页直接是材料。
    let (document, _, _) = PdfDocument::new(
        "Invoice Assistant Expense Materials",
        Mm(A4_WIDTH_MM),
        Mm(A4_HEIGHT_MM),
        "初始化页",
    );
    let font = document
        .add_external_font(std::io::Cursor::new(SOURCE_HAN_SANS_CN_VARIABLE))
        .map_err(|error| AppError::internal(format!("加载内置 PDF 中文字体失败: {error:?}")))?;

    let invoice_by_id = invoices
        .iter()
        .map(|invoice| (invoice.id, invoice))
        .collect::<HashMap<_, _>>();
    let mut sorted_expenses = expenses.iter().collect::<Vec<_>>();
    sorted_expenses.sort_by_key(|expense| {
        (
            expense.trip_group_id.unwrap_or(i64::MAX),
            category_rank(&expense.category_code),
            expense.transaction_date,
            expense.id,
        )
    });

    let expense_total = sorted_expenses.len();
    let total_materials = sorted_expenses
        .iter()
        .flat_map(|expense| expense.documents.iter())
        .filter(|material| material.role != "duplicate_copy")
        .count();
    let mut page_count = 0_usize;
    let mut material_page_count = 0_usize;
    let mut material_count = 0_usize;
    let mut rendered_material_count = 0_usize;
    let mut warnings = Vec::new();

    for (expense_index, expense) in sorted_expenses.iter().copied().enumerate() {
        let mut materials = expense
            .documents
            .iter()
            .filter(|item| item.role != "duplicate_copy")
            .collect::<Vec<_>>();
        materials.sort_by_key(|item| (role_rank(&item.role), item.id));
        if materials.is_empty() {
            warnings.push(format!(
                "费用 #{}（{}，{}）没有可打印的电子材料",
                expense.id, expense.transaction_date, expense.gross_amount
            ));
            continue;
        }

        for material in materials {
            material_count += 1;
            on_progress(
                material_count,
                total_materials,
                safe_name(&material.original_name),
            );
            let remaining = MAX_MATERIAL_PAGES.saturating_sub(material_page_count);
            let invoice = invoice_by_id.get(&expense.primary_invoice_id).copied();
            let appended = append_material(
                &document,
                &font,
                expense,
                invoice,
                material,
                expense_index + 1,
                expense_total,
                page_count + 1,
                remaining,
            );
            page_count += appended.pages;
            material_page_count += appended.pages;
            if appended.rendered {
                rendered_material_count += 1;
            }
            if let Some(message) = appended.warning {
                warnings.push(format!(
                    "费用 #{} · {}：{}",
                    expense.id,
                    safe_name(&material.original_name),
                    message
                ));
            }
        }
    }

    if page_count == 0 {
        let detail = warnings
            .first()
            .map(|warning| format!("：{warning}"))
            .unwrap_or_default();
        return Err(AppError::validation(format!(
            "当前审核版本没有可生成打印页面的有效材料{detail}"
        )));
    }

    let raw_bytes = document
        .save_to_bytes()
        .map_err(|error| AppError::internal(format!("生成打印 PDF 失败: {error:?}")))?;
    let mut parsed = printpdf::lopdf::Document::load_mem(&raw_bytes)
        .map_err(|_| AppError::internal("生成后的 PDF 无法重新打开"))?;
    let raw_pages = parsed.get_pages().len();
    if raw_pages != page_count + 1 {
        return Err(AppError::internal(format!(
            "生成后的 PDF 页数不一致（预计 {}，实际 {raw_pages}）",
            page_count + 1
        )));
    }
    parsed.delete_pages(&[1]);
    parsed.prune_objects();
    parsed.compress();
    let actual_pages = parsed.get_pages().len();
    if actual_pages != page_count {
        return Err(AppError::internal(format!(
            "生成后的 PDF 页数不一致（预计 {page_count}，实际 {actual_pages}）"
        )));
    }
    let mut bytes = Vec::new();
    parsed
        .save_to(&mut bytes)
        .map_err(|error| AppError::internal(format!("整理打印 PDF 失败: {error:?}")))?;
    let reopened = printpdf::lopdf::Document::load_mem(&bytes)
        .map_err(|_| AppError::internal("整理后的打印 PDF 无法重新打开"))?;
    if reopened.get_pages().len() != page_count {
        return Err(AppError::internal("整理后的打印 PDF 页数校验失败"));
    }
    Ok(PrintPdfBuild {
        bytes,
        material_count,
        rendered_material_count,
        page_count,
        warnings,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_material(
    pdf: &PdfDocumentReference,
    font: &IndirectFontRef,
    expense: &ExpenseItem,
    invoice: Option<&ReportedInvoice>,
    material: &InvoiceDocument,
    expense_index: usize,
    expense_total: usize,
    global_page_start: usize,
    remaining_pages: usize,
) -> AppendResult {
    if remaining_pages == 0 {
        return AppendResult::warning(0, "打印文件已达到 600 页安全上限，未渲染该材料");
    }
    let path = Path::new(&material.file_path);
    let bytes = match read_validated_material(path, material.sha256.as_deref()) {
        Ok(bytes) => bytes,
        Err(message) => return AppendResult::warning(0, message),
    };
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let header = MaterialHeader {
        expense,
        material,
        expense_index,
        expense_total,
        global_page_start,
    };
    match extension.as_str() {
        "pdf" => append_pdf_material(pdf, font, &header, path, remaining_pages),
        "png" | "jpg" | "jpeg" | "bmp" | "webp" | "tif" | "tiff" => {
            append_image_material(pdf, font, &header, &bytes)
        }
        "ofd" => append_ofd_material(pdf, font, &header, path, &bytes, remaining_pages),
        "xml" => {
            append_structured_invoice_page(pdf, font, &header, invoice, material);
            AppendResult::success(1)
        }
        _ => AppendResult::warning(
            0,
            format!(
                "暂不支持将 .{} 转换为打印页面",
                extension.if_empty("未知格式")
            ),
        ),
    }
}

struct MaterialHeader<'a> {
    expense: &'a ExpenseItem,
    material: &'a InvoiceDocument,
    expense_index: usize,
    expense_total: usize,
    global_page_start: usize,
}

fn append_pdf_material(
    pdf: &PdfDocumentReference,
    font: &IndirectFontRef,
    header: &MaterialHeader<'_>,
    path: &Path,
    remaining_pages: usize,
) -> AppendResult {
    let page_total = match invoice_parse::pdf_ocr::pdf_page_count(path) {
        Ok(count) if count > 0 => count as usize,
        Ok(_) => return AppendResult::warning(0, "PDF 没有可打印页面"),
        Err(error) => return AppendResult::warning(0, format!("PDF 无法打开：{error}")),
    };
    if page_total > remaining_pages {
        return AppendResult::warning(
            0,
            format!("PDF 共 {page_total} 页，超出打印文件剩余页数安全上限"),
        );
    }
    let mut pages = 0;
    for page_index in 0..page_total {
        let png = match invoice_parse::pdf_ocr::render_pdf_preview_page(
            path,
            page_index as u32,
            PDF_RENDER_SIDE,
        ) {
            Ok(value) => value,
            Err(error) => {
                return AppendResult::warning(
                    pages,
                    format!("PDF 第 {} 页渲染失败：{error}", page_index + 1),
                )
            }
        };
        let image = match decode_image(&png) {
            Ok(value) => value,
            Err(message) => {
                return AppendResult::warning(
                    pages,
                    format!("PDF 第 {} 页图像无效：{message}", page_index + 1),
                )
            }
        };
        if let Err(message) = add_image_page(pdf, font, header, &image, page_index + 1, page_total)
        {
            return AppendResult::warning(
                pages,
                format!("PDF 第 {} 页写入失败：{message}", page_index + 1),
            );
        }
        pages += 1;
    }
    AppendResult::success(pages)
}

fn append_image_material(
    pdf: &PdfDocumentReference,
    font: &IndirectFontRef,
    header: &MaterialHeader<'_>,
    bytes: &[u8],
) -> AppendResult {
    let image = match decode_image(bytes) {
        Ok(value) => value,
        Err(message) => return AppendResult::warning(0, message),
    };
    match add_image_page(pdf, font, header, &image, 1, 1) {
        Ok(()) => AppendResult::success(1),
        Err(message) => AppendResult::warning(0, message),
    }
}

fn append_ofd_material(
    pdf: &PdfDocumentReference,
    font: &IndirectFontRef,
    header: &MaterialHeader<'_>,
    path: &Path,
    bytes: &[u8],
    remaining_pages: usize,
) -> AppendResult {
    let page_total = match invoice_parse::ofd_preview::preview_page_count(bytes, path) {
        Ok(count) if count > 0 => count as usize,
        Ok(_) => return AppendResult::warning(0, "OFD 没有可打印页面"),
        Err(error) => return AppendResult::warning(0, format!("OFD 无法打开：{error}")),
    };
    if page_total > remaining_pages {
        return AppendResult::warning(
            0,
            format!("OFD 共 {page_total} 页，超出打印文件剩余页数安全上限"),
        );
    }
    let mut pages = 0;
    for page_number in 1..=page_total {
        let page = match invoice_parse::ofd_preview::render_preview_page(
            bytes,
            path,
            page_number as u32,
        ) {
            Ok(value) => value,
            Err(error) => {
                return AppendResult::warning(
                    pages,
                    format!("OFD 第 {page_number} 页渲染失败：{error}"),
                )
            }
        };
        add_ofd_page(pdf, font, header, &page, page_number, page_total);
        pages += 1;
    }
    AppendResult::success(pages)
}

fn add_image_page(
    pdf: &PdfDocumentReference,
    font: &IndirectFontRef,
    header: &MaterialHeader<'_>,
    image: &DynamicImage,
    material_page: usize,
    material_pages: usize,
) -> Result<(), String> {
    let max_side = image.width().max(image.height());
    let printable = if max_side > PDF_RENDER_SIDE {
        image.thumbnail(PDF_RENDER_SIDE, PDF_RENDER_SIDE)
    } else {
        image.clone()
    };
    let printable = DynamicImage::ImageRgb8(printable.to_rgb8());
    let (width, height) = printable.dimensions();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 92)
        .encode_image(&printable)
        .map_err(|_| "图像无法转换为 PDF 页面".to_string())?;
    let (page, layer) = pdf.add_page(Mm(A4_WIDTH_MM), Mm(A4_HEIGHT_MM), "材料");
    let layer = pdf.get_page(page).get_layer(layer);
    draw_material_header(&layer, font, header, material_page, material_pages);
    let natural_width_mm = width as f32 * 25.4 / 300.0;
    let natural_height_mm = height as f32 * 25.4 / 300.0;
    let scale = (CONTENT_WIDTH_MM / natural_width_mm).min(CONTENT_HEIGHT_MM / natural_height_mm);
    let rendered_width = natural_width_mm * scale;
    let rendered_height = natural_height_mm * scale;
    let x = CONTENT_LEFT_MM + (CONTENT_WIDTH_MM - rendered_width) / 2.0;
    let y = CONTENT_BOTTOM_MM + (CONTENT_HEIGHT_MM - rendered_height) / 2.0;
    let image_object = ImageXObject {
        width: Px(width as usize),
        height: Px(height as usize),
        color_space: ColorSpace::Rgb,
        bits_per_component: ColorBits::Bit8,
        interpolate: true,
        image_data: jpeg,
        image_filter: Some(ImageFilter::DCT),
        smask: None,
        clipping_bbox: None,
    };
    Image::from(image_object).add_to_layer(
        layer,
        ImageTransform {
            translate_x: Some(Mm(x)),
            translate_y: Some(Mm(y)),
            scale_x: Some(scale),
            scale_y: Some(scale),
            dpi: Some(300.0),
            ..Default::default()
        },
    );
    Ok(())
}

fn add_ofd_page(
    pdf: &PdfDocumentReference,
    font: &IndirectFontRef,
    header: &MaterialHeader<'_>,
    source: &invoice_parse::ofd_preview::OfdPreviewPage,
    material_page: usize,
    material_pages: usize,
) {
    let (page, layer) = pdf.add_page(Mm(A4_WIDTH_MM), Mm(A4_HEIGHT_MM), "OFD 材料");
    let layer = pdf.get_page(page).get_layer(layer);
    draw_material_header(&layer, font, header, material_page, material_pages);
    let scale = (CONTENT_WIDTH_MM / source.width_mm).min(CONTENT_HEIGHT_MM / source.height_mm);
    let offset_x = CONTENT_LEFT_MM + (CONTENT_WIDTH_MM - source.width_mm * scale) / 2.0;
    let offset_y = CONTENT_BOTTOM_MM + (CONTENT_HEIGHT_MM - source.height_mm * scale) / 2.0;
    for text in &source.texts {
        let x = (offset_x + text.x_mm * scale).clamp(CONTENT_LEFT_MM, 198.0);
        let y = (offset_y + (source.height_mm - text.y_mm - text.height_mm) * scale)
            .clamp(CONTENT_BOTTOM_MM, CONTENT_BOTTOM_MM + CONTENT_HEIGHT_MM);
        let font_size = (text.font_size_mm * scale * 2.834_646).clamp(5.5, 18.0);
        layer.use_text(truncate(&text.text, 120), font_size, Mm(x), Mm(y), font);
    }
}

fn append_structured_invoice_page(
    pdf: &PdfDocumentReference,
    font: &IndirectFontRef,
    header: &MaterialHeader<'_>,
    invoice: Option<&ReportedInvoice>,
    material: &InvoiceDocument,
) {
    let (page, layer) = pdf.add_page(Mm(A4_WIDTH_MM), Mm(A4_HEIGHT_MM), "XML 发票");
    let layer = pdf.get_page(page).get_layer(layer);
    draw_material_header(&layer, font, header, 1, 1);
    layer.use_text("结构化电子发票打印页", 17.0, Mm(18.0), Mm(235.0), font);
    let expense = header.expense;
    let mut lines = vec![
        format!(
            "文件：{}",
            truncate(&safe_name(&material.original_name), 80)
        ),
        format!("费用类型：{}", category_label(&expense.category_code)),
        format!("发生日期：{}", expense.transaction_date),
        format!(
            "实际金额：{} {}",
            expense.currency_code, expense.gross_amount
        ),
        format!("交易方：{}", truncate(&expense.counterparty_name, 80)),
        format!(
            "地点：{}",
            expense.location.city_name.as_deref().unwrap_or("未提供")
        ),
    ];
    if let Some(invoice) = invoice {
        lines.push(format!("发票号码：{}", invoice.invoice_number));
        lines.push(format!("开票日期：{}", invoice.issue_date));
        lines.push(format!("票面金额：CNY {}", invoice.amount));
        lines.push(format!(
            "销方名称：{}",
            truncate(invoice.seller_name.as_deref().unwrap_or("未提供"), 80)
        ));
        lines.push(format!(
            "购方名称：{}",
            truncate(invoice.buyer_name.as_deref().unwrap_or("未提供"), 80)
        ));
    }
    if let Some(hash) = material.sha256.as_deref() {
        lines.push(format!("原件校验：SHA-256 {}", truncate(hash, 24)));
    }
    let mut y = 210.0;
    for line in lines {
        layer.use_text(truncate(&line, 96), 10.0, Mm(18.0), Mm(y), font);
        y -= 14.0;
    }
    layer.use_text(
        "本页由审核快照中的稳定费用字段和结构化发票原件生成。",
        8.5,
        Mm(18.0),
        Mm(28.0),
        font,
    );
}

fn draw_material_header(
    layer: &PdfLayerReference,
    font: &IndirectFontRef,
    header: &MaterialHeader<'_>,
    material_page: usize,
    material_pages: usize,
) {
    let expense = header.expense;
    layer.use_text(
        truncate(
            &format!(
                "费用 {}/{}｜{}｜{}｜{} {}",
                header.expense_index,
                header.expense_total,
                category_label(&expense.category_code),
                expense.transaction_date,
                expense.currency_code,
                expense.gross_amount
            ),
            90,
        ),
        10.0,
        Mm(10.0),
        Mm(282.0),
        font,
    );
    layer.use_text(
        truncate(
            &format!(
                "{}｜{}｜材料页 {}/{}",
                role_label(&header.material.role),
                safe_name(&header.material.original_name),
                material_page,
                material_pages
            ),
            100,
        ),
        8.5,
        Mm(10.0),
        Mm(268.0),
        font,
    );
    layer.use_text(
        format!("第 {} 页", header.global_page_start + material_page - 1),
        8.0,
        Mm(178.0),
        Mm(5.5),
        font,
    );
}

fn verify_saved_pdf(path: &Path, expected_pages: usize) -> AppResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| AppError::io(format!("读取导出 PDF 失败（{}）", error.kind())))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() == 0 {
        return Err(AppError::io("导出 PDF 不是可读取的普通文件"));
    }
    let parsed = printpdf::lopdf::Document::load(path)
        .map_err(|_| AppError::io("导出 PDF 写入后无法重新打开"))?;
    let actual_pages = parsed.get_pages().len();
    if actual_pages != expected_pages {
        return Err(AppError::io(format!(
            "导出 PDF 写入后的页数不一致（预计 {expected_pages}，实际 {actual_pages}）"
        )));
    }
    Ok(())
}

/// 只允许打开由当前批次交付任务记录的成功 PDF，前端不能传入任意文件路径。
#[tauri::command]
pub fn open_delivery_pdf(
    batch_id: i64,
    task_id: i64,
    reveal: bool,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    if batch_id <= 0 || task_id <= 0 {
        return Err(AppError::validation("PDF 交付任务无效"));
    }
    let app_state = state
        .lock()
        .map_err(|_| AppError::internal("应用状态锁不可用"))?;
    let task = app_state
        .ledger_db()?
        .list_delivery_tasks(batch_id)
        .map_err(|error| AppError::database(format!("读取 PDF 交付任务失败: {error}")))?
        .into_iter()
        .find(|task| task.id == task_id)
        .ok_or_else(|| AppError::validation("PDF 交付任务不存在"))?;
    if task.kind != "pdf" || task.status != "succeeded" {
        return Err(AppError::validation("该任务还没有可打开的 PDF"));
    }
    let path = PathBuf::from(
        task.output_path
            .ok_or_else(|| AppError::validation("PDF 交付任务没有输出文件"))?,
    );
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| AppError::io("导出的 PDF 已被移动或删除"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("pdf"))
    {
        return Err(AppError::validation("交付任务记录的输出不是有效 PDF 文件"));
    }
    let target = if reveal {
        path.parent()
            .ok_or_else(|| AppError::validation("无法定位 PDF 所在文件夹"))?
            .to_path_buf()
    } else {
        path
    };
    super::review::open_with_windows_default(&target)
}

fn read_validated_material(path: &Path, expected_sha256: Option<&str>) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "原件文件缺失".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("原件路径不是可读取的普通文件".to_string());
    }
    if metadata.len() == 0 {
        return Err("原件为空文件".to_string());
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err("原件超过 50 MB 安全输出上限".to_string());
    }
    let bytes = fs::read(path).map_err(|error| format!("读取原件失败（{}）", error.kind()))?;
    if let Some(expected) = expected_sha256.filter(|value| !value.trim().is_empty()) {
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            return Err("原件内容与审核快照校验值不一致".to_string());
        }
    }
    Ok(bytes)
}

fn decode_image(bytes: &[u8]) -> Result<DynamicImage, String> {
    let image = image::load_from_memory(bytes).map_err(|_| "图像无法打开".to_string())?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err("图像尺寸为空或超过安全上限".to_string());
    }
    Ok(image)
}

fn category_rank(value: &str) -> u8 {
    match value {
        "rail" => 0,
        "flight" => 1,
        "hotel" => 2,
        "meal" => 3,
        "city_transport" => 4,
        "courier_logistics" => 5,
        _ => 9,
    }
}

fn category_label(value: &str) -> &'static str {
    match value {
        "rail" => "火车",
        "flight" => "机票",
        "hotel" => "住宿",
        "city_transport" => "市内交通",
        "meal" => "餐饮",
        "courier_logistics" => "快递/物流",
        _ => "其他",
    }
}

fn role_rank(value: &str) -> u8 {
    match value {
        "main_invoice" => 0,
        "itinerary" => 1,
        "detail" => 2,
        "supporting" => 3,
        _ => 9,
    }
}

fn role_label(value: &str) -> &'static str {
    match value {
        "main_invoice" => "主发票",
        "itinerary" => "行程单",
        "detail" => "消费明细/水单",
        "supporting" => "其他材料",
        "duplicate_copy" => "重复副本",
        _ => "材料",
    }
}

fn safe_name(value: &str) -> String {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("未命名材料")
        .to_string()
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut characters = value.chars();
    let mut result = characters.by_ref().take(max_chars).collect::<String>();
    if characters.next().is_some() {
        result.push('…');
    }
    result
}

trait IfEmpty {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str;
}

impl IfEmpty for String {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.is_empty() {
            fallback
        } else {
            self.as_str()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, Utc};
    use invoice_store::models::{BatchStatus, ExpenseLocation};
    use printpdf::BuiltinFont;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn batch() -> Batch {
        let now = Utc::now().naive_utc();
        Batch {
            id: 8,
            name: "打印合订测试".to_string(),
            month: "2026-06".to_string(),
            status: BatchStatus::Submitted,
            total_amount: Decimal::from_str("126.00").unwrap(),
            invoice_count: 1,
            created_at: now,
            updated_at: now,
            submitted_at: Some(now),
            approved_at: None,
            completed_at: None,
            rejected_at: None,
        }
    }

    fn snapshot() -> BatchReviewSnapshot {
        BatchReviewSnapshot {
            id: 3,
            batch_id: 8,
            version: 2,
            content_sha256: "a".repeat(64),
            invoice_count: 1,
            total_amount: Decimal::from_str("126.00").unwrap(),
            created_at: "2026-07-01T10:00:00Z".to_string(),
            invalidated_at: None,
        }
    }

    fn expense(path: &Path, name: &str) -> ExpenseItem {
        ExpenseItem {
            id: 11,
            batch_id: 8,
            primary_invoice_id: 21,
            model_version: 1,
            category_code: "meal".to_string(),
            category_source: "manual_review".to_string(),
            category_confirmed: true,
            transaction_date: NaiveDate::from_ymd_opt(2026, 6, 25).unwrap(),
            transaction_date_source: "manual".to_string(),
            transaction_date_confirmed: true,
            description: "餐费".to_string(),
            counterparty_name: "测试餐厅".to_string(),
            location: ExpenseLocation {
                city_name: Some("北京".to_string()),
                ..Default::default()
            },
            payment_method: "credit_card".to_string(),
            gross_amount: Decimal::from_str("126.00").unwrap(),
            currency_code: "CNY".to_string(),
            tax_details: Vec::new(),
            trip_group_id: Some(1),
            inclusion_status: "included".to_string(),
            provenance_json: "{}".to_string(),
            documents: vec![InvoiceDocument {
                id: 31,
                batch_id: 8,
                expense_item_id: 11,
                source_invoice_id: Some(21),
                source_pending_document_id: None,
                role: "main_invoice".to_string(),
                file_path: path.to_string_lossy().into_owned(),
                original_name: name.to_string(),
                mime_type: None,
                sha256: None,
                created_at: "2026-07-01T10:00:00Z".to_string(),
            }],
            created_at: "2026-07-01T10:00:00Z".to_string(),
            updated_at: "2026-07-01T10:00:00Z".to_string(),
        }
    }

    #[test]
    fn print_pdf_embeds_image_material_and_reopens() {
        let directory = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("发票.png");
        DynamicImage::new_rgb8(120, 80).save(&image_path).unwrap();
        let output = build_print_pdf_bytes(
            &batch(),
            &snapshot(),
            &[expense(&image_path, "发票.png")],
            &[],
        )
        .unwrap();
        assert!(output.bytes.starts_with(b"%PDF-"));
        assert_eq!(output.material_count, 1);
        assert_eq!(output.rendered_material_count, 1);
        assert_eq!(output.page_count, 1);
        assert!(output.warnings.is_empty());
    }

    #[test]
    fn unsupported_material_is_reported_without_adding_print_pages() {
        let directory = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("发票.png");
        DynamicImage::new_rgb8(120, 80).save(&image_path).unwrap();
        let source = directory.path().join("水单.docx");
        fs::write(&source, b"not a printable document").unwrap();
        let mut item = expense(&image_path, "发票.png");
        item.documents.push(InvoiceDocument {
            id: 32,
            batch_id: 8,
            expense_item_id: 11,
            source_invoice_id: None,
            source_pending_document_id: None,
            role: "supporting".to_string(),
            file_path: source.to_string_lossy().into_owned(),
            original_name: "水单.docx".to_string(),
            mime_type: None,
            sha256: None,
            created_at: "2026-07-01T10:00:00Z".to_string(),
        });
        let output = build_print_pdf_bytes(&batch(), &snapshot(), &[item], &[]).unwrap();
        assert_eq!(output.material_count, 2);
        assert_eq!(output.rendered_material_count, 1);
        assert_eq!(output.warnings.len(), 1);
        assert_eq!(output.page_count, 1);
    }

    #[test]
    fn atomic_pdf_write_replaces_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("合订本.pdf");
        fs::write(&destination, b"old").unwrap();
        write_pdf_atomically(&destination, b"%PDF-new").unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"%PDF-new");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn print_pdf_renders_every_page_from_pdf_material() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("两页发票.pdf");
        let (source, first_page, first_layer) =
            PdfDocument::new("source", Mm(210.0), Mm(297.0), "page-1");
        let font = source.add_builtin_font(BuiltinFont::Helvetica).unwrap();
        source.get_page(first_page).get_layer(first_layer).use_text(
            "Invoice page 1",
            12.0,
            Mm(20.0),
            Mm(260.0),
            &font,
        );
        let (second_page, second_layer) = source.add_page(Mm(210.0), Mm(297.0), "page-2");
        source
            .get_page(second_page)
            .get_layer(second_layer)
            .use_text("Invoice page 2", 12.0, Mm(20.0), Mm(260.0), &font);
        fs::write(&source_path, source.save_to_bytes().unwrap()).unwrap();

        let output = build_print_pdf_bytes(
            &batch(),
            &snapshot(),
            &[expense(&source_path, "两页发票.pdf")],
            &[],
        )
        .unwrap();
        if let Some(path) = std::env::var_os("INVOICE_ASSISTANT_PRINT_PDF_EVIDENCE_PATH") {
            fs::write(path, &output.bytes).unwrap();
        }
        assert_eq!(output.page_count, 2);
        assert_eq!(output.rendered_material_count, 1);
        assert!(output.warnings.is_empty());
    }

    #[test]
    fn print_pdf_renders_every_text_page_from_ofd_material() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("行程单.ofd");
        let mut bytes = Vec::new();
        {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
            archive.start_file("Doc_0/Document.xml", options).unwrap();
            archive.write_all(br#"<ofd:Document xmlns:ofd="http://www.ofdspec.org/2016"><ofd:CommonData><ofd:PageArea><ofd:PhysicalBox>0 0 210 297</ofd:PhysicalBox></ofd:PageArea></ofd:CommonData></ofd:Document>"#).unwrap();
            for page in 0..2 {
                archive
                    .start_file(format!("Doc_0/Pages/Page_{page}/Content.xml"), options)
                    .unwrap();
                archive.write_all(format!(r#"<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016"><ofd:Content><ofd:Layer><ofd:TextObject Boundary="10 20 80 6" Size="4"><ofd:TextCode>行程单第{}页</ofd:TextCode></ofd:TextObject></ofd:Layer></ofd:Content></ofd:Page>"#, page + 1).as_bytes()).unwrap();
            }
            archive.finish().unwrap();
        }
        fs::write(&source_path, bytes).unwrap();

        let mut expense = expense(&source_path, "行程单.ofd");
        expense.documents[0].role = "itinerary".to_string();
        let output = build_print_pdf_bytes(&batch(), &snapshot(), &[expense], &[]).unwrap();
        assert_eq!(output.page_count, 2);
        assert_eq!(output.rendered_material_count, 1);
        assert!(output.warnings.is_empty());
    }
}
