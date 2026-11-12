//! Which ranges a frame may rename and the masks of the words that the CPU and the GPU write.

use super::*;

pub(super) fn overlaps(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Which of the ranges `cands` (`(address, size)`, sorted by address, disjoint ranges are not assumed) a frame renames.
///
/// - `bound` are the storage bindings of the frame. A range is renamed only if every binding that overlaps it lies
///   wholly inside it, because a binding that reaches out of the range cannot read two buffers.
/// - `blocked[i]` says that bytes which the frame reads or writes in other ways (copies, vertex, index and indirect
///   buffers, uniform buffers) overlap range `i` ([`blocked_flags`]). Such a range is left as it is.
/// - A range that overlaps one that was already taken is left as it is.
///
/// Returns the indexes into `cands` that are renamed, and how many were left out.
pub(crate) fn plan(
    cands: &[(u64, u32)],
    bound: &[(u64, u32)],
    blocked: &[bool],
) -> (Vec<usize>, usize) {
    let mut taken: Vec<(u64, u64)> = Vec::new();
    let mut accepted = Vec::new();
    let mut left = 0;
    for (i, &(addr, size)) in cands.iter().enumerate() {
        let r = (addr, addr + u64::from(size));
        let partial = bound.iter().any(|&(a, s)| {
            let b = (a, a + u64::from(s));
            overlaps(r, b) && !(r.0 <= b.0 && b.1 <= r.1)
        });
        let ok = !partial
            && !blocked.get(i).copied().unwrap_or(false)
            && !taken.iter().any(|&t| overlaps(r, t));
        if ok {
            taken.push(r);
            accepted.push(i);
        } else {
            left += 1;
        }
    }
    (accepted, left)
}

/// Which of a list of ranges the other uses of a frame overlap ([`blocked_flags`]).
pub(super) type BlockedBy<'a> = dyn Fn(&[(u64, u32)]) -> Vec<bool> + 'a;

/// Which of the ranges `ranges` (`(address, size)`, sorted by address) one of `items` (`(start, end)`) overlaps. One
/// binary search per item, so a frame with thousands of draws costs little however many ranges there are.
pub(crate) fn blocked_flags(
    ranges: &[(u64, u32)],
    items: impl IntoIterator<Item = (u64, u64)>,
) -> Vec<bool> {
    let mut blocked = vec![false; ranges.len()];
    // The largest end among the ranges up to each index, so that the search finds the first range an item can reach.
    let mut reach = Vec::with_capacity(ranges.len());
    let mut m = 0u64;
    for &(a, s) in ranges {
        m = m.max(a + u64::from(s));
        reach.push(m);
    }
    for (start, end) in items {
        let mut i = reach.partition_point(|&e| e <= start);
        while i < ranges.len() && ranges[i].0 < end {
            if overlaps(
                (ranges[i].0, ranges[i].0 + u64::from(ranges[i].1)),
                (start, end),
            ) {
                blocked[i] = true;
            }
            i += 1;
        }
    }
    blocked
}

/// The words of a range that the GPU changed: `(word index, start, end)` where `end` differs from `start`. A tail of
/// less than four bytes is left out, a compare-and-swap needs a whole word.
pub(crate) fn changed_words<'a>(
    start: &'a [u8],
    end: &'a [u8],
) -> impl Iterator<Item = (usize, u32, u32)> + 'a {
    start
        .as_chunks::<4>()
        .0
        .iter()
        .zip(end.as_chunks::<4>().0)
        .enumerate()
        .filter_map(|(i, (s, e))| {
            let (s, e) = (u32::from_le_bytes(*s), u32::from_le_bytes(*e));
            (s != e).then_some((i, s, e))
        })
}

/// The words that changed between the flush of the frame (`pre`) and the start of the wait for it (`atwait`), one bit per
/// word. For a range that shaders only read these are the CPU's writes in flight; for a range that shaders write they may
/// include writes of the GPU that finished before the wait began.
pub(crate) fn cpu_mask(pre: &[u8], atwait: &[u8]) -> Vec<u32> {
    let mut m = vec![0u32; pre.len().div_ceil(4).div_ceil(32)];
    for (i, (p, a)) in pre.chunks(4).zip(atwait.chunks(4)).enumerate() {
        if p != a {
            m[i / 32] |= 1 << (i % 32);
        }
    }
    m
}

/// The words where `end` differs from `start`, one bit per word.
pub(crate) fn gpu_words(end: &[u8], start: &[u8]) -> Vec<u32> {
    let mut m = vec![0u32; end.len().div_ceil(4).div_ceil(32)];
    for (i, (e, a)) in end.chunks(4).zip(start.chunks(4)).enumerate() {
        if e != a {
            m[i / 32] |= 1 << (i % 32);
        }
    }
    m
}

/// The words of `mask` read from guest memory at `addr`, packed, and for each block of 32 words the number of words
/// before it. `None` when a read fails.
pub(super) fn read_packed(
    read: &dyn Fn(u64, &mut [u8]) -> bool,
    addr: u64,
    mask: &[u32],
) -> Option<(Vec<u32>, Vec<u32>)> {
    let mut vals = Vec::new();
    let mut prefix = Vec::with_capacity(mask.len());
    let mut total = 0u32;
    for m in mask {
        prefix.push(total);
        total += m.count_ones();
    }
    let n = mask.len() * 32;
    let mut w = 0;
    while w < n {
        if mask[w / 32] >> (w % 32) & 1 == 0 {
            w += 1;
            continue;
        }
        let first = w;
        while w < n && mask[w / 32] >> (w % 32) & 1 != 0 {
            w += 1;
        }
        let mut bytes = vec![0u8; 4 * (w - first)];
        if !read(addr + 4 * first as u64, &mut bytes) {
            return None;
        }
        vals.extend(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| u32::from_le_bytes(*b)),
        );
    }
    Some((vals, prefix))
}

/// A range that the kernels can fill: whole 32-bit words at a 4 byte aligned address.
pub(super) fn probeable(addr: u64, size: u32) -> bool {
    size.is_multiple_of(4) && addr.is_multiple_of(4)
}

/// Adds the bits of `add` to `into`.
pub(crate) fn union_mask(into: &mut Vec<u32>, add: &[u32]) -> bool {
    if into.len() < add.len() {
        into.resize(add.len(), 0);
    }
    let mut grew = false;
    for (a, b) in into.iter_mut().zip(add) {
        grew |= *b & !*a != 0;
        *a |= b;
    }
    grew
}

pub(crate) fn is_empty_mask(a: &[u32]) -> bool {
    a.iter().all(|w| *w == 0)
}

/// The words of a range that the host writes back in the wait: `(word index, start, end)` where `end` differs from `start`.
/// A renamed range writes back every word that changed; a merge range only the words that both the CPU and the GPU write
/// (the kernel writes the others at the end of the frame).
pub(super) fn written_words(r: &CapRange, end: &[u8]) -> Vec<(usize, u32, u32)> {
    if r.kind != Kind::Merge {
        return changed_words(&r.start, end).collect();
    }
    let mut v = Vec::new();
    for (b, m) in r.rmask.iter().enumerate() {
        let mut bits = *m;
        while bits != 0 {
            let w = b * 32 + bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let e = end
                .get(4 * w..4 * w + 4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            if let (Some(cur), Some(e)) = (r.cvalue(w), e)
                && cur != e
            {
                v.push((w, cur, e));
            }
        }
    }
    v
}

/// What to swap into a guest word of a renamed range when the frame is written back, or `None` to leave the word.
///
/// - The guest word still holds what the frame started with: the frame's end value.
/// - The guest word holds what the write-back of the frame before wrote (`prev`): the CPU has not touched it since. If
///   the CPU waited between the flushes of the two frames (`same_epoch` is false) the newer value replaces it. If it did
///   not, the GPU of the console would have added onto the older value, so the word takes the older value plus what
///   this frame added, read as `f32` (the words that the game accumulates into are floats).
/// - Anything else: the CPU wrote the word since the flush, it stays.
pub(crate) fn decide_word(
    guest: u32,
    start: u32,
    end: u32,
    prev: Option<u32>,
    same_epoch: bool,
) -> Option<u32> {
    if guest == start {
        Some(end)
    } else if prev == Some(guest) {
        Some(if same_epoch {
            (f32::from_bits(guest) + (f32::from_bits(end) - f32::from_bits(start))).to_bits()
        } else {
            end
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The capture of a frame and the map of its bindings
// ---------------------------------------------------------------------------------------------------------------------
