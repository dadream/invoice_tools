//! 本地发票/配套票据文件和目录输入收集。
//!
//! 该模块只发现用户显式选择的路径，不扫描其他目录；不跟随符号链接，
//! 并对数量和大小设置上限。真实路径不会写入日志。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use invoice_collect::dedupe;

const MAX_INPUT_FILES: usize = 5_000;
const MAX_RECURSION_DEPTH: usize = 8;
pub(crate) const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_TOTAL_STAGED_BYTES: u64 = 500 * 1024 * 1024;
const MAX_STAGED_SOURCE_NAME_CHARS: usize = 120;
// OCR/PDF native dependencies are not uniformly long-path aware on Windows. Keep
// working paths below the traditional MAX_PATH boundary and leave room for their
// own temporary suffixes.
const MAX_STAGED_PATH_CHARS: usize = 240;

#[derive(Debug, Default)]
pub struct LocalCollection {
    pub files: Vec<PathBuf>,
    pub skipped: usize,
    pub duplicates: usize,
    pub total_bytes: u64,
    /// 疑似通过正文链接交付、且没有可直接处理附件的邮件数。
    pub link_only_emails: usize,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct LocalInputPreview {
    pub parseable_files: usize,
    pub skipped: usize,
    pub duplicates: usize,
    pub total_bytes: u64,
}

/// 在不创建任务和不复制文件的情况下检查本地选择。读取内容仅用于同次选择内
/// 的 SHA-256 去重；返回聚合统计，不把真实路径送回 UI 或日志。
pub fn preview_local_inputs(roots: &[PathBuf]) -> Result<LocalInputPreview> {
    if roots.is_empty() {
        bail!("请至少选择一个文件或文件夹");
    }

    let mut candidates = Vec::new();
    let mut skipped = 0;
    for root in roots {
        discover_path(root, 0, &mut candidates, &mut skipped)?;
        if candidates.len() > MAX_INPUT_FILES {
            bail!("选择的文件过多，单次最多处理 {MAX_INPUT_FILES} 个文件");
        }
    }
    candidates.sort();

    let mut preview = LocalInputPreview {
        skipped,
        ..LocalInputPreview::default()
    };
    let mut hashes = HashSet::new();
    for candidate in candidates {
        if !is_parseable_extension(&extension_of(&candidate)) {
            preview.skipped += 1;
            continue;
        }
        let metadata = fs::metadata(&candidate).context("无法读取本地文件属性")?;
        if metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
            preview.skipped += 1;
            continue;
        }
        let bytes = fs::read(&candidate).context("无法读取本地文件")?;
        if !hashes.insert(dedupe::sha256_hex(&bytes)) {
            preview.duplicates += 1;
            continue;
        }
        preview.total_bytes = checked_staged_total(
            preview.parseable_files,
            preview.total_bytes,
            bytes.len() as u64,
        )?;
        preview.parseable_files += 1;
    }
    Ok(preview)
}

/// 收集用户显式选择的文件或目录。目录递归深度和总文件数均有限制。
pub fn collect_local_inputs(roots: &[PathBuf], staging_dir: &Path) -> Result<LocalCollection> {
    if roots.is_empty() {
        bail!("请至少选择一个文件或文件夹");
    }

    fs::create_dir_all(staging_dir).context("无法创建本地输入暂存目录")?;

    let mut candidates = Vec::new();
    let mut skipped = 0;
    for root in roots {
        discover_path(root, 0, &mut candidates, &mut skipped)?;
        if candidates.len() > MAX_INPUT_FILES {
            bail!("选择的文件过多，单次最多处理 {MAX_INPUT_FILES} 个文件");
        }
    }
    candidates.sort();

    let mut result = LocalCollection {
        skipped,
        ..LocalCollection::default()
    };
    let mut hashes = HashSet::new();

    for candidate in candidates {
        let extension = extension_of(&candidate);
        if is_parseable_extension(&extension) {
            collect_plain_file(&candidate, staging_dir, &mut hashes, &mut result)?;
        } else {
            result.skipped += 1;
        }
    }

    Ok(result)
}

fn discover_path(
    path: &Path,
    depth: usize,
    candidates: &mut Vec<PathBuf>,
    skipped: &mut usize,
) -> Result<()> {
    if depth > MAX_RECURSION_DEPTH {
        *skipped += 1;
        return Ok(());
    }

    let metadata = fs::symlink_metadata(path).context("无法读取所选路径")?;
    if is_reparse_point(&metadata) {
        *skipped += 1;
        return Ok(());
    }
    if metadata.is_file() {
        candidates.push(path.to_path_buf());
        return Ok(());
    }
    if !metadata.is_dir() {
        *skipped += 1;
        return Ok(());
    }

    let entries = fs::read_dir(path).context("无法读取所选文件夹")?;
    for entry in entries {
        match entry {
            Ok(entry) => discover_path(&entry.path(), depth + 1, candidates, skipped)?,
            Err(_) => *skipped += 1,
        }
        if candidates.len() > MAX_INPUT_FILES {
            break;
        }
    }
    Ok(())
}

fn collect_plain_file(
    path: &Path,
    staging_dir: &Path,
    hashes: &mut HashSet<String>,
    result: &mut LocalCollection,
) -> Result<()> {
    let metadata = fs::metadata(path).context("无法读取本地文件属性")?;
    if metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
        result.skipped += 1;
        return Ok(());
    }

    let bytes = fs::read(path).context("无法读取本地文件")?;
    let hash = dedupe::sha256_hex(&bytes);
    if !hashes.insert(hash.clone()) {
        result.duplicates += 1;
        return Ok(());
    }
    reserve_staged_input(result, bytes.len() as u64)?;
    let staged_prefix_chars = path_character_count(staging_dir)
        .saturating_add(1)
        .saturating_add(hash.chars().count())
        .saturating_add(1);
    let filename_budget = MAX_STAGED_PATH_CHARS.saturating_sub(staged_prefix_chars);
    if filename_budget < 12 {
        bail!("本地输入暂存目录路径过长；请缩短数据目录后重试");
    }
    let safe_name = sanitize_filename_with_budget(
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("invoice"),
        filename_budget.min(MAX_STAGED_SOURCE_NAME_CHARS),
    );
    let staged_path = staging_dir.join(format!("{hash}-{safe_name}"));
    if path_character_count(&staged_path) > MAX_STAGED_PATH_CHARS {
        bail!("本地输入暂存文件路径过长；请缩短数据目录后重试");
    }
    if staged_path.exists() {
        if fs::read(&staged_path).context("无法校验已暂存的本地发票")? != bytes {
            bail!("本地输入暂存文件哈希冲突");
        }
    } else {
        fs::write(&staged_path, &bytes).context("无法暂存本地发票")?;
    }
    result.files.push(staged_path);
    Ok(())
}

pub(crate) fn checked_staged_total(
    file_count: usize,
    current_total: u64,
    bytes: u64,
) -> Result<u64> {
    if file_count >= MAX_INPUT_FILES {
        bail!("解析后的发票文件过多，单次最多处理 {MAX_INPUT_FILES} 个文件");
    }
    let next_total = current_total
        .checked_add(bytes)
        .context("本地输入总大小溢出")?;
    if next_total > MAX_TOTAL_STAGED_BYTES {
        bail!("解析后的发票总大小超过 500 MiB；请拆分为多个批次后重试");
    }
    Ok(next_total)
}

fn reserve_staged_input(result: &mut LocalCollection, bytes: u64) -> Result<()> {
    result.total_bytes = checked_staged_total(result.files.len(), result.total_bytes, bytes)?;
    Ok(())
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

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn is_parseable_extension(extension: &str) -> bool {
    matches!(
        extension,
        "xml" | "ofd" | "pdf" | "png" | "jpg" | "jpeg" | "webp" | "bmp"
    )
}

fn sanitize_filename_with_budget(name: &str, max_chars: usize) -> String {
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
    let stem_budget = max_chars.saturating_sub(suffix.chars().count()).max(1);
    let basename = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let basename = basename
        .get(65..)
        .filter(|_| {
            basename.as_bytes().get(64) == Some(&b'-')
                && basename[..64]
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
        })
        .unwrap_or(basename);
    let sanitized: String = basename
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
    if sanitized.is_empty() {
        format!("x{suffix}")
    } else {
        format!("{sanitized}{suffix}")
    }
}

fn path_character_count(path: &Path) -> usize {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().count()
    }
    #[cfg(not(target_os = "windows"))]
    {
        path.as_os_str().to_string_lossy().chars().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("invoice-assistant-{label}-{nonce}"))
    }

    #[test]
    fn directory_collection_is_recursive_deduplicated_and_filtered() {
        let root = test_dir("local-source");
        let nested = root.join("子目录");
        let staging = root.join("staging");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("one.xml"), b"<invoice>one</invoice>").unwrap();
        fs::write(nested.join("duplicate.xml"), b"<invoice>one</invoice>").unwrap();
        fs::write(nested.join("two.pdf"), b"%PDF-1.4 synthetic").unwrap();
        fs::write(root.join("note.txt"), b"not an invoice").unwrap();

        let collected = collect_local_inputs(std::slice::from_ref(&root), &staging).unwrap();

        assert_eq!(collected.files.len(), 2);
        assert_eq!(collected.duplicates, 1);
        assert_eq!(collected.skipped, 1);
        assert!(collected
            .files
            .iter()
            .all(|path| path.starts_with(&staging)));
        assert!(collected.files.iter().all(|path| path.is_file()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn long_multilingual_staging_name_preserves_extension() {
        let root = test_dir("long-source-name");
        let staging = root.join("staging");
        fs::create_dir_all(&root).unwrap();
        let long_name = format!("{}.pdf", "超长发票附件名称".repeat(30));
        let source = root.join(long_name);
        fs::write(&source, b"%PDF-1.7 synthetic").unwrap();

        let collected = collect_local_inputs(&[source], &staging).unwrap();

        assert_eq!(collected.files.len(), 1);
        assert_eq!(
            collected.files[0]
                .extension()
                .and_then(|value| value.to_str()),
            Some("pdf")
        );
        assert!(
            collected.files[0]
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap()
                .chars()
                .count()
                <= 64 + 1 + MAX_STAGED_SOURCE_NAME_CHARS
        );
        assert!(path_character_count(&collected.files[0]) <= MAX_STAGED_PATH_CHARS);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deep_staging_path_is_shortened_for_windows_native_parsers() {
        let root = test_dir("deep-staging");
        let staging = root
            .join("a".repeat(30))
            .join("b".repeat(30))
            .join("staging");
        fs::create_dir_all(&root).unwrap();
        let source = root.join(format!("{}.pdf", "long-source-".repeat(20)));
        fs::write(&source, b"%PDF-1.7 synthetic").unwrap();

        let collected = collect_local_inputs(&[source], &staging).unwrap();

        assert_eq!(collected.files.len(), 1);
        assert_eq!(
            collected.files[0].extension().and_then(|v| v.to_str()),
            Some("pdf")
        );
        assert!(path_character_count(&collected.files[0]) <= MAX_STAGED_PATH_CHARS);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn eml_is_not_a_user_facing_local_import_format() {
        let root = test_dir("eml-source");
        let staging = root.join("staging");
        fs::create_dir_all(&root).unwrap();
        let eml = concat!(
            "From: sender@example.test\r\n",
            "To: receiver@example.test\r\n",
            "Subject: synthetic\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"B\"\r\n\r\n",
            "--B\r\n",
            "Content-Type: application/xml\r\n",
            "Content-Disposition: attachment; filename=\"invoice.xml\"\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\n",
            "PEludm9pY2U+c3ludGhldGljPC9JbnZvaWNlPg==\r\n",
            "--B--\r\n"
        )
        .as_bytes();
        let eml_path = root.join("message.eml");
        fs::write(&eml_path, eml).unwrap();

        let collected = collect_local_inputs(&[eml_path], &staging).unwrap();

        assert!(collected.files.is_empty());
        assert_eq!(collected.skipped, 1);
        assert_eq!(collected.link_only_emails, 0);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn staged_input_limits_cover_expanded_email_attachments() {
        let mut too_many = LocalCollection {
            files: vec![PathBuf::from("synthetic.xml"); MAX_INPUT_FILES],
            ..LocalCollection::default()
        };
        let count_error = reserve_staged_input(&mut too_many, 1).unwrap_err();
        assert!(count_error.to_string().contains("最多处理"));

        let mut too_large = LocalCollection {
            total_bytes: MAX_TOTAL_STAGED_BYTES,
            ..LocalCollection::default()
        };
        let size_error = reserve_staged_input(&mut too_large, 1).unwrap_err();
        assert!(size_error.to_string().contains("500 MiB"));
    }

    #[test]
    fn empty_selection_is_rejected() {
        let staging = test_dir("empty-source");
        let error = collect_local_inputs(&[], &staging).unwrap_err();
        assert!(error.to_string().contains("至少选择"));
    }
}
