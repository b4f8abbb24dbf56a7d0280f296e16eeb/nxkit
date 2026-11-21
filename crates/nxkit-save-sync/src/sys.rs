//! The only module with `unsafe`: the system functions of the save layout and of the packed file.
//!
//! `renamex_np` with `RENAME_SWAP` and `clonefile` are the ones `save_sync.c` and the fs service use. The guest name is
//! the attribute `org.d2rmobile.guest-name`, part of a save. `localtime_r` names the backups by the local time, and
//! `sysctlbyname` gives the model of the Mac for the packed file. Other platforms get functions that fail or give
//! nothing, so the crate builds and its pure tests run there.

#![allow(unsafe_code)]

use std::io;
use std::path::Path;

/// The largest guest name that is read, as the buffer of `save_sync.c`. A longer one counts as none.
pub(crate) const GUEST_MAX: usize = 1024;

#[cfg(target_vendor = "apple")]
mod apple {
    use std::ffi::{c_char, c_int, c_long, c_void};

    pub(super) const RENAME_SWAP: u32 = 2;
    pub(super) const XATTR_NOFOLLOW: c_int = 1;

    /// `struct tm` of `<time.h>` on macOS.
    #[repr(C)]
    #[derive(Default)]
    pub(super) struct Tm {
        pub(super) sec: c_int,
        pub(super) min: c_int,
        pub(super) hour: c_int,
        pub(super) mday: c_int,
        pub(super) mon: c_int,
        pub(super) year: c_int,
        pub(super) wday: c_int,
        pub(super) yday: c_int,
        pub(super) isdst: c_int,
        pub(super) gmtoff: c_long,
        pub(super) zone: usize,
    }

    // SAFETY (declarations): these are functions of libSystem with the signatures of `<sys/clonefile.h>`, `<stdio.h>`
    // (`renamex_np`), `<sys/xattr.h>`, `<time.h>` and `<sys/sysctl.h>`.
    unsafe extern "C" {
        pub(super) fn clonefile(src: *const c_char, dst: *const c_char, flags: u32) -> c_int;
        pub(super) fn renamex_np(from: *const c_char, to: *const c_char, flags: u32) -> c_int;
        pub(super) fn getxattr(
            path: *const c_char,
            name: *const c_char,
            value: *mut c_void,
            size: usize,
            position: u32,
            options: c_int,
        ) -> isize;
        pub(super) fn setxattr(
            path: *const c_char,
            name: *const c_char,
            value: *const c_void,
            size: usize,
            position: u32,
            options: c_int,
        ) -> c_int;
        pub(super) fn localtime_r(time: *const i64, out: *mut Tm) -> *mut Tm;
        pub(super) fn sysctlbyname(
            name: *const c_char,
            old: *mut c_void,
            oldlen: *mut usize,
            new: *const c_void,
            newlen: usize,
        ) -> c_int;
    }
}

#[cfg(target_vendor = "apple")]
const GUEST_NAME_ATTR: &std::ffi::CStr = c"org.d2rmobile.guest-name";

#[cfg(target_vendor = "apple")]
fn cpath(p: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(target_vendor = "apple")]
fn check(rc: std::ffi::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Swaps two entries in one step (`renamex_np` with `RENAME_SWAP`).
#[cfg(target_vendor = "apple")]
pub(crate) fn swap(a: &Path, b: &Path) -> io::Result<()> {
    let (x, y) = (cpath(a)?, cpath(b)?);
    // SAFETY: both pointers are NUL-terminated strings that live for the call.
    check(unsafe { apple::renamex_np(x.as_ptr(), y.as_ptr(), apple::RENAME_SWAP) })
}

/// Clones the folder `from` with all it holds to the new folder `to` (APFS `clonefile`).
#[cfg(target_vendor = "apple")]
pub(crate) fn clone_dir(from: &Path, to: &Path) -> io::Result<()> {
    let (a, b) = (cpath(from)?, cpath(to)?);
    // SAFETY: both pointers are NUL-terminated strings that live for the call.
    check(unsafe { apple::clonefile(a.as_ptr(), b.as_ptr(), 0) })
}

/// The guest name of `p`, empty when it has none or one longer than [`GUEST_MAX`], as `save_sync.c` reads it.
#[cfg(target_vendor = "apple")]
pub(crate) fn guest_name(p: &Path) -> Vec<u8> {
    let Ok(c) = cpath(p) else { return Vec::new() };
    let mut buf = vec![0u8; GUEST_MAX];
    // SAFETY: the path and the name are NUL-terminated strings, and `buf` has `buf.len()` writable bytes.
    let n = unsafe {
        apple::getxattr(
            c.as_ptr(),
            GUEST_NAME_ATTR.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
            apple::XATTR_NOFOLLOW,
        )
    };
    buf.truncate(usize::try_from(n).unwrap_or(0));
    buf
}

/// Stores `guest` as the guest name of `p`.
#[cfg(target_vendor = "apple")]
pub(crate) fn set_guest_name(p: &Path, guest: &[u8]) -> io::Result<()> {
    let c = cpath(p)?;
    // SAFETY: the path and the name are NUL-terminated strings, and `guest` has `guest.len()` readable bytes.
    check(unsafe {
        apple::setxattr(
            c.as_ptr(),
            GUEST_NAME_ATTR.as_ptr(),
            guest.as_ptr().cast(),
            guest.len(),
            0,
            apple::XATTR_NOFOLLOW,
        )
    })
}

/// The local time of `t` (seconds since 1970) as year, month, day, hour, minute and second.
#[cfg(target_vendor = "apple")]
pub(crate) fn local_time(t: i64) -> Option<[i32; 6]> {
    let mut tm = apple::Tm::default();
    // SAFETY: `t` and `tm` are valid for the call, and `localtime_r` writes only `tm`.
    let r = unsafe { apple::localtime_r(&t, &mut tm) };
    (!r.is_null()).then_some([tm.year + 1900, tm.mon + 1, tm.mday, tm.hour, tm.min, tm.sec])
}

/// The model of the machine, as `hw.model` gives it ("Mac16,1"), or `unknown`.
#[cfg(target_vendor = "apple")]
pub(crate) fn device_name() -> String {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: the name is a NUL-terminated string, and `buf` has `len` writable bytes. No new value is set.
    let rc = unsafe {
        apple::sysctlbyname(
            c"hw.model".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null(),
            0,
        )
    };
    let text = if rc == 0 {
        &buf[..len.min(buf.len())]
    } else {
        &[][..]
    };
    let model = text.split(|b| *b == 0).next().unwrap_or_default();
    if model.is_empty() {
        "unknown".to_owned()
    } else {
        String::from_utf8_lossy(model).into_owned()
    }
}

#[cfg(not(target_vendor = "apple"))]
fn unsupported() -> io::Error {
    io::Error::from(io::ErrorKind::Unsupported)
}

/// The swap. Only Apple platforms have it.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn swap(_a: &Path, _b: &Path) -> io::Result<()> {
    Err(unsupported())
}

/// The folder clone. Only Apple platforms have it.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn clone_dir(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(unsupported())
}

/// No guest names on other platforms.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn guest_name(_p: &Path) -> Vec<u8> {
    Vec::new()
}

/// No guest names on other platforms.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn set_guest_name(_p: &Path, _guest: &[u8]) -> io::Result<()> {
    Err(unsupported())
}

/// No local time on other platforms.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn local_time(_t: i64) -> Option<[i32; 6]> {
    None
}

/// No model on other platforms.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn device_name() -> String {
    "unknown".to_owned()
}
