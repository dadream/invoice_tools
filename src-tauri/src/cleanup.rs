//! 免安装版的受控数据/程序清除。
//!
//! 主进程只生成无通配符计划并复制自身到系统临时目录。临时副本等待主进程
//! 退出释放 SQLite/EXE 句柄后执行计划；任何未列入计划的文件都不会删除。

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf, Prefix};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

const PLAN_VERSION: u32 = 1;
const CONFIRMATION_PHRASE: &str = "永久清除";
const MAX_ITEMS: usize = 100_000;
const MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupCategory {
    pub name: String,
    pub file_count: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPreview {
    pub program_directory: String,
    pub data_directory: String,
    pub program_cleanup_available: bool,
    pub program_file_count: usize,
    pub data_file_count: usize,
    pub total_bytes: u64,
    pub confirmation_phrase: String,
    pub categories: Vec<CleanupCategory>,
    pub warning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CleanupPlan {
    version: u32,
    data_root: PathBuf,
    program_root: Option<PathBuf>,
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

#[derive(Default)]
struct CollectedTargets {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PortableManifest {
    product: String,
    files: Vec<PortableManifestFile>,
}

#[derive(Debug, Deserialize)]
struct PortableManifestFile {
    path: String,
}

#[tauri::command]
pub fn preview_cleanup() -> AppResult<CleanupPreview> {
    let data_root = crate::paths::data_root().map_err(AppError::from)?;
    let current_exe = std::env::current_exe()?;
    let program_root = current_exe
        .parent()
        .ok_or_else(|| AppError::internal("无法定位程序目录"))?
        .to_path_buf();

    let data = collect_data_targets(&data_root)?;
    let program = collect_portable_program_targets(&program_root, &current_exe).ok();
    let program_bytes = program.as_ref().map_or(0, |items| items.bytes);
    let program_count = program.as_ref().map_or(0, |items| items.files.len());

    let categories = vec![
        category_for(&data_root.join("ledger.db"), "发票台账"),
        category_for(&data_root.join("accounts.db"), "本机邮箱配置（不含授权码）"),
        category_for(&data_root.join("files"), "应用保存的发票原件"),
        category_for(&data_root.join("collection-files"), "邮件收集材料库"),
        category_for(&data_root.join("logs"), "本机诊断日志"),
        category_for(&data_root.join("temp"), "临时缓存"),
    ];

    Ok(CleanupPreview {
        program_directory: program_root.display().to_string(),
        data_directory: data_root.display().to_string(),
        program_cleanup_available: program.is_some(),
        program_file_count: program_count,
        data_file_count: data.files.len(),
        total_bytes: data.bytes.saturating_add(program_bytes),
        confirmation_phrase: CONFIRMATION_PHRASE.to_string(),
        categories,
        warning: "清除不可恢复。建议先导出备份；程序目录中的非产品文件会保留。".to_string(),
    })
}

#[tauri::command]
pub fn start_cleanup(
    app: AppHandle,
    include_program: bool,
    include_data: bool,
    confirmation: String,
) -> AppResult<()> {
    if confirmation.trim() != CONFIRMATION_PHRASE {
        return Err(AppError::validation(format!(
            "请输入确认短语“{CONFIRMATION_PHRASE}”"
        )));
    }
    if !include_program && !include_data {
        return Err(AppError::validation("至少选择清除程序或本机数据"));
    }

    let data_root = crate::paths::data_root().map_err(AppError::from)?;
    let current_exe = std::env::current_exe()?;
    let program_root = current_exe
        .parent()
        .ok_or_else(|| AppError::internal("无法定位程序目录"))?
        .to_path_buf();
    let mut plan = CleanupPlan {
        version: PLAN_VERSION,
        data_root: data_root.clone(),
        program_root: None,
        files: Vec::new(),
        directories: Vec::new(),
    };

    if include_data {
        let data = collect_data_targets(&data_root)?;
        plan.files.extend(data.files);
        plan.directories.extend(data.directories);
    }
    if include_program {
        let program = collect_portable_program_targets(&program_root, &current_exe).map_err(|_| {
            AppError::validation(
                "当前不是带有效 manifest.json 的标准 portable 包；请手动删除程序文件，数据仍可单独清除",
            )
        })?;
        plan.program_root = Some(program_root);
        plan.files.extend(program.files);
        plan.directories.extend(program.directories);
    }
    validate_plan(&plan)?;

    let system_temp = std::env::temp_dir();
    let nonce = Uuid::new_v4();
    let helper = system_temp.join(format!("InvoiceAssistant-cleanup-{nonce}.exe"));
    let plan_path = system_temp.join(format!("InvoiceAssistant-cleanup-{nonce}.json"));
    fs::copy(&current_exe, &helper)?;
    let plan_bytes = serde_json::to_vec(&plan)
        .map_err(|e| AppError::internal(format!("生成清除计划失败: {e}")))?;
    fs::write(&plan_path, plan_bytes)?;

    let spawn_result = Command::new(&helper)
        .arg("--invoice-assistant-cleanup")
        .arg(&plan_path)
        .spawn();
    if let Err(error) = spawn_result {
        let _ = fs::remove_file(&helper);
        let _ = fs::remove_file(&plan_path);
        return Err(AppError::io(format!("无法启动临时清理程序: {error}")));
    }

    app.exit(0);
    Ok(())
}

/// 在任何 WebView、日志和数据库初始化前识别临时清理器模式。
pub fn run_helper_if_requested() -> bool {
    let mut args = std::env::args_os();
    let _exe = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--invoice-assistant-cleanup")) {
        return false;
    }
    let Some(plan_path) = args.next().map(PathBuf::from) else {
        show_cleanup_result(
            "清除未执行",
            "清除计划参数缺失。没有删除任何产品数据。",
            true,
        );
        return true;
    };
    if let Err(error) = run_helper(&plan_path) {
        show_cleanup_result(
            "清除未完成",
            &format!("部分或全部文件未能清除。未列入产品计划的文件未受影响。\n\n{error}"),
            true,
        );
    }
    true
}

fn run_helper(plan_path: &Path) -> anyhow::Result<()> {
    let bytes = fs::read(plan_path)?;
    anyhow::ensure!(bytes.len() <= 16 * 1024 * 1024, "清除计划超过大小上限");
    let plan: CleanupPlan = serde_json::from_slice(&bytes)?;
    validate_plan(&plan).map_err(|e| anyhow::anyhow!(e.message().to_string()))?;

    // 给主进程时间退出并释放 SQLite/EXE 句柄。
    std::thread::sleep(Duration::from_millis(1200));
    let result = execute_cleanup_plan(&plan);
    let _ = fs::remove_file(plan_path);
    result?;
    show_cleanup_result(
        "清除完成",
        "已清除所选产品文件和数据。程序目录中的非产品文件已保留。",
        false,
    );
    Ok(())
}

/// Execute an already validated plan. Kept separate so destructive behavior can
/// be regression-tested entirely inside disposable directories.
fn execute_cleanup_plan(plan: &CleanupPlan) -> anyhow::Result<()> {
    validate_plan(plan).map_err(|e| anyhow::anyhow!(e.message().to_string()))?;
    let mut pending = plan.files.clone();
    for _ in 0..120 {
        pending.retain(|path| {
            if !path.exists() {
                return false;
            }
            if validate_entry_now(path, plan).is_err() {
                return true;
            }
            fs::remove_file(path).is_err()
        });
        if pending.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    for directory in &plan.directories {
        if directory.exists() && validate_entry_now(directory, plan).is_ok() {
            let _ = fs::remove_dir(directory);
        }
    }

    if pending.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("仍有 {} 个产品文件被占用或路径校验失败", pending.len())
    }
}

fn collect_data_targets(root: &Path) -> AppResult<CollectedTargets> {
    validate_safe_root(root)?;
    let mut targets = CollectedTargets::default();
    if root.exists() {
        collect_tree(root, 0, &mut targets)?;
        targets.directories.push(root.to_path_buf());
        sort_directories_deepest_first(&mut targets.directories);
    }
    Ok(targets)
}

fn collect_tree(root: &Path, depth: usize, targets: &mut CollectedTargets) -> AppResult<()> {
    if depth > MAX_DEPTH {
        return Err(AppError::validation("数据目录层级超过清除安全上限"));
    }
    for entry in fs::read_dir(root)? {
        if targets.files.len() + targets.directories.len() >= MAX_ITEMS {
            return Err(AppError::validation("待清除项目数量超过安全上限"));
        }
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if is_reparse_or_symlink(&metadata) {
            return Err(AppError::validation(format!(
                "数据目录包含符号链接或联接点，已拒绝清除: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            collect_tree(&path, depth + 1, targets)?;
            targets.directories.push(path);
        } else if metadata.is_file() {
            targets.bytes = targets.bytes.saturating_add(metadata.len());
            targets.files.push(path);
        }
    }
    Ok(())
}

fn collect_portable_program_targets(
    root: &Path,
    current_exe: &Path,
) -> AppResult<CollectedTargets> {
    validate_safe_root(root)?;
    if current_exe.file_name().and_then(|v| v.to_str()) != Some("InvoiceAssistant.exe") {
        return Err(AppError::validation("当前可执行文件不是标准 portable 名称"));
    }
    let manifest_path = root.join("manifest.json");
    let manifest_bytes = fs::read(&manifest_path)?;
    if manifest_bytes.len() > 4 * 1024 * 1024 {
        return Err(AppError::validation("portable manifest 超过大小上限"));
    }
    let manifest: PortableManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| AppError::validation(format!("portable manifest 无效: {e}")))?;
    if manifest.product != "发票报销助手" {
        return Err(AppError::validation("portable manifest 产品标识不匹配"));
    }

    let mut names = HashSet::new();
    for item in manifest.files {
        let candidate = Path::new(&item.path);
        if candidate.components().count() != 1
            || !matches!(candidate.components().next(), Some(Component::Normal(_)))
        {
            return Err(AppError::validation("portable manifest 包含不安全路径"));
        }
        names.insert(item.path);
    }
    names.insert("manifest.json".to_string());
    names.insert("SHA256SUMS.txt".to_string());
    if !names.contains("InvoiceAssistant.exe") {
        return Err(AppError::validation("portable manifest 缺少主程序"));
    }

    let mut targets = CollectedTargets::default();
    for name in names {
        let path = root.join(name);
        if !path.exists() {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || is_reparse_or_symlink(&metadata) {
            return Err(AppError::validation("portable 产品文件类型不安全"));
        }
        targets.bytes = targets.bytes.saturating_add(metadata.len());
        targets.files.push(path);
    }
    targets.directories.push(root.to_path_buf());
    Ok(targets)
}

fn validate_plan(plan: &CleanupPlan) -> AppResult<()> {
    if plan.version != PLAN_VERSION {
        return Err(AppError::validation("清除计划版本不兼容"));
    }
    validate_safe_root(&plan.data_root)?;
    let expected_data_root = crate::paths::data_root().map_err(AppError::from)?;
    if plan.data_root != expected_data_root {
        return Err(AppError::validation("清除计划数据目录与本机产品目录不匹配"));
    }
    if let Some(program_root) = &plan.program_root {
        validate_safe_root(program_root)?;
        let original_exe = program_root.join("InvoiceAssistant.exe");
        let owned = collect_portable_program_targets(program_root, &original_exe)?;
        let owned_files: HashSet<PathBuf> = owned.files.into_iter().collect();
        for path in &plan.files {
            if path.starts_with(program_root) && !owned_files.contains(path) {
                return Err(AppError::validation(
                    "清除计划包含 portable manifest 未声明的程序文件",
                ));
            }
        }
        for directory in &plan.directories {
            if directory.starts_with(program_root) && directory != program_root {
                return Err(AppError::validation(
                    "标准 portable 包不允许清除未声明的子目录",
                ));
            }
        }
    }
    if plan.files.len() + plan.directories.len() > MAX_ITEMS {
        return Err(AppError::validation("清除计划项目数量超过上限"));
    }
    let mut seen = HashSet::new();
    for path in plan.files.iter().chain(plan.directories.iter()) {
        if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(AppError::validation("清除计划包含非绝对或上跳路径"));
        }
        if !path.starts_with(&plan.data_root)
            && !plan
                .program_root
                .as_ref()
                .is_some_and(|root| path.starts_with(root))
        {
            return Err(AppError::validation("清除计划路径超出产品根目录"));
        }
        if !seen.insert(path.clone()) {
            return Err(AppError::validation("清除计划包含重复路径"));
        }
    }
    Ok(())
}

fn validate_entry_now(path: &Path, plan: &CleanupPlan) -> AppResult<()> {
    if !path.starts_with(&plan.data_root)
        && !plan
            .program_root
            .as_ref()
            .is_some_and(|root| path.starts_with(root))
    {
        return Err(AppError::validation("清除目标超出产品目录"));
    }
    let metadata = fs::symlink_metadata(path)?;
    if is_reparse_or_symlink(&metadata) {
        return Err(AppError::validation("清除目标变成了符号链接或联接点"));
    }
    Ok(())
}

fn validate_safe_root(path: &Path) -> AppResult<()> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(AppError::validation("清除根目录必须是无上跳段的绝对路径"));
    }
    if matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _)))
    {
        return Err(AppError::validation("清除根目录不能位于 UNC 或网络共享"));
    }
    if path.parent().is_none()
        || path
            .parent()
            .is_some_and(|parent| parent.parent().is_none())
    {
        return Err(AppError::validation("拒绝对磁盘根目录或其直接子级执行清除"));
    }
    if let Some(home) = dirs::home_dir() {
        if path == home {
            return Err(AppError::validation("拒绝清除用户主目录"));
        }
    }
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
            return Err(AppError::validation("清除根目录不是安全的普通文件夹"));
        }
    }
    Ok(())
}

fn category_for(path: &Path, name: &str) -> CleanupCategory {
    let mut targets = CollectedTargets::default();
    if path.is_file() {
        if let Ok(metadata) = fs::metadata(path) {
            targets.files.push(path.to_path_buf());
            targets.bytes = metadata.len();
        }
    } else if path.is_dir() {
        let _ = collect_tree(path, 0, &mut targets);
    }
    CleanupCategory {
        name: name.to_string(),
        file_count: targets.files.len(),
        bytes: targets.bytes,
    }
}

fn sort_directories_deepest_first(paths: &mut [PathBuf]) {
    paths.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
}

#[cfg(windows)]
fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn show_cleanup_result(title: &str, message: &str, is_error: bool) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    };

    let title: Vec<u16> = std::ffi::OsStr::new(title)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let message: Vec<u16> = std::ffi::OsStr::new(message)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let icon = if is_error {
        MB_ICONERROR
    } else {
        MB_ICONINFORMATION
    };
    // SAFETY: 字符串为有效的 NUL 结尾 UTF-16，清理器没有可绑定的窗口句柄。
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | icon,
        );
    }
}

#[cfg(not(windows))]
fn show_cleanup_result(_title: &str, _message: &str, _is_error: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_root_rejects_relative_and_disk_root() {
        assert!(validate_safe_root(Path::new("relative")).is_err());
        assert!(validate_safe_root(Path::new("C:\\")).is_err());
    }

    #[test]
    fn data_collection_is_explicit_and_deepest_directory_first() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("InvoiceAssistant").join("Data");
        fs::create_dir_all(root.join("files").join("1")).unwrap();
        fs::write(root.join("ledger.db"), b"db").unwrap();
        fs::write(root.join("files").join("1").join("invoice.xml"), b"xml").unwrap();

        let targets = collect_data_targets(&root).unwrap();
        assert_eq!(targets.files.len(), 2);
        assert_eq!(targets.bytes, 5);
        assert_eq!(targets.directories.last(), Some(&root));
    }

    #[test]
    fn portable_manifest_rejects_path_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("portable").join("release");
        fs::create_dir_all(&root).unwrap();
        let exe = root.join("InvoiceAssistant.exe");
        fs::write(&exe, b"exe").unwrap();
        fs::write(
            root.join("manifest.json"),
            r#"{"product":"发票报销助手","files":[{"path":"../outside.txt"}]}"#.as_bytes(),
        )
        .unwrap();
        assert!(collect_portable_program_targets(&root, &exe).is_err());
    }

    #[test]
    fn cleanup_plan_rejects_program_file_not_declared_by_manifest() {
        let _guard = crate::paths::test_env_lock();
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp
            .path()
            .join("profile")
            .join("InvoiceAssistant")
            .join("Data");
        let program_root = temp.path().join("portable").join("release");
        fs::create_dir_all(&data_root).unwrap();
        fs::create_dir_all(&program_root).unwrap();
        std::env::set_var(crate::paths::DATA_ROOT_OVERRIDE, &data_root);

        let exe = program_root.join("InvoiceAssistant.exe");
        let unrelated = program_root.join("my-notes.txt");
        fs::write(&exe, b"exe").unwrap();
        fs::write(&unrelated, b"keep me").unwrap();
        fs::write(
            program_root.join("manifest.json"),
            r#"{"product":"发票报销助手","files":[{"path":"InvoiceAssistant.exe"}]}"#.as_bytes(),
        )
        .unwrap();

        let plan = CleanupPlan {
            version: PLAN_VERSION,
            data_root: data_root.clone(),
            program_root: Some(program_root.clone()),
            files: vec![exe, unrelated],
            directories: vec![program_root],
        };
        assert!(validate_plan(&plan)
            .unwrap_err()
            .message()
            .contains("未声明"));
        std::env::remove_var(crate::paths::DATA_ROOT_OVERRIDE);
    }

    #[test]
    fn cleanup_execution_removes_only_owned_files_and_preserves_user_file() {
        let _guard = crate::paths::test_env_lock();
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp
            .path()
            .join("profile")
            .join("InvoiceAssistant")
            .join("Data");
        let program_root = temp.path().join("portable").join("release");
        fs::create_dir_all(data_root.join("files")).unwrap();
        fs::create_dir_all(&program_root).unwrap();
        fs::write(data_root.join("ledger.db"), b"ledger").unwrap();
        fs::write(data_root.join("files").join("invoice.xml"), b"invoice").unwrap();

        let exe = program_root.join("InvoiceAssistant.exe");
        let release_notes = program_root.join("RELEASE-NOTES.md");
        let manifest = program_root.join("manifest.json");
        let checksums = program_root.join("SHA256SUMS.txt");
        let user_file = program_root.join("my-notes.txt");
        fs::write(&exe, b"exe").unwrap();
        fs::write(&release_notes, b"notes").unwrap();
        fs::write(&checksums, b"checksums").unwrap();
        fs::write(&user_file, b"keep me").unwrap();
        fs::write(
            &manifest,
            r#"{"product":"发票报销助手","files":[{"path":"InvoiceAssistant.exe"},{"path":"RELEASE-NOTES.md"}]}"#,
        )
        .unwrap();

        std::env::set_var(crate::paths::DATA_ROOT_OVERRIDE, &data_root);
        let data = collect_data_targets(&data_root).unwrap();
        let program = collect_portable_program_targets(&program_root, &exe).unwrap();
        let plan = CleanupPlan {
            version: PLAN_VERSION,
            data_root: data_root.clone(),
            program_root: Some(program_root.clone()),
            files: data.files.into_iter().chain(program.files).collect(),
            directories: data
                .directories
                .into_iter()
                .chain(program.directories)
                .collect(),
        };
        validate_plan(&plan).unwrap();
        execute_cleanup_plan(&plan).unwrap();
        std::env::remove_var(crate::paths::DATA_ROOT_OVERRIDE);

        assert!(!data_root.exists());
        assert!(!exe.exists());
        assert!(!release_notes.exists());
        assert!(!manifest.exists());
        assert!(!checksums.exists());
        assert_eq!(fs::read(&user_file).unwrap(), b"keep me");
        assert!(program_root.is_dir());
    }
}
