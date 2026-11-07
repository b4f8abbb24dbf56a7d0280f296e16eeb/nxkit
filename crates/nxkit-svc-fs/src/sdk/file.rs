//! Files, directories and paths.

use super::*;

pub(super) extern "C" fn open_file(out: u64, path: u64, mode: i32) -> u32 {
    run_text(path, |fs, p| match fs.open_file(p, mode as u32) {
        Ok(h) => put_u64(out, h),
        Err(r) => r,
    })
}

/// Reads and stores the count. A bad handle gives a count of zero, as the game keeps using the handle of a failed open.
pub(super) extern "C" fn read_file(out: u64, handle: u64, offset: i64, buf: u64, size: u64) -> u32 {
    let got = run_read(handle, offset, buf, size);
    let (n, r) = match got {
        Ok(n) => (n, OK),
        Err(r) => (0, r),
    };
    if out != 0 && put_u64(out, n) != OK && r == OK {
        return FAIL;
    }
    r
}

pub(super) extern "C" fn read_file_option(out: u64, handle: u64, offset: i64, buf: u64, size: u64, _opt: u64) -> u32 {
    read_file(out, handle, offset, buf, size)
}

/// Reads all `size` bytes. A read that ends at the end of the file fails.
pub(super) extern "C" fn read_file_exact(handle: u64, offset: i64, buf: u64, size: u64) -> u32 {
    match run_read(handle, offset, buf, size) {
        Ok(n) if n == size => OK,
        Ok(n) => {
            eprintln!("nxkit-svc-fs: short ReadFile at offset {offset}: wanted {size} bytes, got {n}");
            FAIL
        }
        Err(r) => r,
    }
}

pub(super) extern "C" fn read_file_exact_option(handle: u64, offset: i64, buf: u64, size: u64, _opt: u64) -> u32 {
    read_file_exact(handle, offset, buf, size)
}

fn run_read(handle: u64, offset: i64, buf: u64, size: u64) -> Result<u64, u32> {
    let fs = crate::fs().ok_or(FAIL)?;
    fs.file(handle)?.read(offset, buf, size)
}

pub(super) extern "C" fn write_file(handle: u64, offset: i64, buf: u64, size: u64, _opt: u64) -> u32 {
    run(|fs| match fs.file(handle) {
        Ok(f) => f.write(offset, buf, size),
        Err(r) => r,
    })
}

pub(super) extern "C" fn get_file_size(out: u64, handle: u64) -> u32 {
    run(|fs| match fs.file(handle).and_then(|f| f.size()) {
        Ok(n) => put_i64(out, n as i64),
        Err(r) => r,
    })
}

pub(super) extern "C" fn set_file_size(handle: u64, size: i64) -> u32 {
    run(|fs| match fs.file(handle) {
        Ok(f) => f.set_size(size),
        Err(r) => r,
    })
}

/// A write is in the page cache when `WriteFile` returns, so there is nothing to flush.
pub(super) extern "C" fn flush_file(_handle: u64) -> u32 {
    OK
}

pub(super) extern "C" fn close_file(handle: u64) {
    if let Some(fs) = crate::fs() {
        fs.close_file(handle);
    }
}

pub(super) extern "C" fn get_file_open_mode(handle: u64) -> u32 {
    crate::fs().and_then(|fs| fs.file(handle).ok()).map_or(0, |f| f.mode())
}

pub(super) extern "C" fn create_file(path: u64, size: i64) -> u32 {
    run_text(path, |fs, p| fs.create_file(p, size))
}

pub(super) extern "C" fn delete_file(path: u64) -> u32 {
    run_text(path, |fs, p| fs.delete_file(p))
}

pub(super) extern "C" fn create_directory(path: u64) -> u32 {
    run_text(path, |fs, p| fs.create_dir(p))
}

pub(super) extern "C" fn delete_directory(path: u64) -> u32 {
    run_text(path, |fs, p| fs.delete_dir(p))
}

pub(super) extern "C" fn delete_directory_recursively(path: u64) -> u32 {
    run_text(path, |fs, p| fs.delete_dir_recursively(p))
}

pub(super) extern "C" fn clean_directory_recursively(path: u64) -> u32 {
    run_text(path, |fs, p| fs.clean_dir_recursively(p))
}

pub(super) extern "C" fn rename_file(from: u64, to: u64) -> u32 {
    rename(from, to, false)
}

pub(super) extern "C" fn rename_directory(from: u64, to: u64) -> u32 {
    rename(from, to, true)
}

fn rename(from: u64, to: u64, is_dir: bool) -> u32 {
    run_text(from, |fs, f| match text(to) {
        Ok(t) => fs.rename(f, &t, is_dir),
        Err(r) => r,
    })
}

pub(super) extern "C" fn get_entry_type(out: u64, path: u64) -> u32 {
    run_text(path, |fs, p| match fs.entry_type(p) {
        Ok(t) => put_i32(out, t),
        Err(r) => r,
    })
}

pub(super) extern "C" fn open_directory(out: u64, path: u64, mode: i32) -> u32 {
    run_text(path, |fs, p| match fs.open_dir(p, mode as u32) {
        Ok(h) => put_u64(out, h),
        Err(r) => r,
    })
}

pub(super) extern "C" fn read_directory(count: u64, entries: u64, handle: u64, max: i64) -> u32 {
    run(|fs| match fs.dir(handle).and_then(|d| d.read(max, entries)) {
        Ok(n) => put_i64(count, n),
        Err(r) => r,
    })
}

pub(super) extern "C" fn close_directory(handle: u64) {
    if let Some(fs) = crate::fs() {
        fs.close_dir(handle);
    }
}

pub(super) extern "C" fn get_directory_entry_count(out: u64, handle: u64) -> u32 {
    run(|fs| match fs.dir(handle).and_then(|d| d.count()) {
        Ok(n) => put_i64(out, n),
        Err(r) => r,
    })
}
