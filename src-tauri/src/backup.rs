//! 用户主动触发的未加密备份与跨电脑导入。

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;
use uuid::Uuid;
use zip::write::SimpleFileOptions;

use crate::error::{AppError, AppResult};
use crate::AppState;

const BACKUP_FORMAT_VERSION: u32 = 1;
const DATABASE_SCHEMA_VERSION: u32 = invoice_store::LEDGER_SCHEMA_VERSION as u32;
// 备份功能从 ledger v6 开始提供。旧备份在真正切换数据后由正常启动迁移，
// 校验/预览阶段必须保持只读，不能先改写已校验的 ZIP 内容。
const MIN_IMPORT_DATABASE_SCHEMA_VERSION: u32 = 6;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_ENTRY_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MANIFEST_NAME: &str = "backup-manifest.json";
const PENDING_MARKER: &str = ".invoice-assistant-import-pending.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackupEntry {
    path: String,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackupManifest {
    format_version: u32,
    database_schema_version: u32,
    product_version: String,
    created_at_utc: String,
    unencrypted: bool,
    entries: Vec<BackupEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupExportResult {
    pub file_count: usize,
    pub total_bytes: u64,
    pub archive_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupImportPreview {
    pub format_version: u32,
    pub created_at_utc: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub warning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingImport {
    staged_directory: PathBuf,
}

struct SourceFile {
    archive_path: String,
    source_path: PathBuf,
    bytes: u64,
    sha256: String,
}

#[tauri::command]
pub fn export_backup(
    destination_path: String,
    state: State<Mutex<AppState>>,
) -> AppResult<BackupExportResult> {
    let destination = PathBuf::from(destination_path);
    validate_backup_destination(&destination)?;
    let data_root = crate::paths::data_root().map_err(AppError::from)?;
    if destination.starts_with(&data_root) {
        return Err(AppError::validation("备份文件不能保存到应用数据目录内"));
    }

    let temp_root = crate::paths::temp_dir().map_err(AppError::from)?;
    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {e}")))?;
    export_backup_from_db(&destination, &data_root, &temp_root, app_state.ledger_db()?)
}

fn export_backup_from_db(
    destination: &Path,
    data_root: &Path,
    temp_root: &Path,
    db: &invoice_store::LedgerDb,
) -> AppResult<BackupExportResult> {
    fs::create_dir_all(temp_root)?;
    let database_snapshot = temp_root.join(format!("ledger-backup-{}.db", Uuid::new_v4()));
    let partial_archive = destination.with_extension(format!("partial-{}", Uuid::new_v4()));

    let result = (|| -> AppResult<BackupExportResult> {
        db.backup_to(&database_snapshot)
            .map_err(|e| AppError::database(format!("创建数据库一致性快照失败: {e}")))?;

        let mut sources = vec![source_file("data/ledger.db", &database_snapshot)?];
        let originals = data_root.join("files");
        if originals.exists() {
            collect_source_files(&originals, &originals, "data/files", &mut sources)?;
        }
        let collected_materials = data_root.join("collection-files");
        if collected_materials.exists() {
            collect_source_files(
                &collected_materials,
                &collected_materials,
                "data/collection-files",
                &mut sources,
            )?;
        }
        if sources.len() > MAX_ARCHIVE_ENTRIES {
            return Err(AppError::validation("备份文件数量超过上限"));
        }
        let total_bytes = sources
            .iter()
            .try_fold(0u64, |total, item| total.checked_add(item.bytes))
            .ok_or_else(|| anyhow::anyhow!("备份大小溢出"))?;
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(AppError::validation("备份内容超过 5 GiB 上限"));
        }

        let manifest = BackupManifest {
            format_version: BACKUP_FORMAT_VERSION,
            database_schema_version: DATABASE_SCHEMA_VERSION,
            product_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at_utc: chrono::Utc::now().to_rfc3339(),
            unencrypted: true,
            entries: sources
                .iter()
                .map(|item| BackupEntry {
                    path: item.archive_path.clone(),
                    bytes: item.bytes,
                    sha256: item.sha256.clone(),
                })
                .collect(),
        };
        write_archive(&partial_archive, &manifest, &sources)?;
        fs::rename(&partial_archive, destination)?;
        let archive_sha256 = sha256_file(destination)?;
        Ok(BackupExportResult {
            file_count: sources.len(),
            total_bytes,
            archive_sha256,
        })
    })();

    let _ = fs::remove_file(&database_snapshot);
    if result.is_err() {
        let _ = fs::remove_file(&partial_archive);
    }
    result
}

#[tauri::command]
pub fn preview_backup_import(backup_path: String) -> AppResult<BackupImportPreview> {
    let path = PathBuf::from(backup_path);
    let manifest = validate_archive(&path)?;
    Ok(preview_from_manifest(&manifest))
}

#[tauri::command]
pub fn stage_backup_import(backup_path: String) -> AppResult<BackupImportPreview> {
    let source = PathBuf::from(backup_path);
    let manifest = validate_archive(&source)?;
    let data_root = crate::paths::data_root().map_err(AppError::from)?;
    let parent = data_root
        .parent()
        .ok_or_else(|| AppError::validation("数据目录缺少安全父目录"))?;
    let marker_path = parent.join(PENDING_MARKER);
    if marker_path.exists() {
        return Err(AppError::validation("已有待导入备份；请先重启应用完成导入"));
    }

    let staged = parent.join(format!(".invoice-assistant-import-{}", Uuid::new_v4()));
    let result = (|| -> AppResult<()> {
        fs::create_dir(&staged)?;
        extract_validated_archive(&source, &staged, &manifest)?;
        validate_staged_directory(&staged, &manifest)?;
        let pending = PendingImport {
            staged_directory: staged.clone(),
        };
        let marker_bytes = serde_json::to_vec_pretty(&pending)
            .map_err(|e| AppError::internal(format!("生成导入标记失败: {e}")))?;
        let marker_temp = parent.join(format!("{PENDING_MARKER}.{}.tmp", Uuid::new_v4()));
        fs::write(&marker_temp, marker_bytes)?;
        fs::rename(&marker_temp, &marker_path)?;
        Ok(())
    })();
    if result.is_err() {
        safe_remove_generated_dir(parent, &staged);
    }
    result?;
    Ok(preview_from_manifest(&manifest))
}

/// 在数据库打开前应用已完全验证的导入。成功后保留本机回滚目录。
pub fn apply_pending_import(data_root: &Path) -> anyhow::Result<Option<PathBuf>> {
    let parent = data_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("数据目录缺少安全父目录"))?;
    let marker_path = parent.join(PENDING_MARKER);
    if !marker_path.exists() {
        return Ok(None);
    }
    let pending: PendingImport = serde_json::from_slice(&fs::read(&marker_path)?)?;
    ensure_generated_child(
        parent,
        &pending.staged_directory,
        ".invoice-assistant-import-",
    )?;
    let manifest: BackupManifest =
        serde_json::from_slice(&fs::read(pending.staged_directory.join(MANIFEST_NAME))?)?;
    validate_staged_directory(&pending.staged_directory, &manifest)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    fs::create_dir_all(data_root)?;
    let rollback = parent.join(format!(
        ".invoice-assistant-import-rollback-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    ));
    fs::create_dir(&rollback)?;
    let staged_data = pending.staged_directory.join("data");
    let names = ["ledger.db", "files", "collection-files"];
    let mut old_moved = Vec::new();
    let mut new_moved = Vec::new();

    let switch_result = (|| -> std::io::Result<()> {
        for name in names {
            let current = data_root.join(name);
            if current.exists() {
                fs::rename(&current, rollback.join(name))?;
                old_moved.push(name);
            }
        }
        for name in names {
            let imported = staged_data.join(name);
            if imported.exists() {
                fs::rename(&imported, data_root.join(name))?;
                new_moved.push(name);
            }
        }
        Ok(())
    })();

    if let Err(error) = switch_result {
        for name in new_moved.into_iter().rev() {
            let current = data_root.join(name);
            if current.exists() {
                let _ = fs::rename(current, staged_data.join(name));
            }
        }
        for name in old_moved.into_iter().rev() {
            let old = rollback.join(name);
            if old.exists() {
                let _ = fs::rename(old, data_root.join(name));
            }
        }
        let _ = fs::remove_dir(&rollback);
        return Err(anyhow::anyhow!("导入切换失败，原数据已回滚: {error}"));
    }

    fs::remove_file(&marker_path)?;
    safe_remove_generated_dir(parent, &pending.staged_directory);
    Ok(Some(rollback))
}

fn validate_backup_destination(path: &Path) -> AppResult<()> {
    if !path.is_absolute() {
        return Err(AppError::validation("备份保存路径必须是绝对路径"));
    }
    if path.extension().and_then(|value| value.to_str()) != Some("zip") {
        return Err(AppError::validation("备份文件扩展名必须是 .zip"));
    }
    if path.exists() {
        return Err(AppError::validation("目标备份文件已存在，不会静默覆盖"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| AppError::validation("备份路径缺少父目录"))?;
    if !parent.is_dir() {
        return Err(AppError::validation("备份目标文件夹不存在"));
    }
    Ok(())
}

fn collect_source_files(
    root: &Path,
    current: &Path,
    archive_prefix: &str,
    out: &mut Vec<SourceFile>,
) -> AppResult<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        if metadata.is_symlink() {
            continue;
        }
        let path = entry.path();
        if metadata.is_dir() {
            collect_source_files(root, &path, archive_prefix, out)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| AppError::validation("备份文件不在预期数据目录"))?;
            let relative = safe_archive_relative_path(relative)?;
            out.push(source_file(&format!("{archive_prefix}/{relative}"), &path)?);
        }
    }
    Ok(())
}

fn safe_archive_relative_path(path: &Path) -> AppResult<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            _ => return Err(AppError::validation("备份路径包含不安全组件")),
        }
    }
    if parts.is_empty() {
        return Err(AppError::validation("备份路径为空"));
    }
    Ok(parts.join("/"))
}

fn source_file(archive_path: &str, source_path: &Path) -> AppResult<SourceFile> {
    let bytes = fs::metadata(source_path)?.len();
    if bytes > MAX_ENTRY_BYTES {
        return Err(AppError::validation("单个备份文件超过 1 GiB 上限"));
    }
    Ok(SourceFile {
        archive_path: archive_path.to_string(),
        source_path: source_path.to_path_buf(),
        bytes,
        sha256: sha256_file(source_path)?,
    })
}

fn write_archive(path: &Path, manifest: &BackupManifest, sources: &[SourceFile]) -> AppResult<()> {
    let file = File::create(path)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let manifest_bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|e| AppError::internal(format!("生成备份清单失败: {e}")))?;
    writer.start_file(MANIFEST_NAME, options)?;
    writer.write_all(&manifest_bytes)?;
    for source in sources {
        writer.start_file(&source.archive_path, options)?;
        let mut input = File::open(&source.source_path)?;
        std::io::copy(&mut input, &mut writer)?;
    }
    writer.finish()?;
    Ok(())
}

fn validate_archive(path: &Path) -> AppResult<BackupManifest> {
    if !path.is_absolute() || !path.is_file() {
        return Err(AppError::validation("请选择存在的本机备份 ZIP"));
    }
    let file = File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|_| AppError::validation("备份 ZIP 无法解析或已损坏"))?;
    if archive.is_empty() || archive.len() > MAX_ARCHIVE_ENTRIES + 1 {
        return Err(AppError::validation("备份 ZIP 条目数量超限"));
    }
    let manifest_file = archive
        .by_name(MANIFEST_NAME)
        .map_err(|_| AppError::validation("备份缺少清单"))?;
    if manifest_file.size() > MAX_MANIFEST_BYTES {
        return Err(AppError::validation("备份清单超过大小上限"));
    }
    let mut bytes = Vec::new();
    manifest_file
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let manifest: BackupManifest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::validation("备份清单格式无效"))?;
    validate_manifest(&manifest)?;

    let mut archive_paths = std::collections::HashSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| AppError::validation("备份包含路径穿越条目"))?;
        let name = safe_archive_relative_path(&enclosed)?;
        if !archive_paths.insert(name.clone()) {
            return Err(AppError::validation("备份包含重复路径"));
        }
        if entry.size() > MAX_ENTRY_BYTES {
            return Err(AppError::validation("备份包含超大条目"));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| AppError::validation("备份解压大小溢出"))?;
        if total > MAX_TOTAL_BYTES + MAX_MANIFEST_BYTES {
            return Err(AppError::validation("备份解压内容超过 5 GiB 上限"));
        }
    }
    let expected = manifest
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<std::collections::HashSet<_>>();
    if archive_paths.len() != expected.len() + 1
        || !archive_paths.contains(MANIFEST_NAME)
        || archive_paths
            .iter()
            .any(|path| path != MANIFEST_NAME && !expected.contains(path.as_str()))
        || expected.len() != manifest.entries.len()
        || !manifest
            .entries
            .iter()
            .all(|entry| archive_paths.contains(&entry.path))
    {
        return Err(AppError::validation("备份清单与 ZIP 内容不一致"));
    }
    Ok(manifest)
}

fn validate_manifest(manifest: &BackupManifest) -> AppResult<()> {
    if manifest.format_version != BACKUP_FORMAT_VERSION
        || manifest.database_schema_version < MIN_IMPORT_DATABASE_SCHEMA_VERSION
        || manifest.database_schema_version > DATABASE_SCHEMA_VERSION
        || !manifest.unencrypted
    {
        return Err(AppError::validation("不支持的备份格式或数据库版本"));
    }
    if manifest.entries.is_empty() || manifest.entries.len() > MAX_ARCHIVE_ENTRIES {
        return Err(AppError::validation("备份清单条目数量无效"));
    }
    if !manifest
        .entries
        .iter()
        .any(|entry| entry.path == "data/ledger.db")
    {
        return Err(AppError::validation("备份缺少 ledger.db"));
    }
    for entry in &manifest.entries {
        let path = Path::new(&entry.path);
        let normalized = safe_archive_relative_path(path)?;
        if normalized != entry.path
            || !(entry.path == "data/ledger.db"
                || entry.path.starts_with("data/files/")
                || entry.path.starts_with("data/collection-files/"))
            || entry.bytes > MAX_ENTRY_BYTES
            || entry.sha256.len() != 64
            || !entry.sha256.chars().all(|ch| ch.is_ascii_hexdigit())
        {
            return Err(AppError::validation("备份清单包含无效条目"));
        }
    }
    Ok(())
}

fn extract_validated_archive(
    source: &Path,
    staged: &Path,
    manifest: &BackupManifest,
) -> AppResult<()> {
    let mut archive = zip::ZipArchive::new(File::open(source)?)?;
    fs::create_dir_all(staged.join("data/files"))?;
    fs::create_dir_all(staged.join("data/collection-files"))?;
    fs::write(
        staged.join(MANIFEST_NAME),
        serde_json::to_vec_pretty(manifest)
            .map_err(|e| AppError::internal(format!("写入备份清单失败: {e}")))?,
    )?;
    for expected in &manifest.entries {
        let entry = archive.by_name(&expected.path)?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| AppError::validation("备份包含路径穿越条目"))?;
        let output = staged.join(relative);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut target = OpenOptionsExt::create_new_file(&output)?;
        let copied = std::io::copy(&mut entry.take(MAX_ENTRY_BYTES + 1), &mut target)?;
        if copied != expected.bytes || copied > MAX_ENTRY_BYTES {
            return Err(AppError::validation("备份条目大小与清单不一致"));
        }
        target.sync_all()?;
    }
    Ok(())
}

struct OpenOptionsExt;

impl OpenOptionsExt {
    fn create_new_file(path: &Path) -> std::io::Result<File> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    }
}

fn validate_staged_directory(staged: &Path, manifest: &BackupManifest) -> AppResult<()> {
    validate_manifest(manifest)?;
    let expected = manifest
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut actual = std::collections::HashSet::new();
    collect_staged_files(staged, &staged.join("data"), &mut actual)?;
    if actual != expected {
        return Err(AppError::validation(
            "导入暂存目录包含未列入清单的文件或缺少文件",
        ));
    }
    for expected in &manifest.entries {
        let path = staged.join(Path::new(&expected.path));
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != expected.bytes
        {
            return Err(AppError::validation("导入暂存内容类型或大小无效"));
        }
        if sha256_file(&path)? != expected.sha256.to_ascii_uppercase() {
            return Err(AppError::validation("备份内容哈希校验失败"));
        }
    }
    let ledger = staged.join("data/ledger.db");
    let actual_schema = invoice_store::LedgerDb::inspect_existing_database(&ledger)
        .map_err(|e| AppError::database(format!("导入数据库完整性检查失败: {e}")))?;
    if actual_schema < 0 || actual_schema as u32 != manifest.database_schema_version {
        return Err(AppError::validation("备份清单与数据库版本不一致"));
    }
    Ok(())
}

fn collect_staged_files(
    root: &Path,
    current: &Path,
    out: &mut std::collections::HashSet<String>,
) -> AppResult<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(AppError::validation("导入暂存目录包含符号链接或联接点"));
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_staged_files(root, &path, out)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| AppError::validation("导入暂存文件越过安全根目录"))?;
            out.insert(safe_archive_relative_path(relative)?);
        } else {
            return Err(AppError::validation("导入暂存目录包含不支持的文件类型"));
        }
    }
    Ok(())
}

fn preview_from_manifest(manifest: &BackupManifest) -> BackupImportPreview {
    BackupImportPreview {
        format_version: manifest.format_version,
        created_at_utc: manifest.created_at_utc.clone(),
        file_count: manifest.entries.len(),
        total_bytes: manifest.entries.iter().map(|entry| entry.bytes).sum(),
        warning: "此备份未加密；导入将在重启后替换本机发票台账和原件，不包含邮箱授权码。"
            .to_string(),
    }
}

fn sha256_file(path: &Path) -> AppResult<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:X}", hasher.finalize()))
}

fn ensure_generated_child(parent: &Path, child: &Path, prefix: &str) -> anyhow::Result<()> {
    anyhow::ensure!(child.is_absolute(), "导入暂存目录必须是绝对路径");
    anyhow::ensure!(child.parent() == Some(parent), "导入暂存目录不在安全父目录");
    let name = child
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    anyhow::ensure!(name.starts_with(prefix), "导入暂存目录名称无效");
    let metadata = fs::symlink_metadata(child)?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "导入暂存目录类型无效"
    );
    Ok(())
}

fn safe_remove_generated_dir(parent: &Path, child: &Path) {
    if ensure_generated_child(parent, child, ".invoice-assistant-import-").is_ok() {
        let _ = fs::remove_dir_all(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "invoice-assistant-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir(&root).unwrap();
        root
    }

    fn manifest_for(sources: &[SourceFile]) -> BackupManifest {
        BackupManifest {
            format_version: 1,
            database_schema_version: DATABASE_SCHEMA_VERSION,
            product_version: "0.1.0".to_string(),
            created_at_utc: "2026-08-19T00:00:00Z".to_string(),
            unencrypted: true,
            entries: sources
                .iter()
                .map(|source| BackupEntry {
                    path: source.archive_path.clone(),
                    bytes: source.bytes,
                    sha256: source.sha256.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn backup_manifest_schema_tracks_ledger_schema() {
        assert_eq!(
            DATABASE_SCHEMA_VERSION,
            invoice_store::LEDGER_SCHEMA_VERSION as u32
        );
    }

    #[test]
    fn import_accepts_v6_backup_for_post_switch_migration_only() {
        let source = SourceFile {
            archive_path: "data/ledger.db".to_string(),
            source_path: PathBuf::from("ledger.db"),
            bytes: 1,
            sha256: "A".repeat(64),
        };
        let mut manifest = manifest_for(&[source]);
        manifest.database_schema_version = 6;
        assert!(validate_manifest(&manifest).is_ok());

        manifest.database_schema_version = MIN_IMPORT_DATABASE_SCHEMA_VERSION - 1;
        assert!(validate_manifest(&manifest).is_err());
        manifest.database_schema_version = DATABASE_SCHEMA_VERSION + 1;
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn rejects_backup_destination_without_zip_extension() {
        let path = std::env::temp_dir().join("invoice-assistant-backup.txt");
        assert!(validate_backup_destination(&path).is_err());
    }

    #[test]
    fn rejects_manifest_path_traversal() {
        let manifest = BackupManifest {
            format_version: 1,
            database_schema_version: DATABASE_SCHEMA_VERSION,
            product_version: "0.1.0".to_string(),
            created_at_utc: "2026-08-19T00:00:00Z".to_string(),
            unencrypted: true,
            entries: vec![BackupEntry {
                path: "../ledger.db".to_string(),
                bytes: 1,
                sha256: "A".repeat(64),
            }],
        };
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn valid_archive_roundtrip_detects_tampering() {
        let root = test_root("backup-roundtrip");
        let ledger_path = root.join("ledger.db");
        let db = invoice_store::LedgerDb::new(&ledger_path).unwrap();
        db.create_batch("跨电脑备份", "2026-08").unwrap();
        drop(db);
        let original = root.join("invoice.xml");
        fs::write(&original, b"<invoice>synthetic</invoice>").unwrap();
        let sources = vec![
            source_file("data/ledger.db", &ledger_path).unwrap(),
            source_file("data/files/invoice.xml", &original).unwrap(),
        ];
        let manifest = manifest_for(&sources);
        let archive_path = root.join("backup.zip");
        write_archive(&archive_path, &manifest, &sources).unwrap();

        let validated = validate_archive(&archive_path).unwrap();
        let staged = root.join("staged");
        fs::create_dir(&staged).unwrap();
        extract_validated_archive(&archive_path, &staged, &validated).unwrap();
        validate_staged_directory(&staged, &validated).unwrap();

        fs::write(staged.join("data/files/invoice.xml"), b"tampered").unwrap();
        assert!(validate_staged_directory(&staged, &validated).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn full_backup_transfer_replaces_target_ledger_and_keeps_machine_profile() {
        let _guard = crate::paths::test_env_lock();
        let root = test_root("cross-root-transfer");
        let source_data = root.join("source").join("Data");
        fs::create_dir_all(source_data.join("files")).unwrap();
        let source_db = invoice_store::LedgerDb::new(source_data.join("ledger.db")).unwrap();
        source_db.create_batch("源电脑 2026-08", "2026-08").unwrap();
        fs::write(
            source_data.join("files").join("invoice.xml"),
            b"<invoice>source-machine</invoice>",
        )
        .unwrap();
        fs::create_dir_all(source_data.join("collection-files").join("task-1")).unwrap();
        fs::write(
            source_data
                .join("collection-files")
                .join("task-1")
                .join("mail-invoice.pdf"),
            b"mail-source-material",
        )
        .unwrap();

        let archive_path = root.join("transfer.zip");
        let export = export_backup_from_db(
            &archive_path,
            &source_data,
            &source_data.join("temp"),
            &source_db,
        )
        .unwrap();
        drop(source_db);
        assert_eq!(export.file_count, 3);
        assert_eq!(export.archive_sha256.len(), 64);
        let preview = preview_backup_import(archive_path.to_string_lossy().into_owned()).unwrap();
        assert_eq!(preview.format_version, BACKUP_FORMAT_VERSION);
        assert_eq!(preview.file_count, 3);

        let target_data = root.join("target").join("Data");
        fs::create_dir_all(target_data.join("files")).unwrap();
        fs::create_dir_all(target_data.join("collection-files")).unwrap();
        let target_db = invoice_store::LedgerDb::new(target_data.join("ledger.db")).unwrap();
        target_db.create_batch("目标电脑旧台账", "2026-07").unwrap();
        drop(target_db);
        fs::write(target_data.join("files").join("old.xml"), b"old").unwrap();
        fs::write(
            target_data.join("collection-files").join("old.pdf"),
            b"old-mail-material",
        )
        .unwrap();
        fs::write(target_data.join("accounts.db"), b"target-machine-profile").unwrap();

        std::env::set_var(crate::paths::DATA_ROOT_OVERRIDE, &target_data);
        let staged = stage_backup_import(archive_path.to_string_lossy().into_owned()).unwrap();
        assert_eq!(staged.file_count, 3);
        let rollback = apply_pending_import(&target_data).unwrap().unwrap();
        std::env::remove_var(crate::paths::DATA_ROOT_OVERRIDE);

        let imported = invoice_store::LedgerDb::new(target_data.join("ledger.db")).unwrap();
        let imported_batches = imported.list_batches().unwrap();
        assert_eq!(imported_batches.len(), 1);
        assert_eq!(imported_batches[0].name, "源电脑 2026-08");
        drop(imported);
        assert_eq!(
            fs::read(target_data.join("files").join("invoice.xml")).unwrap(),
            b"<invoice>source-machine</invoice>"
        );
        assert!(!target_data.join("files").join("old.xml").exists());
        assert_eq!(
            fs::read(
                target_data
                    .join("collection-files")
                    .join("task-1")
                    .join("mail-invoice.pdf")
            )
            .unwrap(),
            b"mail-source-material"
        );
        assert!(!target_data
            .join("collection-files")
            .join("old.pdf")
            .exists());
        assert_eq!(
            fs::read(target_data.join("accounts.db")).unwrap(),
            b"target-machine-profile"
        );
        let previous = invoice_store::LedgerDb::new(rollback.join("ledger.db")).unwrap();
        assert_eq!(previous.list_batches().unwrap()[0].name, "目标电脑旧台账");
        drop(previous);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_archive_with_path_traversal_entry() {
        let root = test_root("backup-path-traversal");
        let ledger_path = root.join("ledger.db");
        let db = invoice_store::LedgerDb::new(&ledger_path).unwrap();
        drop(db);
        let sources = vec![source_file("data/ledger.db", &ledger_path).unwrap()];
        let manifest = manifest_for(&sources);
        let archive_path = root.join("malicious.zip");
        let mut writer = zip::ZipWriter::new(File::create(&archive_path).unwrap());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        writer.start_file(MANIFEST_NAME, options).unwrap();
        writer
            .write_all(&serde_json::to_vec_pretty(&manifest).unwrap())
            .unwrap();
        writer.start_file("data/ledger.db", options).unwrap();
        let mut ledger = File::open(&ledger_path).unwrap();
        std::io::copy(&mut ledger, &mut writer).unwrap();
        writer.start_file("../outside.txt", options).unwrap();
        writer.write_all(b"must not escape").unwrap();
        writer.finish().unwrap();

        assert!(validate_archive(&archive_path).is_err());
        assert!(!root.join("outside.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn pending_import_atomically_replaces_product_data_and_keeps_machine_profile() {
        let parent = test_root("atomic-import");
        let data_root = parent.join("Data");
        fs::create_dir_all(data_root.join("files")).unwrap();
        let old_db = invoice_store::LedgerDb::new(data_root.join("ledger.db")).unwrap();
        old_db.create_batch("旧台账", "2026-07").unwrap();
        drop(old_db);
        fs::write(data_root.join("files/old.xml"), b"old").unwrap();
        fs::write(data_root.join("accounts.db"), b"machine-local-profile").unwrap();

        let staged = parent.join(format!(".invoice-assistant-import-{}", Uuid::new_v4()));
        fs::create_dir_all(staged.join("data/files")).unwrap();
        let imported_db = invoice_store::LedgerDb::new(staged.join("data/ledger.db")).unwrap();
        imported_db.create_batch("新台账", "2026-08").unwrap();
        drop(imported_db);
        fs::write(staged.join("data/files/new.xml"), b"new").unwrap();
        let sources = vec![
            source_file("data/ledger.db", &staged.join("data/ledger.db")).unwrap(),
            source_file("data/files/new.xml", &staged.join("data/files/new.xml")).unwrap(),
        ];
        let manifest = manifest_for(&sources);
        fs::write(
            staged.join(MANIFEST_NAME),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let marker = PendingImport {
            staged_directory: staged,
        };
        fs::write(
            parent.join(PENDING_MARKER),
            serde_json::to_vec_pretty(&marker).unwrap(),
        )
        .unwrap();

        let rollback = apply_pending_import(&data_root).unwrap().unwrap();
        let imported = invoice_store::LedgerDb::new(data_root.join("ledger.db")).unwrap();
        let imported_batches = imported.list_batches().unwrap();
        assert_eq!(imported_batches.len(), 1);
        assert_eq!(imported_batches[0].name, "新台账");
        drop(imported);
        assert!(data_root.join("files/new.xml").is_file());
        assert!(!data_root.join("files/old.xml").exists());
        assert_eq!(
            fs::read(data_root.join("accounts.db")).unwrap(),
            b"machine-local-profile"
        );

        let previous = invoice_store::LedgerDb::new(rollback.join("ledger.db")).unwrap();
        assert_eq!(previous.list_batches().unwrap()[0].name, "旧台账");
        drop(previous);
        fs::remove_dir_all(parent).unwrap();
    }
}
