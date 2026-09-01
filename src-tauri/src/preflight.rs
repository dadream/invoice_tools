//! Windows 启动前检查。此模块不得依赖 WebView，失败必须能用原生对话框说明。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, Prefix};

const MIN_FREE_BYTES: u64 = 200 * 1024 * 1024;
const WEBVIEW2_CLIENT_ID: &str = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    pub webview2_version: String,
    pub data_root: std::path::PathBuf,
    pub free_bytes: u64,
}

pub fn run() -> anyhow::Result<PreflightReport> {
    anyhow::ensure!(
        cfg!(all(target_os = "windows", target_arch = "x86_64")),
        "本版本仅支持 Windows x64；请使用 Windows 11 x64 设备"
    );

    let webview2_version = webview2_version().ok_or_else(|| {
        anyhow::anyhow!(
            "未检测到 Microsoft Edge WebView2 Evergreen Runtime。请联系企业 IT，或从微软官方 WebView2 下载页安装后重试"
        )
    })?;

    let data_root = crate::paths::data_root()?;
    validate_data_root(&data_root)?;
    fs::create_dir_all(&data_root)?;
    ensure_directory_is_writable(&data_root)?;

    let free_bytes = available_space(&data_root)?;
    anyhow::ensure!(
        free_bytes >= MIN_FREE_BYTES,
        "数据目录可用空间不足 200 MiB；请释放磁盘空间或联系 IT 后重试"
    );
    crate::backup::apply_pending_import(&data_root)?;

    Ok(PreflightReport {
        webview2_version,
        data_root,
        free_bytes,
    })
}

fn validate_data_root(path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(path.is_absolute(), "数据目录必须是本机绝对路径");
    anyhow::ensure!(
        !matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _))),
        "数据目录不能位于 UNC 或网络共享"
    );

    let normalized = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    for marker in ["\\onedrive\\", "\\dropbox\\", "\\google drive\\"] {
        anyhow::ensure!(
            !normalized.contains(marker),
            "数据目录不能位于已知同步盘；请使用本机 LocalAppData"
        );
    }

    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        anyhow::ensure!(metadata.is_dir(), "数据目录路径不是文件夹");
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "数据目录不能是符号链接或联接点"
        );
    }
    Ok(())
}

fn ensure_directory_is_writable(path: &Path) -> anyhow::Result<()> {
    let probe = path.join(format!(".preflight-write-{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)?;
        file.write_all(b"invoice-assistant-preflight")?;
        file.sync_all()?;
        drop(file);
        fs::remove_file(&probe)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&probe);
    }
    result.map_err(|_| anyhow::anyhow!("数据目录不可写；请检查企业策略或文件夹权限"))
}

#[cfg(target_os = "windows")]
fn available_space(path: &Path) -> anyhow::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0u64;
    // SAFETY: `wide` is a valid nul-terminated UTF-16 path and `available` is writable.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    anyhow::ensure!(ok != 0, "无法读取数据目录所在磁盘的可用空间");
    Ok(available)
}

#[cfg(not(target_os = "windows"))]
fn available_space(_path: &Path) -> anyhow::Result<u64> {
    Ok(u64::MAX)
}

#[cfg(target_os = "windows")]
fn webview2_version() -> Option<String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };

    let machine =
        format!("SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\{WEBVIEW2_CLIENT_ID}");
    let user = format!("Software\\Microsoft\\EdgeUpdate\\Clients\\{WEBVIEW2_CLIENT_ID}");
    for (hive, subkey) in [(HKEY_LOCAL_MACHINE, machine), (HKEY_CURRENT_USER, user)] {
        let subkey: Vec<u16> = std::ffi::OsStr::new(&subkey)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let value: Vec<u16> = std::ffi::OsStr::new("pv")
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut buffer = [0u16; 128];
        let mut bytes = std::mem::size_of_val(&buffer) as u32;
        // SAFETY: registry handles are predefined; all pointers target valid buffers.
        let status = unsafe {
            RegGetValueW(
                hive,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast::<c_void>(),
                &mut bytes,
            )
        };
        if status == ERROR_SUCCESS {
            let end = buffer
                .iter()
                .position(|ch| *ch == 0)
                .unwrap_or(buffer.len());
            let version = String::from_utf16_lossy(&buffer[..end]);
            if !version.is_empty() && version != "0.0.0.0" {
                return Some(version);
            }
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
fn webview2_version() -> Option<String> {
    Some("not-applicable".to_string())
}

#[cfg(target_os = "windows")]
pub fn show_fatal_error(title: &str, message: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    let title: Vec<u16> = std::ffi::OsStr::new(title)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let message: Vec<u16> = std::ffi::OsStr::new(message)
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: strings are valid nul-terminated UTF-16 and no owner window is required.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(target_os = "windows"))]
pub fn show_fatal_error(title: &str, message: &str) {
    eprintln!("{title}: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_data_root() {
        assert!(validate_data_root(Path::new("relative-data")).is_err());
    }

    #[test]
    fn rejects_unc_data_root() {
        assert!(validate_data_root(Path::new(r"\\server\share\InvoiceAssistant")).is_err());
    }

    #[test]
    fn rejects_known_sync_folder() {
        assert!(
            validate_data_root(Path::new(r"C:\Users\tester\OneDrive\InvoiceAssistant")).is_err()
        );
    }
}
