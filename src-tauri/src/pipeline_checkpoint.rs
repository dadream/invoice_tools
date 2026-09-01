//! 流水线文件检查点与原件持久化。
//!
//! 检查点都在 DataRoot 内的任务目录中，使用固定文件名、大小限制、相对路径和
//! SHA-256。现有文件不被静默覆盖；不一致时保留现场并要求用户处理。

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use invoice_parse::model::ParsedInvoice;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};

const FORMAT_VERSION: u32 = 1;
pub const PARSED_RESULT_FORMAT_VERSION: u32 = 1;
const MAX_CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;
const ORIGINALS_MANIFEST: &str = "originals-manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectedCheckpoint {
    format_version: u32,
    files: Vec<CollectedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectedFile {
    relative_path: PathBuf,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParsedCheckpoint {
    format_version: u32,
    parser_version: String,
    invoices: Vec<ParsedInvoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OriginalsManifest {
    format_version: u32,
    pipeline_id: String,
    files: Vec<OriginalFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OriginalFile {
    path: String,
    bytes: u64,
    sha256: String,
}

pub fn checkpoint_exists(task_dir: &Path, name: &str) -> bool {
    checkpoint_path(task_dir, name)
        .map(|path| path.is_file())
        .unwrap_or(false)
}

pub fn write_collected(task_dir: &Path, files: &[PathBuf]) -> AppResult<()> {
    let root = canonical_task_dir(task_dir)?;
    let mut seen = HashSet::new();
    let mut records = Vec::with_capacity(files.len());
    for file in files {
        let canonical = canonical_regular_file(file)?;
        if !canonical.starts_with(&root) {
            return Err(AppError::validation(
                "采集结果不在当前任务暂存目录内，不能建立恢复检查点",
            ));
        }
        let relative = canonical
            .strip_prefix(&root)
            .map_err(|_| AppError::validation("无法生成安全的采集相对路径"))?
            .to_path_buf();
        validate_relative_path(&relative)?;
        if !seen.insert(relative.clone()) {
            return Err(AppError::validation("采集检查点含重复文件路径"));
        }
        records.push(CollectedFile {
            relative_path: relative,
            bytes: fs::metadata(&canonical)?.len(),
            sha256: hash_file(&canonical)?,
        });
    }
    let checkpoint = CollectedCheckpoint {
        format_version: FORMAT_VERSION,
        files: records,
    };
    write_json_checkpoint(task_dir, "collected", &checkpoint)
}

pub fn load_collected(task_dir: &Path) -> AppResult<Vec<PathBuf>> {
    let checkpoint: CollectedCheckpoint = read_json_checkpoint(task_dir, "collected")?;
    if checkpoint.format_version != FORMAT_VERSION {
        return Err(AppError::validation("采集检查点版本不受支持"));
    }
    let root = canonical_task_dir(task_dir)?;
    let mut seen = HashSet::new();
    let mut files = Vec::with_capacity(checkpoint.files.len());
    for record in checkpoint.files {
        validate_relative_path(&record.relative_path)?;
        if !seen.insert(record.relative_path.clone()) {
            return Err(AppError::validation("采集检查点含重复文件路径"));
        }
        let candidate = root.join(&record.relative_path);
        let canonical = canonical_regular_file(&candidate)?;
        if !canonical.starts_with(&root)
            || fs::metadata(&canonical)?.len() != record.bytes
            || hash_file(&canonical)? != record.sha256
        {
            return Err(AppError::validation(
                "采集检查点文件已变化；为避免使用错误原件，任务不会自动继续",
            ));
        }
        files.push(canonical);
    }
    Ok(files)
}

pub fn write_json_checkpoint<T: Serialize>(
    task_dir: &Path,
    name: &str,
    value: &T,
) -> AppResult<()> {
    let path = checkpoint_path(task_dir, name)?;
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| AppError::internal(format!("序列化 {name} 检查点失败: {error}")))?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_CHECKPOINT_BYTES {
        return Err(AppError::validation(format!(
            "{name} 检查点超过 64 MiB 上限"
        )));
    }
    if path.exists() {
        let existing = read_limited(&path, MAX_CHECKPOINT_BYTES)?;
        if existing == bytes {
            return Ok(());
        }
        return Err(AppError::validation(format!(
            "现有 {name} 检查点与本次结果不一致；不会覆盖，请保留现场"
        )));
    }
    let temporary = path.with_extension("json.tmp");
    if temporary.exists() {
        let existing = read_limited(&temporary, MAX_CHECKPOINT_BYTES)?;
        if existing != bytes {
            return Err(AppError::validation(format!(
                "未完成的 {name} 检查点与本次结果不一致；不会覆盖"
            )));
        }
    } else {
        write_new(&temporary, &bytes)?;
    }
    fs::rename(&temporary, &path)
        .map_err(|error| AppError::io(format!("发布 {name} 检查点失败: {error}")))
}

pub fn read_json_checkpoint<T: DeserializeOwned>(task_dir: &Path, name: &str) -> AppResult<T> {
    let path = checkpoint_path(task_dir, name)?;
    let bytes = read_limited(&path, MAX_CHECKPOINT_BYTES)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| AppError::validation(format!("{name} 检查点无效: {error}")))
}

pub fn write_parsed(task_dir: &Path, invoices: &[ParsedInvoice]) -> AppResult<()> {
    let checkpoint = ParsedCheckpoint {
        format_version: PARSED_RESULT_FORMAT_VERSION,
        parser_version: invoice_parse::PARSER_VERSION.to_string(),
        invoices: invoices.to_vec(),
    };
    write_json_checkpoint(task_dir, "parsed", &checkpoint)
}

pub fn load_parsed(task_dir: &Path) -> AppResult<Vec<ParsedInvoice>> {
    let checkpoint: ParsedCheckpoint = read_json_checkpoint(task_dir, "parsed")?;
    if checkpoint.format_version != PARSED_RESULT_FORMAT_VERSION {
        return Err(AppError::validation("解析结果检查点版本不受支持"));
    }
    if checkpoint.parser_version.trim().is_empty() {
        return Err(AppError::validation("解析结果检查点缺少解析器版本"));
    }
    Ok(checkpoint.invoices)
}

pub fn validate_parsed_sources(parsed: &[ParsedInvoice], collected: &[PathBuf]) -> AppResult<()> {
    validate_collected_source_paths(
        parsed.iter().map(|invoice| invoice.source_path.as_path()),
        collected,
        "解析检查点引用了采集清单之外的文件，任务不会继续",
    )
}

/// 比较检查点来源时统一使用规范化绝对路径，避免 Windows 大小写、`..` 或
/// 路径前缀表示不同导致恢复任务把同一个普通文件误判成外部文件。
pub(crate) fn validate_collected_source_paths<'a, I>(
    sources: I,
    collected: &[PathBuf],
    message: &str,
) -> AppResult<()>
where
    I: IntoIterator<Item = &'a Path>,
{
    let allowed = collected
        .iter()
        .map(|path| canonical_regular_file(path))
        .collect::<AppResult<HashSet<_>>>()?;
    for source in sources {
        if !allowed.contains(&canonical_regular_file(source)?) {
            return Err(AppError::validation(message));
        }
    }
    Ok(())
}

/// 把解析使用的暂存文件复制到稳定原件目录；可从逐文件完成状态恢复。
pub fn prepare_originals(
    data_root: &Path,
    pipeline_id: &str,
    invoices: &[ParsedInvoice],
) -> AppResult<Vec<PathBuf>> {
    uuid::Uuid::parse_str(pipeline_id)
        .map_err(|_| AppError::validation("流水线标识无效，不能保存原件"))?;
    let files_root = data_root.join("files");
    fs::create_dir_all(&files_root)?;
    let originals = files_root
        .join(format!("pipeline-{pipeline_id}"))
        .join("originals");
    fs::create_dir_all(&originals)?;
    ensure_plain_directory(&originals)?;

    let mut expected = Vec::with_capacity(invoices.len());
    let mut paths = Vec::with_capacity(invoices.len());
    for (index, invoice) in invoices.iter().enumerate() {
        let source = canonical_regular_file(&invoice.source_path)?;
        let source_hash = hash_file(&source)?;
        let source_bytes = fs::metadata(&source)?.len();
        let original_name = source
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("invoice");
        let name = format!("{:04}-{}", index + 1, sanitize_filename(original_name));
        let target = originals.join(&name);
        let temporary = originals.join(format!(".{name}.{source_hash}.tmp"));
        if target.exists() {
            if hash_file(&target)? != source_hash || fs::metadata(&target)?.len() != source_bytes {
                return Err(AppError::validation(format!(
                    "原件 {name} 与恢复检查点不一致；不会覆盖"
                )));
            }
        } else if temporary.exists() {
            if hash_file(&temporary)? != source_hash
                || fs::metadata(&temporary)?.len() != source_bytes
            {
                return Err(AppError::validation(format!(
                    "未完成原件 {name} 已损坏；不会覆盖"
                )));
            }
            fs::rename(&temporary, &target)?;
        } else {
            copy_new_synced(&source, &temporary)?;
            if hash_file(&temporary)? != source_hash {
                return Err(AppError::io(format!("复制原件 {name} 后哈希不一致")));
            }
            fs::rename(&temporary, &target)?;
        }
        expected.push(OriginalFile {
            path: name,
            bytes: source_bytes,
            sha256: source_hash,
        });
        paths.push(target);
    }

    validate_originals_names(&originals, &expected)?;
    let manifest = OriginalsManifest {
        format_version: FORMAT_VERSION,
        pipeline_id: pipeline_id.to_string(),
        files: expected,
    };
    let manifest_path = originals.join(ORIGINALS_MANIFEST);
    if manifest_path.exists() {
        let actual: OriginalsManifest =
            serde_json::from_slice(&read_limited(&manifest_path, MAX_CHECKPOINT_BYTES)?)
                .map_err(|error| AppError::validation(format!("原件清单无效: {error}")))?;
        if actual != manifest {
            return Err(AppError::validation("原件清单与当前检查点不一致；不会覆盖"));
        }
    } else {
        let mut bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| AppError::internal(format!("序列化原件清单失败: {error}")))?;
        bytes.push(b'\n');
        let temporary = originals.join(".originals-manifest.json.tmp");
        write_new(&temporary, &bytes)?;
        fs::rename(temporary, manifest_path)?;
    }
    Ok(paths)
}

/// 把无法自动归属的材料复制到独立稳定目录。它们不会进入费用或金额，
/// 但会随 DataRoot 备份并在批次审核中等待人工挂载/忽略。
pub fn prepare_pending_documents(
    data_root: &Path,
    pipeline_id: &str,
    sources: &[PathBuf],
) -> AppResult<Vec<PathBuf>> {
    uuid::Uuid::parse_str(pipeline_id)
        .map_err(|_| AppError::validation("流水线标识无效，不能保存待挂载材料"))?;
    let files_root = data_root.join("files");
    fs::create_dir_all(&files_root)?;
    let documents = files_root
        .join(format!("pipeline-{pipeline_id}"))
        .join("pending-documents");
    fs::create_dir_all(&documents)?;
    ensure_plain_directory(&documents)?;

    let mut expected = Vec::with_capacity(sources.len());
    let mut paths = Vec::with_capacity(sources.len());
    for (index, source_path) in sources.iter().enumerate() {
        let source = canonical_regular_file(source_path)?;
        let source_hash = hash_file(&source)?;
        let source_bytes = fs::metadata(&source)?.len();
        let original_name = source
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("document");
        let name = format!("{:04}-{}", index + 1, sanitize_filename(original_name));
        let target = documents.join(&name);
        let temporary = documents.join(format!(".{name}.{source_hash}.tmp"));
        if target.exists() {
            if hash_file(&target)? != source_hash || fs::metadata(&target)?.len() != source_bytes {
                return Err(AppError::validation(format!(
                    "待挂载材料 {name} 与恢复检查点不一致；不会覆盖"
                )));
            }
        } else if temporary.exists() {
            if hash_file(&temporary)? != source_hash
                || fs::metadata(&temporary)?.len() != source_bytes
            {
                return Err(AppError::validation(format!(
                    "未完成待挂载材料 {name} 已损坏；不会覆盖"
                )));
            }
            fs::rename(&temporary, &target)?;
        } else {
            copy_new_synced(&source, &temporary)?;
            if hash_file(&temporary)? != source_hash {
                return Err(AppError::io(format!("复制待挂载材料 {name} 后哈希不一致")));
            }
            fs::rename(&temporary, &target)?;
        }
        expected.push(OriginalFile {
            path: name,
            bytes: source_bytes,
            sha256: source_hash,
        });
        paths.push(target);
    }

    validate_originals_names(&documents, &expected)?;
    let manifest = OriginalsManifest {
        format_version: FORMAT_VERSION,
        pipeline_id: pipeline_id.to_string(),
        files: expected,
    };
    let manifest_path = documents.join(ORIGINALS_MANIFEST);
    if manifest_path.exists() {
        let actual: OriginalsManifest =
            serde_json::from_slice(&read_limited(&manifest_path, MAX_CHECKPOINT_BYTES)?)
                .map_err(|error| AppError::validation(format!("待挂载材料清单无效: {error}")))?;
        if actual != manifest {
            return Err(AppError::validation(
                "待挂载材料清单与当前检查点不一致；不会覆盖",
            ));
        }
    } else {
        let mut bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| AppError::internal(format!("序列化待挂载材料清单失败: {error}")))?;
        bytes.push(b'\n');
        let temporary = documents.join(".originals-manifest.json.tmp");
        write_new(&temporary, &bytes)?;
        fs::rename(temporary, manifest_path)?;
    }
    Ok(paths)
}

fn checkpoint_path(task_dir: &Path, name: &str) -> AppResult<PathBuf> {
    if !matches!(
        name,
        "collected"
            | "source-notices"
            | "email-ledger"
            | "parsed"
            | "materials"
            | "deduped"
            | "grouped"
            | "store-baseline"
            | "complete"
    ) {
        return Err(AppError::validation("未知流水线检查点名称"));
    }
    fs::create_dir_all(task_dir)?;
    ensure_plain_directory(task_dir)?;
    Ok(task_dir.join(format!("{name}.json")))
}

fn canonical_task_dir(task_dir: &Path) -> AppResult<PathBuf> {
    fs::create_dir_all(task_dir)?;
    ensure_plain_directory(task_dir)?;
    task_dir
        .canonicalize()
        .map_err(|error| AppError::io(format!("无法解析任务暂存目录: {error}")))
}

fn canonical_regular_file(path: &Path) -> AppResult<PathBuf> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::validation("检查点目标必须是普通文件"));
    }
    path.canonicalize()
        .map_err(|error| AppError::io(format!("无法解析检查点文件: {error}")))
}

fn ensure_plain_directory(path: &Path) -> AppResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::validation("任务暂存路径必须是普通本地目录"));
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> AppResult<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AppError::validation("检查点包含不安全相对路径"));
    }
    Ok(())
}

fn validate_originals_names(originals: &Path, expected: &[OriginalFile]) -> AppResult<()> {
    let expected_names: HashSet<&str> = expected.iter().map(|file| file.path.as_str()).collect();
    for item in fs::read_dir(originals)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        let allowed = expected_names.contains(name.as_str())
            || name == ORIGINALS_MANIFEST
            || name == ".originals-manifest.json.tmp"
            || expected.iter().any(|file| {
                name.starts_with(&format!(".{}.", file.path)) && name.ends_with(".tmp")
            });
        if !allowed {
            return Err(AppError::validation(format!(
                "原件目录包含未知文件 {name}；不会删除或覆盖"
            )));
        }
    }
    Ok(())
}

const MAX_ORIGINAL_BASENAME_CHARS: usize = 51;

fn sanitize_filename(name: &str) -> String {
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
    let stem_budget = MAX_ORIGINAL_BASENAME_CHARS
        .saturating_sub(suffix.chars().count())
        .max(1);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("invoice");
    let value: String = stem
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(stem_budget)
        .collect();
    // Win32 会静默移除文件名末尾的句点和空格。如果把未归一化的名字写入
    // 清单，随后 read_dir 得到的实际名字会不同，恢复校验就会把刚复制的
    // 原件误判为未知文件。
    let value = value.trim_end_matches(['.', ' ']);
    let stem = if value.is_empty() { "invoice" } else { value };
    format!("{stem}{suffix}")
}

fn read_limited(path: &Path, limit: u64) -> AppResult<Vec<u8>> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(AppError::validation("检查点文件类型或大小无效"));
    }
    fs::read(path).map_err(Into::into)
}

fn write_new(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn copy_new_synced(source: &Path, destination: &Path) -> AppResult<()> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.flush()?;
    output.sync_all()?;
    Ok(())
}

fn hash_file(path: &Path) -> AppResult<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:X}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use invoice_parse::model::{ParseLevel, TicketType};
    use rust_decimal::Decimal;

    fn parsed(source_path: PathBuf) -> ParsedInvoice {
        ParsedInvoice {
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            total_amount: Decimal::new(10000, 2),
            tax_amount: None,
            tax_rate: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Other,
            transport_document_kind: Default::default(),
            parse_level: ParseLevel::L0,
            confidence: 1.0,
            city: Some("北京".to_string()),
            travel_route: None,
            departure_time: None,
            checkin_date: None,
            source_path,
        }
    }

    #[test]
    fn collected_checkpoint_roundtrips_and_detects_tampering() {
        let root = tempfile::tempdir().unwrap();
        let task = root.path().join("task");
        fs::create_dir_all(&task).unwrap();
        let source = task.join("invoice.xml");
        fs::write(&source, b"stable").unwrap();
        write_collected(&task, std::slice::from_ref(&source)).unwrap();
        assert_eq!(load_collected(&task).unwrap().len(), 1);
        fs::write(&source, b"changed").unwrap();
        assert!(load_collected(&task).is_err());
    }

    #[test]
    fn source_notices_checkpoint_roundtrips_and_unknown_name_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let task = root.path().join("task");
        let value = serde_json::json!({
            "formatVersion": 1,
            "linkOnlyEmailCount": 2
        });

        write_json_checkpoint(&task, "source-notices", &value).unwrap();
        let loaded: serde_json::Value = read_json_checkpoint(&task, "source-notices").unwrap();
        assert_eq!(loaded, value);
        assert!(write_json_checkpoint(&task, "unexpected", &value).is_err());
    }

    #[test]
    fn legacy_email_ledger_checkpoint_name_remains_readable() {
        let root = tempfile::tempdir().unwrap();
        let task = root.path().join("task");
        let value = serde_json::json!([{"uid": 42, "status": "imported"}]);

        write_json_checkpoint(&task, "email-ledger", &value).unwrap();
        let loaded: serde_json::Value = read_json_checkpoint(&task, "email-ledger").unwrap();
        assert_eq!(loaded, value);
    }

    #[test]
    fn parsed_source_validation_compares_canonical_paths() {
        let root = tempfile::tempdir().unwrap();
        let task = root.path().join("task");
        fs::create_dir_all(&task).unwrap();
        let source = task.join("invoice.xml");
        fs::write(&source, b"stable").unwrap();
        let lexical = task.join("child").join("..").join("invoice.xml");

        validate_parsed_sources(&[parsed(lexical)], &[source]).unwrap();
    }

    #[test]
    fn parsed_checkpoint_saves_versions_and_rejects_future_format() {
        let root = tempfile::tempdir().unwrap();
        let task = root.path().join("task");
        fs::create_dir_all(&task).unwrap();
        let source = task.join("invoice.xml");
        fs::write(&source, b"stable").unwrap();

        write_parsed(&task, &[parsed(source.clone())]).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(task.join("parsed.json")).unwrap()).unwrap();
        assert_eq!(
            saved["formatVersion"],
            serde_json::json!(PARSED_RESULT_FORMAT_VERSION)
        );
        assert_eq!(saved["parserVersion"], invoice_parse::PARSER_VERSION);
        assert_eq!(load_parsed(&task).unwrap()[0].source_path, source);

        fs::write(
            task.join("parsed.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "formatVersion": PARSED_RESULT_FORMAT_VERSION + 1,
                "parserVersion": invoice_parse::PARSER_VERSION,
                "invoices": []
            }))
            .unwrap(),
        )
        .unwrap();
        let error = load_parsed(&task).unwrap_err();
        assert!(error.message().contains("版本不受支持"));
    }

    #[test]
    fn originals_are_resumable_and_unknown_files_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let task = root.path().join("task");
        fs::create_dir_all(&task).unwrap();
        let source = task.join("invoice.xml");
        fs::write(&source, b"stable").unwrap();
        let pipeline_id = "44444444-4444-4444-8444-444444444444";
        let first = prepare_originals(root.path(), pipeline_id, &[parsed(source.clone())]).unwrap();
        let second = prepare_originals(root.path(), pipeline_id, &[parsed(source)]).unwrap();
        assert_eq!(first, second);
        fs::write(first[0].parent().unwrap().join("unknown.txt"), b"x").unwrap();
        assert!(prepare_originals(
            root.path(),
            pipeline_id,
            &[parsed(task.join("invoice.xml"))]
        )
        .is_err());
    }

    #[test]
    fn sanitized_original_name_is_bounded_preserves_extension_and_avoids_windows_trim() {
        let source = format!("{}.pdf", "a".repeat(119));
        let sanitized = sanitize_filename(&source);

        assert!(sanitized.chars().count() <= MAX_ORIGINAL_BASENAME_CHARS);
        assert!(sanitized.ends_with(".pdf"));
        assert!(!sanitized.ends_with(['.', ' ']));
    }

    #[test]
    fn prepared_original_component_stays_below_legacy_windows_path_budget() {
        let root = tempfile::tempdir().unwrap();
        let task = root.path().join("task");
        fs::create_dir_all(&task).unwrap();
        let source = task.join(format!("{}.PDF", "很长的发票文件名".repeat(10)));
        fs::write(&source, b"stable").unwrap();

        let paths = prepare_originals(
            root.path(),
            "55555555-5555-4555-8555-555555555555",
            &[parsed(source)],
        )
        .unwrap();
        let component = paths[0].file_name().unwrap().to_string_lossy();

        assert!(component.chars().count() <= MAX_ORIGINAL_BASENAME_CHARS + 5);
        assert!(component.ends_with(".pdf"));
        assert!(paths[0].is_file());
    }
}
