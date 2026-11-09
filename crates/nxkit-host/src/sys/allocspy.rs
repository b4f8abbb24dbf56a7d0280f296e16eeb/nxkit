//! The allocation spy: `NXKIT_MEM_ALLOC=<MiB>` prints a line and a backtrace for every heap allocation (or growth) of
//! at least that many MiB, on stderr, with the prefix `nxkit-mem-alloc`. It finds who owns a large part of the host
//! heap, which the memory report (`crate::memreport`) only counts. It is off when the variable is unset: the cost is
//! one relaxed load and a compare per allocation.
//!
//! The spy is the global allocator of the `nxkit-host` binary and forwards to the system allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicU64, Ordering};

unsafe extern "C" {
    fn getenv(name: *const c_char) -> *const c_char;
}

const UNSET: u64 = u64::MAX - 1;
const OFF: u64 = u64::MAX;

/// The threshold in bytes, `UNSET` before the first allocation read the variable, `OFF` when it is not set.
static THRESHOLD: AtomicU64 = AtomicU64::new(UNSET);

thread_local! {
    /// True while this thread prints a report, so that the allocations of the report do not report themselves.
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

/// MiB in `text` as bytes. None for a value that is not a positive number.
pub fn parse_threshold(text: &str) -> Option<u64> {
    let mib: u64 = text.trim().parse().ok()?;
    (mib > 0).then(|| mib.saturating_mul(1024 * 1024))
}

fn threshold() -> u64 {
    let t = THRESHOLD.load(Ordering::Relaxed);
    if t != UNSET {
        return t;
    }
    // SAFETY: `getenv` gets a NUL-terminated name and allocates nothing. The value is read at once and not kept.
    let value = unsafe { getenv(c"NXKIT_MEM_ALLOC".as_ptr()) };
    let t = if value.is_null() {
        OFF
    } else {
        // SAFETY: `getenv` returned a NUL-terminated string.
        let text = unsafe { CStr::from_ptr(value) }.to_str().unwrap_or("");
        parse_threshold(text).unwrap_or(OFF)
    };
    THRESHOLD.store(t, Ordering::Relaxed);
    t
}

#[cold]
fn report(what: &str, size: usize) {
    if BUSY.with(|b| b.replace(true)) {
        return;
    }
    let bt = std::backtrace::Backtrace::force_capture();
    eprintln!(
        "nxkit-mem-alloc: {what} {} MiB on thread {:?}\n{bt}",
        size / (1024 * 1024),
        std::thread::current().name().unwrap_or("?")
    );
    BUSY.with(|b| b.set(false));
}

fn watch(what: &str, size: usize) {
    let t = threshold();
    if t != OFF && size as u64 >= t {
        report(what, size);
    }
}

/// The system allocator with the spy in front.
pub struct Spy;

// SAFETY: every method forwards to `System` with the same arguments, after a report that allocates only on its own
// thread behind the `BUSY` guard.
unsafe impl GlobalAlloc for Spy {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        watch("alloc", layout.size());
        // SAFETY: the caller's contract is the contract of `System`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        watch("alloc_zeroed", layout.size());
        // SAFETY: as in `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as in `alloc`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            watch("realloc", new_size);
        }
        // SAFETY: as in `alloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_threshold_is_in_mib() {
        assert_eq!(parse_threshold("64"), Some(64 << 20));
        assert_eq!(parse_threshold(" 1 "), Some(1 << 20));
        assert_eq!(parse_threshold("0"), None);
        assert_eq!(parse_threshold("x"), None);
    }
}
