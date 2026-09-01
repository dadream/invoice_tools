//! Windows process-wide DLL search hardening.

#[cfg(windows)]
pub fn harden_dll_search() -> anyhow::Result<()> {
    use windows_sys::Win32::System::LibraryLoader::{
        SetDefaultDllDirectories, SetDllDirectoryW, LOAD_LIBRARY_SEARCH_SYSTEM32,
        LOAD_LIBRARY_SEARCH_USER_DIRS,
    };

    let flags = LOAD_LIBRARY_SEARCH_USER_DIRS | LOAD_LIBRARY_SEARCH_SYSTEM32;
    // SAFETY: both functions are process-wide loader configuration calls. `empty` is a
    // valid NUL-terminated empty UTF-16 string, which removes the current directory.
    unsafe {
        if SetDefaultDllDirectories(flags) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let empty = [0_u16];
        if SetDllDirectoryW(empty.as_ptr()) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn harden_dll_search() -> anyhow::Result<()> {
    Ok(())
}
