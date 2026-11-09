//! The ranges of host memory whose pages must stay where they are: the memory under the GPU's no-copy buffers.
//!
//! The GPU driver wires the pages under a no-copy Metal buffer when the buffer becomes resident, and from then on the
//! GPU reads and writes those pages. A call that puts new pages under such a buffer (`mmap` with `MAP_FIXED`, which
//! `HostSpace::release` used, or `mach_vm_remap` with overwrite) leaves the GPU on the old pages: the CPU sees the new
//! ones and the GPU the old ones, and the old ones stay wired until the buffer is freed (measured 2026-11-09: a GPU copy
//! after `MAP_FIXED` read the old page, a GPU fill was not seen by the CPU, and the wired count did not drop). Inside
//! a pinned range `release` zeroes the pages in place, and `MapMemory` copies instead of sharing.
//!
//! There are two ways to pin. [`set`] pins one whole range (one buffer over the whole guest range) and replaces the
//! range it set before. [`pin`] and [`unpin`] pin a range for as long as a buffer lives (one buffer per memory pool).
//! A range that is pinned twice needs two unpins. Ranges of different pins may overlap and may share a host page
//! (buffers are rounded out to whole host pages, and neighbours share one): a page stays pinned until every pin that
//! reaches it is gone.
//!
//! The kernel asks only in memory calls, never on a hot path. About 50 pins and 25 unpins a second come from the GPU
//! thread, and a few thousand pins are live. One mutex covers the table. The table is a map from `(start, end)` to a
//! count, and the longest range ever pinned bounds the scan: [`overlaps`] costs `O(log n + k)` for `n` live pins and
//! `k` pins that start within that length before the end of the query, and [`pinned_pages`] costs the same plus a sort
//! of the `k` hits. Both are `O(n)` at worst, which is a few thousand steps. `NXKIT_GUEST_KEEP=0` is read by the host
//! (`nxkit-host`), which then pins nothing.

use crate::thread::lock;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

static START: AtomicU64 = AtomicU64::new(0);
static END: AtomicU64 = AtomicU64::new(0);
static PINS: Mutex<Pins> = Mutex::new(Pins::new());

/// The counted pins. `longest` is the longest range ever pinned and only grows, so a scan can start that far before
/// the query.
struct Pins {
    count: BTreeMap<(u64, u64), u32>,
    longest: u64,
}

impl Pins {
    const fn new() -> Pins {
        Pins {
            count: BTreeMap::new(),
            longest: 0,
        }
    }

    /// The pins that share a byte with `[start, end)`, in order of start.
    fn hits(&self, start: u64, end: u64) -> impl Iterator<Item = (u64, u64)> + '_ {
        let from = start.saturating_sub(self.longest);
        self.count
            .range((from, 0)..(end, 0))
            .map(|(&(s, e), _)| (s, e))
            .filter(move |&(_, e)| e > start)
    }
}

/// Pins `[start, end)` as the one whole range. A later call replaces that range; `set(0, 0)` takes it away. The
/// counted pins of [`pin`] are not touched.
pub fn set(start: u64, end: u64) {
    START.store(start, Ordering::Relaxed);
    END.store(end.max(start), Ordering::Release);
}

/// The whole range of [`set`] as `(start, end)`.
pub fn range() -> (u64, u64) {
    let end = END.load(Ordering::Acquire);
    (START.load(Ordering::Relaxed), end)
}

/// Pins `[start, end)`. An empty range pins nothing. The same range pinned twice needs two [`unpin`] calls.
pub fn pin(start: u64, end: u64) {
    if end <= start {
        return;
    }
    let mut p = lock(&PINS);
    *p.count.entry((start, end)).or_insert(0) += 1;
    p.longest = p.longest.max(end - start);
}

/// Takes back one [`pin`] of exactly `[start, end)`. Returns false when that range is not pinned. Another pin that
/// reaches the same pages keeps them pinned.
pub fn unpin(start: u64, end: u64) -> bool {
    let mut p = lock(&PINS);
    let Some(n) = p.count.get_mut(&(start, end)) else {
        return false;
    };
    *n -= 1;
    if *n == 0 {
        p.count.remove(&(start, end));
    }
    true
}

/// Drops the whole range and every counted pin.
pub fn clear() {
    set(0, 0);
    let mut p = lock(&PINS);
    p.count.clear();
    p.longest = 0;
}

/// True when `[addr, addr + len)` shares a byte with the whole range or with a counted pin.
pub fn overlaps(addr: u64, len: u64) -> bool {
    let (start, end) = range();
    let top = addr.saturating_add(len);
    len > 0 && ((addr < end && top > start) || lock(&PINS).hits(addr, top).next().is_some())
}

/// The pinned parts of `[start, end)`: sorted, merged where they touch or overlap, and cut to the range.
pub fn pinned_pages(start: u64, end: u64) -> Vec<(u64, u64)> {
    if end <= start {
        return Vec::new();
    }
    let mut parts: Vec<(u64, u64)> = lock(&PINS).hits(start, end).collect();
    let (ws, we) = range();
    if ws < we {
        parts.push((ws, we));
    }
    parts.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(parts.len());
    for (s, e) in parts {
        let (s, e) = (s.max(start), e.min(end));
        if e <= s {
            continue;
        }
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Cuts `[first, last)` into runs of whole `page`-sized pages, each marked true when a pin reaches it. A pin that
/// reaches a byte of a host page pins the page. The runs are in order, touch each other, and cover the range. `first`
/// and `last` are multiples of `page`. `release` gives back the false runs and zeroes the true runs in place.
pub fn split(first: u64, last: u64, page: u64) -> Vec<(u64, u64, bool)> {
    cut(first, last, page, &pinned_pages(first, last))
}

fn cut(first: u64, last: u64, page: u64, pins: &[(u64, u64)]) -> Vec<(u64, u64, bool)> {
    let mut out = Vec::new();
    let mut at = first;
    let mut push = |s: u64, e: u64, pinned: bool| {
        if e > s {
            out.push((s, e, pinned));
        }
    };
    for &(s, e) in pins {
        let s = (s & !(page - 1)).max(at);
        let e = e.next_multiple_of(page).min(last);
        if e <= s {
            continue;
        }
        push(at, s, false);
        push(s, e, true);
        at = e;
    }
    push(at, last, false);
    // two pinned runs that touch after the rounding are one run
    let mut merged: Vec<(u64, u64, bool)> = Vec::with_capacity(out.len());
    for run in out {
        match merged.last_mut() {
            Some(prev) if prev.2 == run.2 => prev.1 = run.1,
            _ => merged.push(run),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// The pins are statics of the process and the unit tests run in parallel: a test that touches them holds this
    /// lock, and starts and ends with no pin.
    static TEST: Mutex<()> = Mutex::new(());

    fn exclusive() -> MutexGuard<'static, ()> {
        let g = TEST.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        g
    }

    #[test]
    fn a_range_overlaps_when_it_shares_a_byte() {
        let _g = exclusive();
        set(0x1000, 0x3000);
        assert!(overlaps(0x2000, 0x10));
        assert!(overlaps(0x0, 0x1001));
        assert!(!overlaps(0x0, 0x1000));
        assert!(!overlaps(0x3000, 0x1000));
        assert!(!overlaps(0x2000, 0));
        set(0, 0);
        assert!(!overlaps(0x2000, 0x10));
    }

    #[test]
    fn a_counted_pin_needs_as_many_unpins() {
        let _g = exclusive();
        pin(0x4000, 0x8000);
        pin(0x4000, 0x8000);
        assert!(unpin(0x4000, 0x8000));
        assert!(overlaps(0x5000, 1));
        assert!(unpin(0x4000, 0x8000));
        assert!(!overlaps(0x5000, 1));
        assert!(!unpin(0x4000, 0x8000));
        assert!(pinned_pages(0, u64::MAX).is_empty());
    }

    #[test]
    fn overlaps_is_exact_at_the_edges_of_a_pin() {
        let _g = exclusive();
        pin(0x4000, 0x8000);
        assert!(!overlaps(0x3000, 0x1000));
        assert!(overlaps(0x3000, 0x1001));
        assert!(overlaps(0x7fff, 1));
        assert!(!overlaps(0x8000, 0x1000));
        assert!(!overlaps(0x5000, 0));
        // a long pin that starts far below the query is found
        pin(0x1_0000, 0x100_0000);
        assert!(overlaps(0xff_f000, 0x10));
        assert!(unpin(0x4000, 0x8000));
        assert!(unpin(0x1_0000, 0x100_0000));
        assert!(!overlaps(0x4000, 0x4000));
    }

    #[test]
    fn a_page_two_pins_share_stays_pinned_until_both_are_gone() {
        let _g = exclusive();
        // the pools share the page 0x4000..0x8000
        pin(0x0, 0x8000);
        pin(0x4000, 0xc000);
        assert!(unpin(0x0, 0x8000));
        assert!(overlaps(0x4000, 0x4000));
        assert!(overlaps(0x7fff, 1));
        assert!(!overlaps(0x0, 0x4000));
        assert_eq!(pinned_pages(0, 0x10000), vec![(0x4000, 0xc000)]);
        assert!(unpin(0x4000, 0xc000));
        assert!(!overlaps(0x4000, 0x4000));
    }

    #[test]
    fn pinned_pages_splits_merges_and_clips() {
        let _g = exclusive();
        pin(0x2000, 0x3000);
        pin(0x5000, 0x7000);
        pin(0x6000, 0x9000);
        pin(0x9000, 0xa000);
        pin(0x20000, 0x21000);
        assert_eq!(
            pinned_pages(0x2800, 0x9800),
            vec![(0x2800, 0x3000), (0x5000, 0x9800)]
        );
        assert_eq!(pinned_pages(0x3000, 0x5000), Vec::new());
        assert_eq!(pinned_pages(0x2000, 0x2000), Vec::new());
        assert_eq!(
            pinned_pages(0, 0x30000),
            vec![(0x2000, 0x3000), (0x5000, 0xa000), (0x20000, 0x21000)]
        );
        for (s, e) in [
            (0x2000, 0x3000),
            (0x5000, 0x7000),
            (0x6000, 0x9000),
            (0x9000, 0xa000),
            (0x20000, 0x21000),
        ] {
            assert!(unpin(s, e));
        }
    }

    #[test]
    fn set_is_the_whole_range_beside_the_counted_pins() {
        let _g = exclusive();
        set(0x10000, 0x20000);
        pin(0x4000, 0x5000);
        assert!(overlaps(0x4000, 1));
        assert!(overlaps(0x18000, 1));
        assert_eq!(
            pinned_pages(0, 0x30000),
            vec![(0x4000, 0x5000), (0x10000, 0x20000)]
        );
        // a later set replaces the whole range and leaves the pin
        set(0x40000, 0x50000);
        assert!(!overlaps(0x18000, 1));
        assert!(overlaps(0x4800, 1));
        set(0, 0);
        assert!(unpin(0x4000, 0x5000));
        assert!(!overlaps(0, u64::MAX));
    }

    #[test]
    fn split_cuts_at_pinned_host_pages() {
        // 16 KiB pages; the pins are cut by hand, without the statics
        let page = 0x4000;
        assert_eq!(cut(0, 0x10000, page, &[]), vec![(0, 0x10000, false)]);
        // a pin of one byte pins its page; two pins in one page, and two pages that touch, make one run
        let runs = cut(
            0,
            0x20000,
            page,
            &[(0x4001, 0x4002), (0x5000, 0x6000), (0x8000, 0xa000)],
        );
        assert_eq!(
            runs,
            vec![
                (0, 0x4000, false),
                (0x4000, 0xc000, true),
                (0xc000, 0x20000, false)
            ]
        );
        // a pin past the range is cut, a pin that fills the range gives one pinned run
        assert_eq!(
            cut(0x4000, 0x8000, page, &[(0x0, 0x10000)]),
            vec![(0x4000, 0x8000, true)]
        );
    }

    #[test]
    fn split_reads_the_pins() {
        let _g = exclusive();
        pin(0x14100, 0x14200);
        assert_eq!(
            split(0x10000, 0x20000, 0x4000),
            vec![
                (0x10000, 0x14000, false),
                (0x14000, 0x18000, true),
                (0x18000, 0x20000, false)
            ]
        );
        assert!(unpin(0x14100, 0x14200));
    }
}
