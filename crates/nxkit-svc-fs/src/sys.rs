//! The only module with `unsafe`: mapping files and the system calls for cloning a folder, swapping two folders and
//! the guest-name attribute.
//!
//! The clone, the swap and the attribute are the ones `dm/host/hle_fs.c` uses for the save layout (R13):
//! `clonefile`, `renamex_np` with `RENAME_SWAP` and the attribute `org.d2rmobile.guest-name`. Other platforms get
//! functions that fail with "unsupported", so the crate builds and its other tests run there.

#![allow(unsafe_code)]

#[cfg(target_vendor = "apple")]
use std::ffi::CString;
use std::ffi::{c_int, c_void};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
#[cfg(target_vendor = "apple")]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const PROT_READ: c_int = 1;
const MAP_PRIVATE: c_int = 2;

// SAFETY (declarations): `mmap` and `munmap` are POSIX functions with these signatures on macOS and Linux.
unsafe extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
}

/// A file mapped read-only. Reading the file does not copy it. The page cache serves the bytes.
///
/// The file must not shrink while it is mapped, or a read past the new end raises `SIGBUS`. Only files of the romfs
/// and of read-only mounts are mapped, and nothing changes them.
pub(crate) struct Map {
    ptr: *const u8,
    len: usize,
}

// SAFETY: the mapping is read-only and private. Nothing in the process writes through the pointer, so sharing it
// between threads is the same as sharing a `&[u8]`.
unsafe impl Send for Map {}
// SAFETY: see `Send`.
unsafe impl Sync for Map {}

impl Map {
    /// Maps the first `len` bytes of `file`. A length of zero maps nothing and reads as an empty slice.
    pub(crate) fn new(file: &File, len: usize) -> io::Result<Map> {
        if len == 0 {
            return Ok(Map { ptr: std::ptr::null(), len: 0 });
        }
        // SAFETY: a null address lets the system choose, `len` is not zero, the descriptor is open for the call and
        // the offset is zero. The result is checked before use.
        let p = unsafe { mmap(std::ptr::null_mut(), len, PROT_READ, MAP_PRIVATE, file.as_raw_fd(), 0) };
        if p as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Map { ptr: p as *const u8, len })
    }

    /// The mapped bytes.
    pub(crate) fn as_slice(&self) -> &[u8] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `ptr` and `len` came from a successful `mmap` that lives until `drop`, and the mapping is
        // readable.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        if self.len != 0 {
            // SAFETY: the range is the mapping this value made and nothing uses it after `drop`.
            unsafe { munmap(self.ptr as *mut c_void, self.len) };
        }
    }
}

#[cfg(target_vendor = "apple")]
fn cpath(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(target_vendor = "apple")]
fn check(rc: c_int) -> io::Result<()> {
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Whether a host error says that the file system cannot clone or swap folders (HFS+ and exFAT).
pub(crate) fn is_unsupported(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Unsupported || e.raw_os_error() == Some(ENOTSUP)
}

const ENOTSUP: i32 = 45;

#[cfg(target_vendor = "apple")]
mod apple {
    use std::ffi::{c_char, c_int, c_void};

    pub(super) const RENAME_SWAP: u32 = 2;
    pub(super) const XATTR_NOFOLLOW: c_int = 1;
    pub(super) const ENOATTR: i32 = 93;

    // SAFETY (declarations): these are functions of libSystem with the signatures of `<sys/clonefile.h>`,
    // `<stdio.h>` (`renamex_np`) and `<sys/xattr.h>`.
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
        pub(super) fn removexattr(path: *const c_char, name: *const c_char, options: c_int) -> c_int;
    }
}

/// The attribute that holds the name the game gave an entry whose host name had to differ.
#[cfg(target_vendor = "apple")]
const GUEST_NAME_ATTR: &std::ffi::CStr = c"org.d2rmobile.guest-name";

/// Clones the folder `from` with all it holds to the new folder `to`, copy-on-write (APFS `clonefile`).
#[cfg(target_vendor = "apple")]
pub(crate) fn clone_dir(from: &Path, to: &Path) -> io::Result<()> {
    let (a, b) = (cpath(from)?, cpath(to)?);
    // SAFETY: both pointers are NUL-terminated strings that live for the call.
    check(unsafe { apple::clonefile(a.as_ptr(), b.as_ptr(), 0) })
}

/// Swaps two entries in one step (`renamex_np` with `RENAME_SWAP`). A kill leaves the old or the new content.
#[cfg(target_vendor = "apple")]
pub(crate) fn swap(a: &Path, b: &Path) -> io::Result<()> {
    let (x, y) = (cpath(a)?, cpath(b)?);
    // SAFETY: both pointers are NUL-terminated strings that live for the call.
    check(unsafe { apple::renamex_np(x.as_ptr(), y.as_ptr(), apple::RENAME_SWAP) })
}

/// The guest name stored on `host`, or `None` if the entry has none.
#[cfg(target_vendor = "apple")]
pub(crate) fn get_guest_name(host: &Path) -> io::Result<Option<Vec<u8>>> {
    let p = cpath(host)?;
    let mut buf = vec![0u8; 0x301];
    // SAFETY: the path and the name are NUL-terminated strings, and `buf` has `buf.len()` writable bytes.
    let n = unsafe {
        apple::getxattr(
            p.as_ptr(),
            GUEST_NAME_ATTR.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
            apple::XATTR_NOFOLLOW,
        )
    };
    if n >= 0 {
        buf.truncate(n as usize);
        return Ok(Some(buf));
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(apple::ENOATTR) { Ok(None) } else { Err(e) }
}

/// Stores `guest` as the guest name of `host`, or removes the name when `guest` is `None`.
#[cfg(target_vendor = "apple")]
pub(crate) fn set_guest_name(host: &Path, guest: Option<&[u8]>) -> io::Result<()> {
    let p = cpath(host)?;
    match guest {
        // SAFETY: the path and the name are NUL-terminated strings, and `g` has `g.len()` readable bytes.
        Some(g) => check(unsafe {
            apple::setxattr(p.as_ptr(), GUEST_NAME_ATTR.as_ptr(), g.as_ptr().cast(), g.len(), 0, apple::XATTR_NOFOLLOW)
        }),
        None => {
            // SAFETY: the path and the name are NUL-terminated strings that live for the call.
            let rc = unsafe { apple::removexattr(p.as_ptr(), GUEST_NAME_ATTR.as_ptr(), apple::XATTR_NOFOLLOW) };
            match check(rc) {
                Err(e) if e.raw_os_error() == Some(apple::ENOATTR) => Ok(()),
                other => other,
            }
        }
    }
}

/// Folder clone. Only Apple platforms have it.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn clone_dir(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Folder swap. Only Apple platforms have it.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn swap(_a: &Path, _b: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// No entry has a guest name. A case-sensitive file system needs none.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn get_guest_name(_host: &Path) -> io::Result<Option<Vec<u8>>> {
    Ok(None)
}

/// Removing a name always works. Storing one is refused, because only Apple platforms have the attribute.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn set_guest_name(_host: &Path, guest: Option<&[u8]>) -> io::Result<()> {
    if guest.is_some() { Err(io::Error::from(io::ErrorKind::Unsupported)) } else { Ok(()) }
}

/// The entry of a guest function that the host looked up by its mangled name (`set_guest_sym`).
#[derive(Clone, Copy)]
pub(crate) struct GuestFn(pub(crate) u64);

/// Releases `name` from the SDK's own mount table. Returns false when the SDK does not hold the name.
///
/// The SDK's `Unmount` aborts on a name it does not hold, so `IsMounted` runs first. `name` is a guest string. The
/// two functions are the SDK's `bool IsMounted(const char *)` and `void Unmount(const char *)`, as `set_guest_sym`
/// requires of the host.
pub(crate) fn release_sdk_mount(is_mounted: GuestFn, unmount: GuestFn, name: u64) -> bool {
    // SAFETY: `set_guest_sym` makes the host answer with the entries of the SDK functions of these exact mangled
    // names, which have these signatures. The guest is native code in this process, so a guest address is callable.
    unsafe {
        let is_mounted: extern "C" fn(u64) -> u32 = std::mem::transmute::<usize, _>(is_mounted.0 as usize);
        if is_mounted(name) & 0xFF == 0 {
            return false;
        }
        let unmount: extern "C" fn(u64) = std::mem::transmute::<usize, _>(unmount.0 as usize);
        unmount(name);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_mapped_file_reads_as_its_bytes() {
        let dir = crate::testutil::scratch("sys-map");
        let path = dir.join("f");
        File::create(&path).and_then(|mut f| f.write_all(b"hello romfs")).unwrap();
        let f = File::open(&path).unwrap();
        let m = Map::new(&f, 11).unwrap();
        assert_eq!(m.as_slice(), b"hello romfs");
        let empty = Map::new(&f, 0).unwrap();
        assert!(empty.as_slice().is_empty());
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_guest_name_round_trips() {
        let dir = crate::testutil::scratch("sys-name");
        let path = dir.join("f");
        File::create(&path).unwrap();
        assert_eq!(get_guest_name(&path).unwrap(), None);
        set_guest_name(&path, Some(b"Name")).unwrap();
        assert_eq!(get_guest_name(&path).unwrap(), Some(b"Name".to_vec()));
        set_guest_name(&path, None).unwrap();
        assert_eq!(get_guest_name(&path).unwrap(), None);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn clone_and_swap_exchange_two_folders() {
        let dir = crate::testutil::scratch("sys-swap");
        std::fs::create_dir(dir.join("a")).unwrap();
        std::fs::write(dir.join("a").join("x"), b"one").unwrap();
        clone_dir(&dir.join("a"), &dir.join("b")).unwrap();
        std::fs::write(dir.join("b").join("x"), b"two").unwrap();
        swap(&dir.join("a"), &dir.join("b")).unwrap();
        assert_eq!(std::fs::read(dir.join("a").join("x")).unwrap(), b"two");
        assert_eq!(std::fs::read(dir.join("b").join("x")).unwrap(), b"one");
    }
}
