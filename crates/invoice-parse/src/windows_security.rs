//! Windows DLL search hardening for the isolated OCR worker.

use std::path::Path;

pub struct DllDirectoryCookie {
    #[cfg(windows)]
    cookie: usize,
}

#[cfg(windows)]
impl Drop for DllDirectoryCookie {
    fn drop(&mut self) {
        use windows::Win32::System::LibraryLoader::RemoveDllDirectory;

        // SAFETY: `cookie` was returned by AddDllDirectory and is removed exactly once.
        let _ = unsafe { RemoveDllDirectory(self.cookie as *const core::ffi::c_void) };
    }
}

#[cfg(windows)]
pub fn harden_process_dll_search() -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::LibraryLoader::{
        SetDefaultDllDirectories, SetDllDirectoryW, LOAD_LIBRARY_SEARCH_SYSTEM32,
        LOAD_LIBRARY_SEARCH_USER_DIRS,
    };

    let flags = LOAD_LIBRARY_SEARCH_USER_DIRS | LOAD_LIBRARY_SEARCH_SYSTEM32;
    // SAFETY: both calls only configure the current process. `empty` is a valid
    // NUL-terminated empty UTF-16 string and removes the current directory.
    unsafe {
        SetDefaultDllDirectories(flags).map_err(|error| error.to_string())?;
        let empty = [0_u16];
        SetDllDirectoryW(PCWSTR(empty.as_ptr())).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn harden_process_dll_search() -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub fn add_verified_dll_directory(path: &Path) -> Result<DllDirectoryCookie, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::LibraryLoader::AddDllDirectory;

    harden_process_dll_search()?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and remains alive for the duration of the call.
    let cookie = unsafe { AddDllDirectory(PCWSTR(wide.as_ptr())) };
    if cookie.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(DllDirectoryCookie {
        cookie: cookie as usize,
    })
}

#[cfg(not(windows))]
pub fn add_verified_dll_directory(_path: &Path) -> Result<DllDirectoryCookie, String> {
    Ok(DllDirectoryCookie {})
}
