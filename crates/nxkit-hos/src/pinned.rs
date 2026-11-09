//! The range of host memory whose pages must stay where they are: the range of the no-copy GPU buffer.
//!
//! The GPU driver wires the pages under a no-copy Metal buffer when the buffer becomes resident, and from then on the
//! GPU reads and writes those pages. A call that puts new pages under the range (`mmap` with `MAP_FIXED`, which
//! `HostSpace::release` used, or `mach_vm_remap` with overwrite) leaves the GPU on the old pages: the CPU sees the new
//! ones and the GPU the old ones, and the old ones stay wired until the buffer is freed (measured 2026-11-09: a GPU copy
//! after `MAP_FIXED` read the old page, a GPU fill was not seen by the CPU, and the wired count did not drop). Inside
//! this range `release` zeroes the pages in place, and `MapMemory` copies instead of sharing.
//!
//! The host sets the range once, after it made the buffer. An empty range (the default) pins nothing.

use std::sync::atomic::{AtomicU64, Ordering};

static START: AtomicU64 = AtomicU64::new(0);
static END: AtomicU64 = AtomicU64::new(0);

/// Pins `[start, end)`. A later call replaces the range; `set(0, 0)` pins nothing.
pub fn set(start: u64, end: u64) {
    START.store(start, Ordering::Relaxed);
    END.store(end.max(start), Ordering::Release);
}

/// The pinned range as `(start, end)`.
pub fn range() -> (u64, u64) {
    let end = END.load(Ordering::Acquire);
    (START.load(Ordering::Relaxed), end)
}

/// True when `[addr, addr + len)` shares a byte with the pinned range.
pub fn overlaps(addr: u64, len: u64) -> bool {
    let (start, end) = range();
    len > 0 && addr < end && addr.saturating_add(len) > start
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_overlaps_when_it_shares_a_byte() {
        // the tests of the crate share the statics: only this test sets them
        set(0x1000, 0x3000);
        assert!(overlaps(0x2000, 0x10));
        assert!(overlaps(0x0, 0x1001));
        assert!(!overlaps(0x0, 0x1000));
        assert!(!overlaps(0x3000, 0x1000));
        assert!(!overlaps(0x2000, 0));
        set(0, 0);
        assert!(!overlaps(0x2000, 0x10));
    }
}
