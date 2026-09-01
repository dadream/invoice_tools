//! 应用目录解析。
//!
//! 程序目录与用户数据目录严格分离。Windows 正式路径位于
//! `%LOCALAPPDATA%\InvoiceAssistant\Data`；测试可以用
//! `INVOICE_ASSISTANT_HOME` 覆盖，但覆盖值也必须是明确路径。

use std::path::PathBuf;

pub const DATA_ROOT_OVERRIDE: &str = "INVOICE_ASSISTANT_HOME";

/// 返回持久数据根目录，不创建目录。
pub fn data_root() -> anyhow::Result<PathBuf> {
    if let Some(override_root) = std::env::var_os(DATA_ROOT_OVERRIDE) {
        let path = PathBuf::from(override_root);
        if path.as_os_str().is_empty() {
            anyhow::bail!("{DATA_ROOT_OVERRIDE} 不能为空");
        }
        return Ok(path);
    }

    let local_data =
        dirs::data_local_dir().ok_or_else(|| anyhow::anyhow!("无法定位本机用户数据目录"))?;
    Ok(local_data.join("InvoiceAssistant").join("Data"))
}

pub fn logs_dir() -> anyhow::Result<PathBuf> {
    Ok(data_root()?.join("logs"))
}

pub fn temp_dir() -> anyhow::Result<PathBuf> {
    Ok(data_root()?.join("temp"))
}

/// 返回只读 OCR 运行时与模型目录，不创建目录。
///
/// 免安装包把 ocr/ 放在 EXE 同级；开发/测试构建回落到仓库中的
/// src-tauri/assets/ocr/。用户数据目录永不参与模型或 DLL 加载。
pub fn ocr_assets_dir() -> anyhow::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let executable_dir = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("无法定位程序目录"))?;
    let portable = executable_dir.join("ocr");
    if portable.is_dir() {
        return Ok(portable);
    }

    #[cfg(any(debug_assertions, test))]
    {
        let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("ocr");
        if development.is_dir() {
            return Ok(development);
        }
    }

    Ok(portable)
}

#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_controls_all_subdirectories() {
        let _guard = test_env_lock();
        let root = std::env::temp_dir().join("invoice-assistant-path-test");
        std::env::set_var(DATA_ROOT_OVERRIDE, &root);

        assert_eq!(data_root().unwrap(), root);
        assert_eq!(logs_dir().unwrap(), root.join("logs"));
        assert_eq!(temp_dir().unwrap(), root.join("temp"));

        std::env::remove_var(DATA_ROOT_OVERRIDE);
    }

    #[test]
    fn default_is_not_the_program_directory() {
        let _guard = test_env_lock();
        std::env::remove_var(DATA_ROOT_OVERRIDE);
        let root = data_root().unwrap();
        assert!(root.ends_with(PathBuf::from("InvoiceAssistant").join("Data")));
        assert_ne!(root, std::env::current_dir().unwrap());
    }
}
