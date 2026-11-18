//! Publish a complete file on the same volume without replacing an existing entry.

use std::{io, path::Path};

/// Publish `from` at `to` atomically. A preexisting destination is always preserved.
#[cfg(target_os = "macos")]
pub fn file_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::{CString, c_char, c_int, c_uint};
    use std::os::unix::ffi::OsStrExt;

    // <sys/stdio.h>
    const RENAME_EXCL: c_uint = 0x00000004;
    unsafe extern "C" {
        fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
    }
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: Both paths remain valid NUL-terminated strings throughout the call.
    if unsafe { renamex_np(from.as_ptr(), to.as_ptr(), RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Publish a complete file on platforms without `renamex_np`. Its staging directory owns the old link.
#[cfg(not(target_os = "macos"))]
pub fn file_new(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::hard_link(from, to)
}
