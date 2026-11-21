//! Runs of pages, the block masks of written ranges, and releasing the runs of a frame.

use super::*;

/// Merges sorted page numbers into runs `(first, end)`.
pub(super) fn runs_of(sorted: &[u32]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &p in sorted {
        match out.last_mut() {
            Some(last) if p == last.1 => last.1 = p + 1,
            Some(last) if p < last.1 => {}
            _ => out.push((p, p + 1)),
        }
    }
    out
}

/// Page runs of the host ranges `(address, length)`, sorted and merged.
pub(super) fn span_runs(ranges: &[(u64, u64)]) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = ranges
        .iter()
        .filter_map(|&(a, l)| page_span(a, l))
        .collect();
    v.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(v.len());
    for (s, e) in v {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Sets the blocks that the byte offsets `[lo, hi)` of a page touch (rounded outward). `hi` must be above `lo`.
pub(super) fn set_blocks(m: &mut Mask, lo: u64, hi: u64) {
    let (first, last) = (lo / BLOCK, (hi - 1) / BLOCK);
    // One mask per word of the bits `first..=last` that lie in it.
    for (w, word) in m.iter_mut().enumerate() {
        let (from, to) = (w as u64 * 64, w as u64 * 64 + 63);
        if last < from || first > to {
            continue;
        }
        let (a, b) = (first.max(from) - from, last.min(to) - from);
        let bits = if b - a == 63 {
            u64::MAX
        } else {
            ((1u64 << (b - a + 1)) - 1) << a
        };
        *word |= bits;
    }
}

/// The blocks that the host ranges `(address, length)` touch in each page of `runs` (the runs of the same ranges, as
/// [`span_runs`] makes them): one mask per page, per run.
pub(super) fn range_masks(runs: &[(u32, u32)], ranges: &[(u64, u64)]) -> Vec<Vec<Mask>> {
    let mut out: Vec<Vec<Mask>> = runs
        .iter()
        .map(|&(s, e)| vec![[0; 4]; (e - s) as usize])
        .collect();
    let base = SHARED.base.load(Ordering::Acquire);
    let total = SHARED.len.load(Ordering::Acquire);
    let npages = SHARED.npages.load(Ordering::Acquire) as u64;
    let p = page();
    for &(a, l) in ranges {
        let lo = a.max(base);
        let hi = a.saturating_add(l.max(1)).min(base.saturating_add(total));
        if lo >= hi {
            continue;
        }
        let (first, last) = ((lo - base) / p, (hi - 1 - base) / p);
        for pg in first..=last.min(npages.saturating_sub(1)) {
            let i = runs.partition_point(|&(_, e)| u64::from(e) <= pg);
            let Some(&(s, e)) = runs.get(i) else { break };
            if pg < u64::from(s) || pg >= u64::from(e) {
                continue;
            }
            let from = if pg == first { (lo - base) % p } else { 0 };
            let to = if pg == last {
                (hi - 1 - base) % p + 1
            } else {
                p
            };
            set_blocks(&mut out[i][(pg - u64::from(s)) as usize], from, to);
        }
    }
    out
}

pub(super) fn in_runs(runs: &[(u32, u32)], p: u32) -> bool {
    let i = runs.partition_point(|&(_, e)| e <= p);
    runs.get(i).is_some_and(|&(s, _)| s <= p)
}

/// The pages `[first, end)` of the frame `frame` that are still protected for it go read only and clean.
pub(super) fn release_runs(frame: u64, runs: &[(u32, u32)]) {
    let want = pack(GPU, frame);
    for &(first, end) in runs {
        let mut start: Option<u32> = None;
        for p in first..end {
            let done = word(p as usize).is_some_and(|w| {
                w.compare_exchange(want, CLEAN, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            });
            match (done, start) {
                (true, None) => start = Some(p),
                (false, Some(s)) => {
                    protect_pages(s, p, PROT_READ, true);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            protect_pages(s, end, PROT_READ, true);
        }
    }
}

/// The pages of `runs` that a host thread opened for reading in frame `frame` lose their access again.
pub(super) fn close_opened(frame: u64, runs: &[(u32, u32)]) {
    for &(first, end) in runs {
        let mut start: Option<u32> = None;
        for p in first..end {
            let done = word(p as usize).is_some_and(|w| {
                loop {
                    let cur = w.load(Ordering::Acquire);
                    if cur & STATE_MASK != OPEN
                        || u64::from(cur >> FRAME_SHIFT) != frame & FRAME_MASK
                    {
                        break false;
                    }
                    let back = pack(GPU, frame) | (cur & FRESH);
                    if w.compare_exchange(cur, back, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        break true;
                    }
                }
            });
            match (done, start) {
                (true, None) => start = Some(p),
                (false, Some(s)) => {
                    protect_pages(s, p, PROT_NONE, true);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            protect_pages(s, end, PROT_NONE, true);
        }
    }
}
