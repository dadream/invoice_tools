//! 幂等输出包：把同一审核快照的 Excel、CSV 和 PDF 台账写入稳定目录。
//!
//! 完成目录永不覆盖。生成过程使用同父目录的 `.partial` 目录，每个成功文件
//! 都先写入带内容哈希的临时文件，再写 SHA-256 边车，最后原子改名。进程中断后
//! 可以复用已经完成的文件；只有全部文件与 manifest 校验通过后才发布完成目录。

use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use invoice_store::models::{Batch, BatchGrouping, ReportedInvoice};
use invoice_store::LedgerDb;
use printpdf::{IndirectFontRef, Mm, PdfDocument, PdfDocumentReference};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::export::{build_csv_bytes, build_excel_bytes, build_pdf_bytes};
use crate::error::{AppError, AppResult};
use crate::AppState;

const FORMAT_VERSION: u32 = 2;
const JOB_FILE: &str = "job.json";
const MANIFEST_FILE: &str = "manifest.json";
const MANIFEST_TEMP_FILE: &str = ".manifest.json.tmp";
const SOURCE_HAN_SANS_CN_VARIABLE: &[u8] =
    include_bytes!("../../assets/fonts/SourceHanSansCN-VF.ttf");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPackageFile {
    pub kind: String,
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportJob {
    format_version: u32,
    task_key: String,
    batch_id: i64,
    batch_name: String,
    month: String,
    invoice_count: usize,
    total_amount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportManifest {
    format_version: u32,
    generator_version: String,
    task_key: String,
    batch_id: i64,
    batch_name: String,
    month: String,
    invoice_count: usize,
    total_amount: String,
    completed_at_utc: String,
    files: Vec<ExportPackageFile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPackageResult {
    pub task_key: String,
    pub output_directory: String,
    pub manifest_path: String,
    pub reused: bool,
    pub files: Vec<ExportPackageFile>,
}

struct OutputSpec {
    kind: &'static str,
    name: &'static str,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OriginalFingerprint {
    #[serde(skip)]
    source_path: PathBuf,
    archive_path: String,
    bytes: u64,
    sha256: String,
}

const OUTPUT_SPECS: [OutputSpec; 5] = [
    OutputSpec {
        kind: "excel_ledger",
        name: "invoice-details.xlsx",
    },
    OutputSpec {
        kind: "csv_ledger",
        name: "invoice-details.csv",
    },
    OutputSpec {
        kind: "pdf_ledger",
        name: "invoice-ledger.pdf",
    },
    OutputSpec {
        kind: "print_booklet",
        name: "invoice-print-booklet-a4.pdf",
    },
    OutputSpec {
        kind: "originals_archive",
        name: "invoice-originals.zip",
    },
];

#[tauri::command]
pub fn export_batch_package(
    state: State<Mutex<AppState>>,
    batch_id: i64,
    output_root: String,
) -> AppResult<ExportPackageResult> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;
    export_batch_package_from_db(db, batch_id, Path::new(&output_root))
}

fn export_batch_package_from_db(
    db: &LedgerDb,
    batch_id: i64,
    output_root: &Path,
) -> AppResult<ExportPackageResult> {
    let batch = db
        .get_batch(batch_id)
        .map_err(|error| AppError::database(format!("获取批次失败: {error}")))?;
    super::export::ensure_batch_exportable(&batch.status)?;
    let invoices = db
        .list_reimbursable_invoices_by_batch(batch_id)
        .map_err(|error| AppError::database(format!("获取发票列表失败: {error}")))?;
    let grouping = db
        .get_batch_grouping(batch_id)
        .map_err(|error| AppError::database(format!("获取归组快照失败: {error}")))?;

    export_package_to_directory(&batch, &invoices, grouping.as_ref(), output_root)
}

fn export_package_to_directory(
    batch: &Batch,
    invoices: &[ReportedInvoice],
    grouping: Option<&BatchGrouping>,
    output_root: &Path,
) -> AppResult<ExportPackageResult> {
    let output_root = validate_output_root(output_root)?;
    let originals = original_fingerprints(invoices)?;
    let task_key = snapshot_task_key(batch, invoices, grouping, &originals)?;
    let suffix = &task_key[..16];
    let final_directory = output_root.join(format!("InvoiceAssistant-batch-{}-{suffix}", batch.id));
    let staging_directory = output_root.join(format!(
        ".InvoiceAssistant-batch-{}-{suffix}.partial",
        batch.id
    ));

    if final_directory.exists() {
        return validate_completed_directory(&final_directory, &task_key, true);
    }

    let job = ExportJob {
        format_version: FORMAT_VERSION,
        task_key: task_key.clone(),
        batch_id: batch.id,
        batch_name: batch.name.clone(),
        month: batch.month.clone(),
        invoice_count: invoices.len(),
        total_amount: batch.total_amount.to_string(),
    };

    if staging_directory.exists() {
        ensure_directory(&staging_directory, "未完成输出路径不是目录")?;
        if staging_directory.join(MANIFEST_FILE).exists() {
            finalize_staging(&staging_directory, &final_directory, &task_key)?;
            return validate_completed_directory(&final_directory, &task_key, false);
        }
        validate_job(&staging_directory.join(JOB_FILE), &job)?;
    } else {
        fs::create_dir(&staging_directory)
            .map_err(|error| AppError::io(format!("创建未完成输出目录失败: {error}")))?;
        write_json_new(&staging_directory.join(JOB_FILE), &job)?;
    }
    validate_staging_names(&staging_directory)?;

    let mut files = Vec::with_capacity(OUTPUT_SPECS.len());
    for spec in &OUTPUT_SPECS {
        let entry = recover_or_write_output(&staging_directory, spec, || match spec.kind {
            "excel_ledger" => build_excel_bytes(batch, invoices),
            "csv_ledger" => Ok(build_csv_bytes(batch, invoices)),
            "pdf_ledger" => build_pdf_bytes(batch, invoices),
            "print_booklet" => build_print_booklet_bytes(batch, invoices, &originals),
            "originals_archive" => build_originals_zip_bytes(invoices, &originals),
            _ => Err(AppError::internal("未知输出类型")),
        })?;
        files.push(entry);
    }

    let manifest = ExportManifest {
        format_version: FORMAT_VERSION,
        generator_version: env!("CARGO_PKG_VERSION").to_string(),
        task_key: task_key.clone(),
        batch_id: batch.id,
        batch_name: batch.name.clone(),
        month: batch.month.clone(),
        invoice_count: invoices.len(),
        total_amount: batch.total_amount.to_string(),
        completed_at_utc: chrono::Utc::now().to_rfc3339(),
        files,
    };
    write_manifest_for_finalization(&staging_directory, &manifest)?;
    finalize_staging(&staging_directory, &final_directory, &task_key)?;
    validate_completed_directory(&final_directory, &task_key, false)
}

fn validate_output_root(path: &Path) -> AppResult<PathBuf> {
    if !path.is_absolute() {
        return Err(AppError::validation("输出位置必须是绝对路径"));
    }
    ensure_directory(path, "所选输出位置不是文件夹")?;
    path.canonicalize()
        .map_err(|error| AppError::io(format!("无法读取输出位置: {error}")))
}

fn ensure_directory(path: &Path, message: &str) -> AppResult<()> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| AppError::io(format!("{message}: {error}")))?;
    if !metadata.is_dir() {
        return Err(AppError::validation(message));
    }
    Ok(())
}

fn snapshot_task_key(
    batch: &Batch,
    invoices: &[ReportedInvoice],
    grouping: Option<&BatchGrouping>,
    originals: &[OriginalFingerprint],
) -> AppResult<String> {
    let canonical = serde_json::to_vec(&(FORMAT_VERSION, batch, invoices, grouping, originals))
        .map_err(|error| AppError::internal(format!("构建输出任务键失败: {error}")))?;
    Ok(sha256_hex(&canonical))
}

fn original_fingerprints(invoices: &[ReportedInvoice]) -> AppResult<Vec<OriginalFingerprint>> {
    invoices
        .iter()
        .enumerate()
        .map(|(index, invoice)| {
            let source_path = PathBuf::from(&invoice.file_path);
            let metadata = fs::symlink_metadata(&source_path)
                .map_err(|_| AppError::validation("原件文件缺失；请恢复原件后再生成最终输出"))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(AppError::validation("原件路径不是可导出的普通文件"));
            }
            let extension = source_path
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase)
                .filter(|value| {
                    matches!(
                        value.as_str(),
                        "xml" | "ofd" | "pdf" | "png" | "jpg" | "jpeg" | "webp" | "bmp"
                    )
                })
                .ok_or_else(|| AppError::validation("原件文件类型不受支持"))?;
            let sha256 = hash_file(&source_path)?;
            Ok(OriginalFingerprint {
                source_path,
                archive_path: format!("originals/{:03}-{}.{}", index + 1, &sha256[..12], extension),
                bytes: metadata.len(),
                sha256,
            })
        })
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OriginalIndexEntry<'a> {
    invoice_id: i64,
    archive_path: &'a str,
    bytes: u64,
    sha256: &'a str,
}

fn build_originals_zip_bytes(
    invoices: &[ReportedInvoice],
    originals: &[OriginalFingerprint],
) -> AppResult<Vec<u8>> {
    if invoices.len() != originals.len() {
        return Err(AppError::internal("原件清单与发票数量不一致"));
    }
    let cursor = Cursor::new(Vec::new());
    let mut archive = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut index = Vec::with_capacity(invoices.len());
    for (invoice, original) in invoices.iter().zip(originals) {
        let mut bytes = Vec::new();
        File::open(&original.source_path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|_| AppError::io("读取原件失败"))?;
        let bytes_len =
            u64::try_from(bytes.len()).map_err(|_| AppError::internal("原件大小超出支持范围"))?;
        if bytes_len != original.bytes || sha256_hex(&bytes) != original.sha256 {
            return Err(AppError::validation(
                "原件在输出过程中发生变化；请重新开始输出",
            ));
        }
        archive
            .start_file(&original.archive_path, options)
            .map_err(|error| AppError::io(format!("创建原件归档失败: {error}")))?;
        archive.write_all(&bytes)?;
        index.push(OriginalIndexEntry {
            invoice_id: invoice.id,
            archive_path: &original.archive_path,
            bytes: original.bytes,
            sha256: &original.sha256,
        });
    }
    let mut index_bytes = serde_json::to_vec_pretty(&index)
        .map_err(|error| AppError::internal(format!("生成原件索引失败: {error}")))?;
    index_bytes.push(b'\n');
    archive
        .start_file("originals/index.json", options)
        .map_err(|error| AppError::io(format!("创建原件索引失败: {error}")))?;
    archive.write_all(&index_bytes)?;
    archive
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| AppError::io(format!("完成原件归档失败: {error}")))
}

struct BookletFont {
    font: IndirectFontRef,
    supports_cjk: bool,
}

fn load_booklet_font(document: &PdfDocumentReference) -> AppResult<BookletFont> {
    let font = document
        .add_external_font(Cursor::new(SOURCE_HAN_SANS_CN_VARIABLE))
        .map_err(|error| AppError::internal(format!("加载内置 PDF 中文字体失败: {error:?}")))?;
    Ok(BookletFont {
        font,
        supports_cjk: true,
    })
}

fn printable_text(value: &str, supports_cjk: bool, max_chars: usize) -> String {
    let normalized = if supports_cjk {
        value.to_string()
    } else {
        value
            .chars()
            .filter(|character| character.is_ascii_graphic() || character.is_ascii_whitespace())
            .collect()
    };
    let mut characters = normalized.chars();
    let mut result: String = characters.by_ref().take(max_chars).collect();
    if characters.next().is_some() {
        result.push_str("...");
    }
    result
}

fn build_print_booklet_bytes(
    batch: &Batch,
    invoices: &[ReportedInvoice],
    originals: &[OriginalFingerprint],
) -> AppResult<Vec<u8>> {
    if invoices.len() != originals.len() {
        return Err(AppError::internal("合订本原件清单数量不一致"));
    }
    let (document, cover_page, cover_layer) =
        PdfDocument::new("Invoice Print Booklet", Mm(210.0), Mm(297.0), "Cover");
    let booklet_font = load_booklet_font(&document)?;
    let cover = document.get_page(cover_page).get_layer(cover_layer);
    cover.use_text(
        "Invoice Print Booklet (A4)",
        20.0,
        Mm(18.0),
        Mm(272.0),
        &booklet_font.font,
    );
    cover.use_text(
        format!("Batch ID: {}", batch.id),
        11.0,
        Mm(18.0),
        Mm(258.0),
        &booklet_font.font,
    );
    cover.use_text(
        format!(
            "Batch: {}",
            printable_text(&batch.name, booklet_font.supports_cjk, 64)
        ),
        11.0,
        Mm(18.0),
        Mm(248.0),
        &booklet_font.font,
    );
    cover.use_text(
        format!(
            "Month: {}  Invoices: {}  Total CNY: {}",
            batch.month,
            invoices.len(),
            batch.total_amount
        ),
        11.0,
        Mm(18.0),
        Mm(238.0),
        &booklet_font.font,
    );
    cover.use_text(
        "Each following A4 page is a normalized printable view. Exact source files are in invoice-originals.zip.",
        9.0,
        Mm(18.0),
        Mm(222.0),
        &booklet_font.font,
    );

    for (index, (invoice, original)) in invoices.iter().zip(originals).enumerate() {
        let (page, layer) = document.add_page(Mm(210.0), Mm(297.0), "Invoice");
        let layer = document.get_page(page).get_layer(layer);
        let fields = [
            ("Invoice", format!("{} / {}", index + 1, invoices.len())),
            ("Invoice number", invoice.invoice_number.clone()),
            (
                "Issue date",
                invoice.issue_date.format("%Y-%m-%d").to_string(),
            ),
            ("Amount (CNY)", invoice.amount.to_string()),
            (
                "Tax (CNY)",
                invoice
                    .tax_amount
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            ("Ticket type", invoice.ticket_type.to_str().to_string()),
            ("Buyer", invoice.buyer_name.clone().unwrap_or_default()),
            ("Seller", invoice.seller_name.clone().unwrap_or_default()),
            ("City", invoice.city.clone().unwrap_or_default()),
            (
                "Verification",
                invoice.verification_result.clone().unwrap_or_default(),
            ),
            ("Exact original", original.archive_path.clone()),
            ("Original SHA-256", original.sha256.clone()),
        ];
        let mut y = 270.0;
        for (label, value) in fields {
            layer.use_text(label, 9.0, Mm(18.0), Mm(y), &booklet_font.font);
            layer.use_text(
                printable_text(&value, booklet_font.supports_cjk, 72),
                11.0,
                Mm(55.0),
                Mm(y),
                &booklet_font.font,
            );
            y -= 15.0;
        }
    }
    document
        .save_to_bytes()
        .map_err(|error| AppError::internal(format!("生成 A4 合订本失败: {error:?}")))
}
fn validate_job(path: &Path, expected: &ExportJob) -> AppResult<()> {
    let bytes = fs::read(path).map_err(|error| {
        AppError::validation(format!(
            "发现无法恢复的未完成输出（缺少 job.json）: {error}；请保留现场并选择其他输出位置"
        ))
    })?;
    let actual: ExportJob = serde_json::from_slice(&bytes).map_err(|error| {
        AppError::validation(format!(
            "未完成输出的 job.json 已损坏: {error}；请保留现场并选择其他输出位置"
        ))
    })?;
    if actual.format_version != expected.format_version
        || actual.task_key != expected.task_key
        || actual.batch_id != expected.batch_id
    {
        return Err(AppError::validation(
            "未完成输出不属于当前审核快照；请保留现场并选择其他输出位置",
        ));
    }
    Ok(())
}

fn recover_or_write_output<F>(
    staging: &Path,
    spec: &OutputSpec,
    generate: F,
) -> AppResult<ExportPackageFile>
where
    F: FnOnce() -> AppResult<Vec<u8>>,
{
    let target = staging.join(spec.name);
    let sidecar = staging.join(format!("{}.sha256", spec.name));
    let temporary_files = find_output_temporaries(staging, spec.name)?;

    if target.exists() {
        if !temporary_files.is_empty() || !sidecar.exists() {
            return Err(inconsistent_partial_error(spec.name));
        }
        let expected_hash = read_sha256_sidecar(&sidecar)?;
        let actual_hash = hash_file(&target)?;
        if actual_hash != expected_hash {
            return Err(inconsistent_partial_error(spec.name));
        }
        return file_entry(spec, &target, actual_hash);
    }

    if temporary_files.len() > 1 {
        return Err(inconsistent_partial_error(spec.name));
    }
    if let Some(temporary) = temporary_files.first() {
        let actual_hash = hash_file(temporary)?;
        let encoded_hash = temporary_hash(temporary, spec.name)?;
        if actual_hash != encoded_hash {
            return Err(inconsistent_partial_error(spec.name));
        }
        if sidecar.exists() {
            if read_sha256_sidecar(&sidecar)? != actual_hash {
                return Err(inconsistent_partial_error(spec.name));
            }
        } else {
            write_new(&sidecar, format!("{actual_hash}\n").as_bytes())?;
        }
        fs::rename(temporary, &target)
            .map_err(|error| AppError::io(format!("恢复 {} 失败: {error}", spec.name)))?;
        return file_entry(spec, &target, actual_hash);
    }

    if sidecar.exists() {
        return Err(inconsistent_partial_error(spec.name));
    }
    let bytes = generate()?;
    let hash = sha256_hex(&bytes);
    let temporary = staging.join(format!(".{}.{}.tmp", spec.name, hash));
    write_new(&temporary, &bytes)?;
    if let Err(error) = write_new(&sidecar, format!("{hash}\n").as_bytes()) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(&temporary, &target)
        .map_err(|error| AppError::io(format!("发布 {} 失败: {error}", spec.name)))?;
    file_entry(spec, &target, hash)
}

fn find_output_temporaries(staging: &Path, name: &str) -> AppResult<Vec<PathBuf>> {
    let prefix = format!(".{name}.");
    let mut matches = Vec::new();
    for item in fs::read_dir(staging)? {
        let item = item?;
        let item_name = item.file_name();
        let item_name = item_name.to_string_lossy();
        if item_name.starts_with(&prefix) && item_name.ends_with(".tmp") {
            matches.push(item.path());
        }
    }
    Ok(matches)
}

fn temporary_hash(path: &Path, output_name: &str) -> AppResult<String> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| inconsistent_partial_error(output_name))?;
    let prefix = format!(".{output_name}.");
    let hash = name
        .strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix(".tmp"))
        .ok_or_else(|| inconsistent_partial_error(output_name))?;
    if hash.len() != 64 || !hash.bytes().all(|value| value.is_ascii_hexdigit()) {
        return Err(inconsistent_partial_error(output_name));
    }
    Ok(hash.to_ascii_uppercase())
}

fn inconsistent_partial_error(name: &str) -> AppError {
    AppError::validation(format!(
        "未完成输出中的 {name} 状态不一致；为避免覆盖证据，请选择其他输出位置"
    ))
}

fn file_entry(spec: &OutputSpec, path: &Path, sha256: String) -> AppResult<ExportPackageFile> {
    Ok(ExportPackageFile {
        kind: spec.kind.to_string(),
        path: spec.name.to_string(),
        bytes: fs::metadata(path)?.len(),
        sha256,
    })
}

fn write_manifest_for_finalization(staging: &Path, manifest: &ExportManifest) -> AppResult<()> {
    let manifest_path = staging.join(MANIFEST_FILE);
    if manifest_path.exists() {
        let existing = read_manifest(&manifest_path)?;
        if existing.task_key != manifest.task_key {
            return Err(AppError::validation("未完成输出 manifest 的任务键不匹配"));
        }
        return Ok(());
    }
    let temporary = staging.join(MANIFEST_TEMP_FILE);
    if temporary.exists() {
        let existing: ExportManifest = serde_json::from_slice(&fs::read(&temporary)?)
            .map_err(|error| AppError::validation(format!("临时 manifest 已损坏: {error}")))?;
        if existing.task_key != manifest.task_key {
            return Err(AppError::validation("临时 manifest 的任务键不匹配"));
        }
    } else {
        write_json_new(&temporary, manifest)?;
    }
    fs::rename(&temporary, &manifest_path)
        .map_err(|error| AppError::io(format!("发布输出 manifest 失败: {error}")))
}

fn finalize_staging(staging: &Path, final_directory: &Path, task_key: &str) -> AppResult<()> {
    validate_staging_names(staging)?;
    let manifest_path = staging.join(MANIFEST_FILE);
    let manifest = read_manifest(&manifest_path)?;
    validate_manifest_files(staging, &manifest, task_key)?;

    remove_if_exists(&staging.join(JOB_FILE))?;
    remove_if_exists(&staging.join(MANIFEST_TEMP_FILE))?;
    for spec in &OUTPUT_SPECS {
        remove_if_exists(&staging.join(format!("{}.sha256", spec.name)))?;
    }
    validate_staging_names(staging)?;
    if final_directory.exists() {
        return Err(AppError::validation(
            "输出完成目录在发布时已存在；为避免覆盖，请选择其他输出位置",
        ));
    }
    fs::rename(staging, final_directory)
        .map_err(|error| AppError::io(format!("原子发布输出目录失败: {error}")))
}

fn validate_completed_directory(
    directory: &Path,
    task_key: &str,
    reused: bool,
) -> AppResult<ExportPackageResult> {
    ensure_directory(directory, "同名输出路径不是目录")?;
    let manifest = read_manifest(&directory.join(MANIFEST_FILE))?;
    validate_manifest_files(directory, &manifest, task_key)?;
    let manifest_path = directory.join(MANIFEST_FILE);
    Ok(ExportPackageResult {
        task_key: task_key.to_string(),
        output_directory: directory.to_string_lossy().into_owned(),
        manifest_path: manifest_path.to_string_lossy().into_owned(),
        reused,
        files: manifest.files,
    })
}

fn read_manifest(path: &Path) -> AppResult<ExportManifest> {
    serde_json::from_slice(&fs::read(path)?).map_err(|error| {
        AppError::validation(format!(
            "输出 manifest 无效: {error}；不会覆盖现有目录，请选择其他输出位置"
        ))
    })
}

fn validate_manifest_files(
    directory: &Path,
    manifest: &ExportManifest,
    task_key: &str,
) -> AppResult<()> {
    if manifest.format_version != FORMAT_VERSION || manifest.task_key != task_key {
        return Err(AppError::validation(
            "现有输出不属于当前审核快照；不会覆盖，请选择其他输出位置",
        ));
    }
    if manifest.files.len() != OUTPUT_SPECS.len() {
        return Err(AppError::validation("输出 manifest 文件数量不匹配"));
    }
    for spec in &OUTPUT_SPECS {
        let entries: Vec<_> = manifest
            .files
            .iter()
            .filter(|entry| entry.path == spec.name && entry.kind == spec.kind)
            .collect();
        if entries.len() != 1 {
            return Err(AppError::validation("输出 manifest 文件清单不匹配"));
        }
        let entry = entries[0];
        let relative = Path::new(&entry.path);
        if relative.components().count() != 1
            || !matches!(relative.components().next(), Some(Component::Normal(_)))
        {
            return Err(AppError::validation("输出 manifest 包含不安全路径"));
        }
        let target = directory.join(relative);
        let metadata = fs::metadata(&target)?;
        if !metadata.is_file()
            || metadata.len() != entry.bytes
            || hash_file(&target)? != entry.sha256
        {
            return Err(AppError::validation(format!(
                "现有输出 {} 校验失败；不会覆盖，请选择其他输出位置",
                entry.path
            )));
        }
    }
    Ok(())
}

fn validate_staging_names(staging: &Path) -> AppResult<()> {
    for item in fs::read_dir(staging)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        let fixed = name == JOB_FILE || name == MANIFEST_FILE || name == MANIFEST_TEMP_FILE;
        let output = OUTPUT_SPECS.iter().any(|spec| name == spec.name);
        let sidecar = OUTPUT_SPECS
            .iter()
            .any(|spec| name == format!("{}.sha256", spec.name));
        let temporary = OUTPUT_SPECS
            .iter()
            .any(|spec| name.starts_with(&format!(".{}.", spec.name)) && name.ends_with(".tmp"));
        if !(fixed || output || sidecar || temporary) {
            return Err(AppError::validation(format!(
                "未完成输出目录含未知文件 {name}；不会删除或覆盖，请选择其他输出位置"
            )));
        }
    }
    Ok(())
}

fn write_json_new<T: Serialize>(path: &Path, value: &T) -> AppResult<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| AppError::internal(format!("序列化输出清单失败: {error}")))?;
    bytes.push(b'\n');
    write_new(path, &bytes)
}

fn write_new(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| AppError::io(format!("创建输出文件失败: {error}")))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn read_sha256_sidecar(path: &Path) -> AppResult<String> {
    let value = fs::read_to_string(path)?.trim().to_ascii_uppercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::validation("输出 SHA-256 边车格式无效"));
    }
    Ok(value)
}

fn hash_file(path: &Path) -> AppResult<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:X}", hasher.finalize()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:X}", Sha256::digest(bytes))
}

fn remove_if_exists(path: &Path) -> AppResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io(format!("清理输出元数据失败: {error}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use invoice_store::models::BatchStatus;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn submitted_batch() -> Batch {
        let now = Utc::now().naive_utc();
        Batch {
            id: 42,
            name: "2026 年 8 月测试".to_string(),
            month: "2026-08".to_string(),
            status: BatchStatus::Submitted,
            total_amount: Decimal::from_str("0.00").unwrap(),
            invoice_count: 0,
            created_at: now,
            updated_at: now,
            submitted_at: Some(now),
            approved_at: None,
            completed_at: None,
            rejected_at: None,
        }
    }

    #[test]
    fn bundled_pdf_font_is_present_and_cjk_text_is_preserved() {
        assert!(SOURCE_HAN_SANS_CN_VARIABLE.len() > 17_000_000);
        assert_eq!(
            printable_text("北京发票报销助手", true, 64),
            "北京发票报销助手"
        );
    }

    #[test]
    fn creates_hashed_package_and_reuses_same_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let batch = submitted_batch();
        let first = export_package_to_directory(&batch, &[], None, root.path()).unwrap();
        assert!(!first.reused);
        assert_eq!(first.files.len(), 5);
        assert!(Path::new(&first.manifest_path).is_file());
        assert!(!root
            .path()
            .join(format!(
                ".InvoiceAssistant-batch-{}-{}.partial",
                batch.id,
                &first.task_key[..16]
            ))
            .exists());
        for file in &first.files {
            assert_eq!(file.sha256.len(), 64);
            assert!(Path::new(&first.output_directory)
                .join(&file.path)
                .is_file());
        }

        let second = export_package_to_directory(&batch, &[], None, root.path()).unwrap();
        assert!(second.reused);
        assert_eq!(second.task_key, first.task_key);
        assert_eq!(second.output_directory, first.output_directory);
    }

    #[test]
    fn refuses_to_overwrite_corrupted_completed_output() {
        let root = tempfile::tempdir().unwrap();
        let batch = submitted_batch();
        let first = export_package_to_directory(&batch, &[], None, root.path()).unwrap();
        fs::write(
            Path::new(&first.output_directory).join("invoice-details.csv"),
            b"tampered",
        )
        .unwrap();
        let error = export_package_to_directory(&batch, &[], None, root.path()).unwrap_err();
        assert!(error.message().contains("校验失败"));
    }

    #[test]
    fn recovers_completed_file_without_regenerating() {
        let root = tempfile::tempdir().unwrap();
        let spec = &OUTPUT_SPECS[1];
        let first = recover_or_write_output(root.path(), spec, || Ok(b"stable".to_vec())).unwrap();
        let second = recover_or_write_output(root.path(), spec, || {
            panic!("completed output must be reused")
        })
        .unwrap();
        assert_eq!(first.sha256, second.sha256);
    }

    #[test]
    fn rejects_relative_output_root() {
        let batch = submitted_batch();
        let error =
            export_package_to_directory(&batch, &[], None, Path::new("relative")).unwrap_err();
        assert!(error.message().contains("绝对路径"));
    }

    #[test]
    fn originals_archive_preserves_bytes_and_omits_source_path() {
        use chrono::NaiveDate;
        use invoice_store::models::TicketType;
        use std::io::Read as _;

        let root = tempfile::tempdir().unwrap();
        let original_path = root.path().join("source.ofd");
        let original_bytes = b"exact-original-content";
        fs::write(&original_path, original_bytes).unwrap();
        let now = Utc::now().naive_utc();
        let invoice = ReportedInvoice {
            id: 7,
            batch_id: 42,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            amount: Decimal::from_str("88.00").unwrap(),
            tax_amount: Some(Decimal::from_str("8.00").unwrap()),
            buyer_name: Some("测试用户".to_string()),
            seller_name: Some("测试商户".to_string()),
            ticket_type: TicketType::Hotel,
            city: Some("北京".to_string()),
            departure_time: None,
            checkin_date: Some(NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()),
            file_path: original_path.to_string_lossy().into_owned(),
            created_at: now,
            updated_at: now,
            verification_result: Some("not_signed".to_string()),
            is_duplicate: false,
            duplicate_reason: None,
        };
        let originals = original_fingerprints(std::slice::from_ref(&invoice)).unwrap();
        let archive_bytes =
            build_originals_zip_bytes(std::slice::from_ref(&invoice), &originals).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes)).unwrap();
        {
            let mut original = archive.by_name(&originals[0].archive_path).unwrap();
            let mut restored = Vec::new();
            original.read_to_end(&mut restored).unwrap();
            assert_eq!(restored, original_bytes);
        }
        let mut index = String::new();
        archive
            .by_name("originals/index.json")
            .unwrap()
            .read_to_string(&mut index)
            .unwrap();
        assert!(index.contains(&originals[0].sha256));
        assert!(!index.contains(&original_path.to_string_lossy().to_string()));

        let booklet = build_print_booklet_bytes(
            &submitted_batch(),
            std::slice::from_ref(&invoice),
            &originals,
        )
        .unwrap();
        if let Some(path) = std::env::var_os("INVOICE_ASSISTANT_PDF_EVIDENCE_PATH") {
            fs::write(path, &booklet).expect("写入 PDF 视觉验证证据失败");
        }
        assert!(booklet.starts_with(b"%PDF-"));
        assert!([b"/FontFile2".as_slice(), b"/FontFile3".as_slice()]
            .iter()
            .any(|marker| booklet
                .windows(marker.len())
                .any(|window| window == *marker)));
        assert!(booklet
            .windows(b"/ToUnicode".len())
            .any(|window| window == b"/ToUnicode"));
    }

    #[test]
    fn excluded_invoice_is_absent_from_every_package_data_source() {
        use chrono::NaiveDate;
        use invoice_store::models::TicketType;
        use serde_json::Value;
        use std::io::Read as _;

        let root = tempfile::tempdir().unwrap();
        let db = LedgerDb::new(root.path().join("ledger.db")).unwrap();
        let batch_id = db.create_batch("剔除集成验证", "2026-08").unwrap();
        let kept_path = root.path().join("kept.xml");
        let excluded_path = root.path().join("excluded.xml");
        fs::write(&kept_path, b"<invoice>kept</invoice>").unwrap();
        fs::write(&excluded_path, b"<invoice>excluded</invoice>").unwrap();
        let now = Utc::now().naive_utc();
        let invoice = |number: &str, amount: &str, path: &Path| ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: number.to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            amount: Decimal::from_str(amount).unwrap(),
            tax_amount: None,
            buyer_name: Some("测试用户".to_string()),
            seller_name: Some("测试商户".to_string()),
            ticket_type: TicketType::Meal,
            city: Some("北京".to_string()),
            departure_time: None,
            checkin_date: None,
            file_path: path.to_string_lossy().into_owned(),
            created_at: now,
            updated_at: now,
            verification_result: Some("not_signed".to_string()),
            is_duplicate: false,
            duplicate_reason: None,
        };
        let kept_id = db
            .add_invoice(&invoice(
                "KEEP0000000000000001",
                "12.00",
                kept_path.as_path(),
            ))
            .unwrap();
        let excluded_id = db
            .add_invoice(&invoice(
                "DROP0000000000000001",
                "99.00",
                excluded_path.as_path(),
            ))
            .unwrap();
        db.set_invoice_excluded_with_audit(excluded_id, true)
            .unwrap();
        db.transition_batch_status(batch_id, BatchStatus::Submitted)
            .unwrap();

        let output_root = root.path().join("output");
        fs::create_dir(&output_root).unwrap();
        let result = export_batch_package_from_db(&db, batch_id, output_root.as_path()).unwrap();
        let output = Path::new(&result.output_directory);

        let csv = fs::read_to_string(output.join("invoice-details.csv")).unwrap();
        assert!(csv.contains("KEEP0000000000000001"));
        assert!(!csv.contains("DROP0000000000000001"));

        let manifest: Value =
            serde_json::from_slice(&fs::read(output.join(MANIFEST_FILE)).unwrap()).unwrap();
        assert_eq!(manifest["invoiceCount"], 1);
        assert_eq!(manifest["totalAmount"], "12.00");

        let archive_bytes = fs::read(output.join("invoice-originals.zip")).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes)).unwrap();
        let mut index = String::new();
        archive
            .by_name("originals/index.json")
            .unwrap()
            .read_to_string(&mut index)
            .unwrap();
        assert!(index.contains(&format!("\"invoiceId\": {kept_id}")));
        assert!(!index.contains(&format!("\"invoiceId\": {excluded_id}")));
        assert_eq!(archive.len(), 2);
    }
}
