//! Safe overlap by renaming: a frame reads and writes its own copy of the small storage ranges that both the CPU and the
//! GPU write (`docs/nxkit-gpu.md`, "Renamed storage ranges under the overlap").
//!
//! With the overlap on, the CPU writes the data of frame N+1 while frame N still runs on the GPU. A storage buffer that
//! a compute shader accumulates into and that the CPU resets or re-initialises every frame (a sum that the CPU reads and
//! zeroes, a block of coefficients) then races: the CPU's writes for the next frame meet the GPU's sums of this one. The
//! host learns such ranges and gives each frame a copy of its own:
//!
//! 1. **Learning** ([`Learn`], [`judge`]). A storage buffer of at most [`MAX_BYTES`] that a shader may write (`rw` of
//!    its binding) is a candidate. For each frame in which it is bound, three copies of its bytes are compared: `pre`
//!    (at the flush of the frame), `post` (after the GPU finished the frame, read in a wait that is honest) and `pre_next`
//!    (at the flush of the next frame). The words the GPU wrote are those where `post` differs from `pre`. If the CPU
//!    changed each of them again before the next flush (`pre_next` differs from `post`) the frame is clean; a word that
//!    stayed is state that the GPU keeps from frame to frame, and the range is never renamed. After [`LEARN_FRAMES`]
//!    clean frames the range is renamed. A frame where the GPU wrote nothing neither counts nor resets. A range that
//!    stays quiet for [`QUIET_LIMIT`] frames is set aside for [`RECHECK_EVERY`] frames. A renamed range is checked
//!    again every [`RECHECK_EVERY`] frames (one frame bound to the guest range, with an honest wait), and one dirty
//!    frame makes it `Never`.
//! 2. **Capture** ([`Renamer::at_flush`], on the CPU thread at the flush of a frame). The guest bytes of each renamed
//!    range are copied into the [`Capture`] of the frame (`start`). The range stays as it is if a copy, a vertex, index
//!    or indirect buffer, or a uniform buffer touches it, or a binding covers it only in part ([`plan`]).
//! 3. **Binding** ([`Renamer::begin_slot`], at the encode). The bytes go into a buffer that belongs to the batch slot of
//!    the frame, and every storage binding inside the range reads that copy ([`RenameMap::resolve`], called from
//!    `bindless::args::sbuf_word`). A range that a raw global write touched is left out here.
//! 4. **Write-back** ([`Renamer::after_wait`]). In a CPU wait for the frame (never at bare completion), each word that
//!    the GPU changed (`end` differs from `start`) goes into guest memory with a compare-and-swap that needs the guest
//!    word to still hold `start`; a word that the CPU wrote since stays. The slot's frame is also written back before its
//!    buffer is reused.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use nxkit_metal::{BATCHES_IN_FLIGHT, Context, DataBuffer, GuestSpace};
use nxkit_nvn::diag::{Reason, note_n};

use crate::detail::{DrawArgs, FrameDetail, Stage};
use crate::draws::{FastMap, FastSet};
use crate::programs::{BindKind, Programs};

mod merge;
pub(crate) use merge::{MergeRun, Phase, encode as merge_encode};

/// Clean frames that make a range a renamed one.
pub(crate) const LEARN_FRAMES: u32 = 8;
/// Frames between two checks of a renamed range, and the pause of a range that stayed quiet.
pub(crate) const RECHECK_EVERY: u64 = 300;
/// Frames in a row in which the GPU wrote nothing into a range before it is set aside.
pub(crate) const QUIET_LIMIT: u32 = 32;
/// Ranges are judged only in one flush in this many (and in the flush after it, which takes `pre_next`): the wait for a
/// judged frame is a full wait, so the judging costs the overlap of one frame in this many.
pub(crate) const LEARN_PERIOD: u64 = 16;
/// Frames without a new candidate or a change of class after which the judging backs off to [`SLOW_PERIOD`].
pub(crate) const QUIET_FRAMES: u64 = 300;
/// One judged flush in this many once nothing new was found for [`QUIET_FRAMES`].
pub(crate) const SLOW_PERIOD: u64 = 300;
/// The largest range of a snapshot (a range that shaders only read and the CPU writes in flight).
pub(crate) const SNAPSHOT_MAX_BYTES: u32 = 1 << 20;
/// The bytes of all snapshots of one frame.
pub(crate) const SNAPSHOT_BUDGET: u64 = 8 << 20;
/// The bytes that the judged ranges of one flush may read.
const WATCH_BUDGET: u64 = 8 << 20;
/// Judged frames in which the masks of a merge range may grow before it is given up.
const MAX_GROWN: u32 = 3;
/// Judged frames in which the CPU wrote a read-only range in flight before it becomes a snapshot.
const SNAPSHOT_HITS: u32 = 2;
/// Frames that a range rests after a judged frame in which the CPU did not write it in flight.
const WATCH_PAUSE: u64 = 600;
/// The largest range that is renamed.
pub(crate) const MAX_BYTES: u32 = 64 * 1024;
/// The most candidate ranges.
const MAX_CANDIDATES: usize = 2048;
/// The part of a buffer of unknown size that counts as touched by a copy or a vertex stream.
const UNKNOWN_BYTES: u64 = 1 << 20;
/// Alignment of the copies in a slot buffer.
const ALIGN: u64 = 16;

// ---------------------------------------------------------------------------------------------------------------------
// The learner
// ---------------------------------------------------------------------------------------------------------------------

/// What one frame says about a range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The GPU wrote no word.
    Quiet,
    /// The GPU wrote words and the CPU changed each of them again before the next flush.
    Clean,
    /// The GPU wrote a word that the CPU did not change again: state that stays on the GPU.
    Dirty,
}

/// Judges one frame from the bytes at its flush (`pre`), after the GPU finished it (`post`) and at the next flush
/// (`next`). The words are 32 bit; a shorter tail is one word.
pub(crate) fn judge(pre: &[u8], post: &[u8], next: &[u8]) -> Verdict {
    let mut written = false;
    for ((p, q), n) in pre.chunks(4).zip(post.chunks(4)).zip(next.chunks(4)) {
        if p != q {
            written = true;
            if q == n {
                return Verdict::Dirty;
            }
        }
    }
    if written {
        Verdict::Clean
    } else {
        Verdict::Quiet
    }
}

/// The class of a range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// Not decided yet; the frames that bind it are judged.
    Learning,
    /// Each frame reads a copy of its own.
    Rename,
    /// The GPU keeps state in it, or the CPU does not reset what the GPU wrote. Never renamed.
    Never,
    /// The GPU wrote nothing for a long time. Not watched until the pause is over.
    Idle,
    /// Bound only for reading so far; the frames that bind it are judged for CPU writes in flight.
    Watch,
    /// Only read by the GPU and written by the CPU in flight: each frame reads a copy of its own, nothing goes back.
    Snapshot,
    /// Written by the GPU and, in other words, by the CPU in flight: each frame reads a copy that the GPU fills at its
    /// start (guest bytes, then the CPU's words) and merges back at its end (the GPU's words) (`merge`).
    Merge,
    /// Written by the GPU and by the CPU in flight, in the same words. The wait for a frame that binds it is a full one.
    Conflict,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Learning => "Learning",
            Class::Rename => "Rename",
            Class::Never => "Never",
            Class::Idle => "Idle",
            Class::Watch => "Watch",
            Class::Snapshot => "Snapshot",
            Class::Merge => "Merge",
            Class::Conflict => "Conflict",
        }
    }
}

/// The learning state of one range.
#[derive(Clone, Debug)]
pub(crate) struct Learn {
    class: Class,
    /// Clean frames so far.
    clean: u32,
    /// Quiet frames in a row.
    quiet: u32,
    /// Frames since the range became `Rename` or `Idle`, or was last checked.
    since: u64,
    /// A renamed range that is checked: the frames are bound to the guest range and judged.
    checking: bool,
    /// Frames in which the range was bound.
    frames: u64,
}

impl Default for Learn {
    fn default() -> Self {
        Self::new()
    }
}

impl Learn {
    pub(crate) fn new() -> Self {
        Self {
            class: Class::Learning,
            clean: 0,
            quiet: 0,
            since: 0,
            checking: false,
            frames: 0,
        }
    }

    /// A range that is only read: its frames are watched.
    fn watch() -> Self {
        Self {
            class: Class::Watch,
            ..Self::new()
        }
    }

    fn set(&mut self, class: Class) {
        self.class = class;
        self.checking = false;
    }

    pub(crate) fn class(&self) -> Class {
        self.class
    }

    /// Frames in which the range was bound.
    pub(crate) fn frames(&self) -> u64 {
        self.frames
    }

    /// The range is bound in a frame. Starts a check of a renamed range that is due, and ends the pause of an idle one.
    pub(crate) fn tick(&mut self) {
        self.frames += 1;
        match self.class {
            Class::Rename => {
                self.since += 1;
                if self.since >= RECHECK_EVERY && !self.checking {
                    self.checking = true;
                    self.quiet = 0;
                }
            }
            Class::Idle => {
                self.since += 1;
                if self.since >= RECHECK_EVERY {
                    self.class = Class::Learning;
                    self.clean = 0;
                    self.quiet = 0;
                }
            }
            Class::Learning
            | Class::Never
            | Class::Watch
            | Class::Snapshot
            | Class::Merge
            | Class::Conflict => {}
        }
    }

    /// True when the frame that binds the range is judged: it is bound to the guest range and its wait is honest.
    pub(crate) fn sampling(&self) -> bool {
        self.class == Class::Learning || (self.class == Class::Rename && self.checking)
    }

    /// True when the frame that binds the range reads a copy.
    #[cfg(test)]
    pub(crate) fn renaming(&self) -> bool {
        self.class == Class::Rename && !self.checking
    }

    /// Takes the verdict of a frame. Returns the new class when it changed.
    pub(crate) fn observe(&mut self, v: Verdict) -> Option<Class> {
        match (self.class, v) {
            (Class::Learning, Verdict::Dirty) => {
                self.class = Class::Never;
                Some(Class::Never)
            }
            (Class::Learning, Verdict::Clean) => {
                self.quiet = 0;
                self.clean += 1;
                if self.clean >= LEARN_FRAMES {
                    self.class = Class::Rename;
                    self.since = 0;
                    self.checking = false;
                    return Some(Class::Rename);
                }
                None
            }
            (Class::Learning, Verdict::Quiet) => {
                self.quiet += 1;
                if self.quiet >= QUIET_LIMIT {
                    self.class = Class::Idle;
                    self.since = 0;
                    return Some(Class::Idle);
                }
                None
            }
            (Class::Rename, Verdict::Dirty) if self.checking => {
                self.class = Class::Never;
                self.checking = false;
                Some(Class::Never)
            }
            (Class::Rename, Verdict::Clean) if self.checking => {
                self.checking = false;
                self.since = 0;
                None
            }
            (Class::Rename, Verdict::Quiet) if self.checking => {
                // A frame that says nothing does not end the check, but the check does not go on for ever.
                self.quiet += 1;
                if self.quiet >= QUIET_LIMIT {
                    self.checking = false;
                    self.since = 0;
                    self.quiet = 0;
                }
                None
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// What a frame may rename
// ---------------------------------------------------------------------------------------------------------------------

fn overlaps(a: (u64, u64), b: (u64, u64)) -> bool {
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
type BlockedBy<'a> = dyn Fn(&[(u64, u32)]) -> Vec<bool> + 'a;

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
fn read_packed(
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
fn probeable(addr: u64, size: u32) -> bool {
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
fn written_words(r: &CapRange, end: &[u8]) -> Vec<(usize, u32, u32)> {
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

/// How a frame binds a range that it has a copy of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    /// Written by the GPU, re-initialised by the CPU: the end value goes back in the wait.
    Rename,
    /// Only read: nothing goes back.
    Snapshot,
    /// The GPU merges the copy with guest memory at the start and the end of the frame.
    Merge,
    /// A written range in a judged frame: the GPU reads and writes a copy that it fills from guest memory at the start
    /// (a second copy keeps the start state), and writes the words that it changed back at the end. Guest memory is
    /// written by the CPU only while the frame runs, so the host can tell the CPU's words from the GPU's.
    Probe,
}

/// A renamed range of one frame with the guest bytes at its flush.
#[derive(Debug)]
pub(crate) struct CapRange {
    pub(crate) addr: u64,
    /// The bytes of the range.
    pub(crate) len: u32,
    /// The bytes at the flush: the whole range for `Rename` and `Snapshot`, nothing for the others.
    pub(crate) start: Vec<u8>,
    pub(crate) kind: Kind,
    /// For a merge range, one bit per word: `cmask` the words that the CPU writes in flight, `gmask` the words that only
    /// the GPU writes (`P`: the kernel at the end of the frame writes these into guest memory) and `rmask` the words that
    /// both write (`R`: the host writes the GPU's value back in the wait, if the CPU has not written the word since).
    pub(crate) cmask: Vec<u32>,
    pub(crate) gmask: Vec<u32>,
    pub(crate) rmask: Vec<u32>,
    /// For a merge range: the bytes of the words of `cmask` at the flush, packed, and for each block of 32 words the
    /// number of such words before it.
    pub(crate) cvals: Vec<u32>,
    pub(crate) cprefix: Vec<u32>,
}

impl CapRange {
    /// The bytes of the word `w` at the flush, for a word of `cmask` of a merge range.
    pub(crate) fn cvalue(&self, w: usize) -> Option<u32> {
        let block = *self.cmask.get(w / 32)?;
        if block >> (w % 32) & 1 == 0 {
            return None;
        }
        let before = (block & ((1u32 << (w % 32)) - 1)).count_ones();
        self.cvals
            .get((self.cprefix.get(w / 32)? + before) as usize)
            .copied()
    }
}

/// The renamed ranges of a frame, taken at its flush on the CPU thread.
#[derive(Debug)]
pub(crate) struct Capture {
    /// The fence of the frame.
    pub(crate) fence: u64,
    pub(crate) ranges: Vec<CapRange>,
    /// `NXKIT_DIAG=rename` prints this frame's capture and write-back.
    pub(crate) sampled: bool,
    /// The number of CPU waits so far at the flush. Two frames with the same number have no wait between them.
    pub(crate) epoch: u64,
}

/// Where each renamed range of a frame is read: `(guest address, size, real address of the copy)`, sorted by address.
#[derive(Default)]
pub(crate) struct RenameMap {
    entries: Vec<(u64, u32, u64)>,
}

impl RenameMap {
    /// A map without a range.
    pub(crate) const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// The map of `entries`, `(guest address, size, real address of the copy)`.
    pub(crate) fn from_entries(mut entries: Vec<(u64, u32, u64)>) -> Self {
        entries.sort_unstable();
        Self { entries }
    }

    /// The real address of the binding `[addr, addr + size)` when it lies inside a renamed range: the copy with the same
    /// offset into the range.
    pub(crate) fn resolve(&self, addr: u64, size: u32) -> Option<u64> {
        if self.entries.is_empty() {
            return None;
        }
        let i = self
            .entries
            .partition_point(|e| e.0 <= addr)
            .checked_sub(1)?;
        let (start, len, real) = self.entries[i];
        let inside = addr - start + u64::from(size) <= u64::from(len);
        inside.then(|| real + (addr - start))
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The state
// ---------------------------------------------------------------------------------------------------------------------

/// The bytes of one frame of a range that is being judged.
struct Sample {
    fence: u64,
    /// The flush counter of the frame; the sample is judged at the next flush and dropped if that does not bind it.
    frame: u64,
    pre: Vec<u8>,
    /// The bytes after the GPU finished the frame, when they were read before the next flush.
    post: Option<Vec<u8>>,
    /// The bytes at the start of the wait for the frame, before the GPU was known to be done (`NXKIT_DIAG=rename`).
    atwait: Option<Vec<u8>>,
}

struct Cand {
    /// Bound only for reading so far.
    ro: bool,
    /// Judged frames in which the CPU wrote the range in flight.
    hits: u32,
    /// The words that the CPU wrote in flight and the words that the GPU wrote, over the judged frames (bit per word).
    cmask: Vec<u32>,
    gmask: Vec<u32>,
    /// Judged frames of a merge range in which a mask grew: the host did not know all the words yet.
    grown: u32,
    /// The flush counter before which the range is not judged for CPU writes in flight again.
    rest_until: u64,
    size: u32,
    learn: Learn,
    sample: Option<Sample>,
    /// The flush counter at the last frame that bound the range.
    last: u64,
    /// The bindings that bound the range, up to [`MAX_KEYS`]: what it teaches and what teaches it.
    keys: Vec<Key>,
    /// The class comes from the keys of its bindings, not from judging the range.
    derived: bool,
}

/// The class and masks of a range that is bound by one more key.
fn combine(c: &mut Cand, ki: &KeyInfo) {
    use Class::*;
    let (a, b) = (c.learn.class(), ki.class);
    let class = match (a, b) {
        (x, y) if x == y => x,
        (Conflict, _) | (_, Conflict) => Conflict,
        (Merge, Rename) | (Rename, Merge) => {
            note_n(Reason::OverlapKeyDisagree, 1);
            Conflict
        }
        // Reading needs nothing; a range that nothing else needs stays as it is.
        (Snapshot, x) | (x, Snapshot) | (Never, x) | (x, Never) => x,
        (x, _) => x,
    };
    c.learn.set(class);
    c.ro = c.ro && ki.ro;
    union_mask(&mut c.cmask, &ki.cmask);
    union_mask(&mut c.gmask, &ki.gmask);
}

/// A binding as a class is learned for it: the program, the stage and slot, and the size in blocks of 256 bytes. The ring
/// buffers of a game change their address from frame to frame, and the binding stays the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    /// The hash of the program key.
    prog: u64,
    /// The slot, with the stage in the high bits.
    slot_stage: u32,
    /// The size rounded up to 256 bytes.
    size: u32,
}

/// What is known of the ranges that a key binds: their class and the masks of their words (from the start of the range).
#[derive(Clone, Debug)]
struct KeyInfo {
    class: Class,
    ro: bool,
    cmask: Vec<u32>,
    gmask: Vec<u32>,
    grown: u32,
}

/// A storage binding of a frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Binding {
    addr: u64,
    size: u32,
    key: Key,
    at: Recheck,
}

/// Where a binding is in the detail of its frame: `(0, draw, buffer)` or `(1, dispatch, buffer)`.
pub(crate) type Recheck = (u8, u32, u32);

/// Keys that one range remembers.
const MAX_KEYS: usize = 4;

/// A range that a frame renames.
struct Renamed {
    addr: u64,
    len: u32,
    start: Vec<u8>,
    kind: Kind,
    cmask: Vec<u32>,
    gmask: Vec<u32>,
    rmask: Vec<u32>,
    cvals: Vec<u32>,
    cprefix: Vec<u32>,
}

/// One flush's result for the capture.
#[derive(Default)]
struct Flushed {
    /// The ranges to rename, with their bytes.
    renamed: Vec<Renamed>,
    /// A frame binds a range that needs a full wait.
    conflict: bool,
    /// A read-only range is measured in this frame: the start of the wait for it is looked at.
    watching: bool,
    /// The time of the judging and of the capture in this flush (`NXKIT_DIAG=rename`).
    judge_ns: u64,
    capture_ns: u64,
    /// The bytes of the snapshots among them.
    snapshot_bytes: u64,
    /// Snapshots left out because the frame is over the budget.
    over_budget: usize,
    /// Ranges left out by [`plan`].
    excluded: usize,
    /// A range is judged in this frame, so the wait for it must be honest.
    sampling: bool,
    /// Among them a renamed range that is checked.
    checking: bool,
    /// A change of class to announce: address, size, class, frames.
    news: Vec<(u64, u32, Class, u64)>,
}

#[derive(Default)]
struct State {
    /// The flushes so far.
    frame: u64,
    cands: FastMap<u64, Cand>,
    /// What is known of each key.
    keys: FastMap<Key, KeyInfo>,
    /// Whether the program of a key may write the binding (what the encoder found), so that the flush does not send a
    /// binding that is only read to the encoder again.
    rw_known: FastMap<Key, bool>,
    /// The hash of a program key, by the allocation of the key (draws), which the cache keeps alive.
    prog_hash: FastMap<usize, (Arc<str>, u64)>,
    /// Whether a program may write a binding, by the key's allocation (draws) or the hash of the key (dispatches), with
    /// the slot and stage.
    rw_ptr: FastMap<(usize, u32), (Arc<str>, bool)>,
    rw_str: FastMap<(u64, u32), bool>,
    /// The flush counter at the last event: a candidate with an address not seen before, or a change of class.
    last_event: u64,
    /// Every address that was a candidate, so that a ring of buffers that comes round is not new each time.
    seen: FastSet<u64>,
    /// Judge one flush in [`LEARN_PERIOD`] always (for tests).
    fast_only: bool,
    /// The knob `conflictwait`: a frame that binds a range that the CPU writes in flight waits in full.
    conflict_wait: bool,
    /// `NXKIT_DIAG=rename`: ranges that are watched although they are not learned (`Never`, `Idle`, read-only).
    watch: bool,
    /// `NXKIT_DIAG=rename`: for each watched range, the judged frames and the CPU writes in flight found in them.
    inflight: FastMap<u64, Inflight>,
    /// Ranges that became snapshots in a wait, to announce at the next flush.
    pending_news: Vec<u64>,
}

/// The CPU writes that were found in a range while a frame that binds it was in flight.
#[derive(Default)]
struct Inflight {
    size: u32,
    class: &'static str,
    ro: bool,
    /// Judged frames, and those in which the CPU had changed a word that the GPU did not write.
    frames: u64,
    hit: u64,
    /// The most words in one frame.
    words: usize,
}

impl State {
    /// The hash of the program key `k`, cached by its allocation.
    fn prog_of(&mut self, k: &Arc<str>) -> u64 {
        use std::hash::BuildHasher;
        let id = Arc::as_ptr(k).cast::<u8>() as usize;
        if let Some((_, h)) = self.prog_hash.get(&id) {
            return *h;
        }
        if self.prog_hash.len() > 50_000 {
            self.prog_hash.clear();
        }
        let h = foldhash::fast::FixedState::default().hash_one(&**k);
        self.prog_hash.insert(id, (k.clone(), h));
        h
    }

    /// The storage bindings of a frame, each distinct binding once.
    fn bindings(&mut self, detail: &FrameDetail) -> (Vec<Binding>, Vec<(u64, u32)>) {
        use std::hash::BuildHasher;
        let mut out = Vec::new();
        // The distinct ranges, sorted, for the checks of the frame.
        let mut ranges: Vec<(u64, u32)> = Vec::new();
        let mut seen_range = vec![(0u64, 0u32); 4096];
        // A direct-mapped table of the bindings seen lately drops the repeats: the same few hundred buffers are bound by
        // thousands of draws.
        let mut seen = vec![(0u64, 0u32, 0u32, 0u64); 4096];
        let mut push = |s: &crate::detail::UboDetail, prog: u64, at: Recheck| {
            if s.address == 0 || s.size == 0 || s.size > SNAPSHOT_MAX_BYTES {
                return;
            }
            let slot_stage = s.slot | (s.stage as u32) << 24;
            let ix = ((s.address >> 4) ^ (s.address >> 16) ^ prog ^ u64::from(slot_stage)) as usize
                & 4095;
            let id = (s.address, s.size, slot_stage, prog);
            if seen[ix] == id {
                return;
            }
            seen[ix] = id;
            let rat = ((s.address >> 4) ^ (s.address >> 16) ^ u64::from(s.size)) as usize & 4095;
            if seen_range[rat] != (s.address, s.size) {
                seen_range[rat] = (s.address, s.size);
                ranges.push((s.address, s.size));
            }
            out.push(Binding {
                addr: s.address,
                size: s.size,
                key: Key {
                    prog,
                    slot_stage,
                    size: s.size.next_multiple_of(256),
                },
                at,
            });
        };
        // The hash of the last program key of each stage, by its allocation.
        let mut vs: Option<(usize, u64)> = None;
        let mut fs: Option<(usize, u64)> = None;
        for (di, d) in detail.draws.iter().enumerate() {
            if d.args.is_mesh() || d.sbufs.is_empty() {
                continue;
            }
            for (si, s) in d.sbufs.iter().enumerate() {
                let (k, memo) = match s.stage {
                    Stage::Vertex => (Some(&d.program.vertex), &mut vs),
                    Stage::Fragment => (d.program.fragment.as_ref(), &mut fs),
                    _ => continue,
                };
                let Some(k) = k else { continue };
                let ptr = Arc::as_ptr(k).cast::<u8>() as usize;
                let prog = match memo {
                    Some((p, h)) if *p == ptr => *h,
                    _ => {
                        let h = self.prog_of(k);
                        *memo = Some((ptr, h));
                        h
                    }
                };
                push(s, prog, (0, di as u32, si as u32));
            }
        }
        for (di, d) in detail.dispatches.iter().enumerate() {
            let mut prog = None;
            for (si, s) in d.sbufs.iter().enumerate() {
                if s.stage != Stage::Compute {
                    continue;
                }
                let h = *prog.get_or_insert_with(|| {
                    foldhash::fast::FixedState::default().hash_one(d.program.as_str())
                });
                push(s, h, (1, di as u32, si as u32));
            }
        }
        ranges.sort_unstable();
        ranges.dedup();
        (out, ranges)
    }

    /// The bindings of a frame meet the ranges that the host knows. A range that is not known, bound by a key with a
    /// class, gets the class at once; a range that several keys bind gets what all of them need (a snapshot only reads,
    /// so it asks for nothing; a merge and a renamed range cannot be combined: `Conflict`, the frame waits in full).
    fn adopt(&mut self, bindings: &[Binding]) -> Vec<Recheck> {
        let now = self.frame;
        let State {
            cands,
            keys,
            rw_known,
            ..
        } = self;
        // The bindings that the encoder has to look at: whether a shader writes them is not known yet.
        let mut recheck = Vec::new();
        for b in bindings {
            if let Some(c) = cands.get_mut(&b.addr) {
                let read_only = c.ro || c.learn.class() == Class::Snapshot;
                if b.size > c.size || (read_only && rw_known.get(&b.key) != Some(&false)) {
                    recheck.push(b.at);
                }
                if c.keys.len() < MAX_KEYS && !c.keys.contains(&b.key) {
                    c.keys.push(b.key);
                    if c.derived
                        && let Some(ki) = keys.get(&b.key)
                    {
                        combine(c, ki);
                    }
                }
            } else if cands.len() < MAX_CANDIDATES
                && let Some(ki) = keys.get(&b.key)
            {
                let mut learn = Learn::new();
                learn.set(ki.class);
                let mut c = Cand {
                    ro: ki.ro,
                    hits: SNAPSHOT_HITS,
                    cmask: ki.cmask.clone(),
                    gmask: ki.gmask.clone(),
                    grown: ki.grown,
                    // A range of a known class is not judged again soon.
                    rest_until: now + WATCH_PAUSE,
                    size: b.size,
                    learn,
                    sample: None,
                    last: now,
                    keys: vec![b.key],
                    derived: true,
                };
                if ki.class == Class::Merge && !probeable(b.addr, b.size) {
                    c.learn.set(Class::Never);
                }
                cands.insert(b.addr, c);
            } else {
                recheck.push(b.at);
            }
        }
        recheck
    }

    /// Tells the keys of the range at `addr` what was found for it.
    fn push_keys(&mut self, addr: u64) {
        let State { cands, keys, .. } = self;
        let Some(c) = cands.get(&addr) else {
            return;
        };
        let class = c.learn.class();
        if !matches!(
            class,
            Class::Rename | Class::Snapshot | Class::Merge | Class::Conflict | Class::Never
        ) {
            return;
        }
        for k in &c.keys {
            match keys.get_mut(k) {
                None => {
                    keys.insert(
                        *k,
                        KeyInfo {
                            class,
                            ro: c.ro,
                            cmask: c.cmask.clone(),
                            gmask: c.gmask.clone(),
                            grown: c.grown,
                        },
                    );
                }
                // A range that is `Never` for its own reasons does not unteach a key.
                Some(_) if class == Class::Never => {}
                Some(ki) if ki.class == Class::Conflict => {}
                Some(ki) => {
                    ki.class = class;
                    ki.ro = c.ro;
                    union_mask(&mut ki.cmask, &c.cmask);
                    union_mask(&mut ki.gmask, &c.gmask);
                    ki.grown = ki.grown.max(c.grown);
                }
            }
        }
    }

    /// A candidate at `addr` is added: it is an event when the address was never a candidate.
    fn fresh(&mut self, addr: u64) {
        if self.seen.len() > 100_000 {
            self.seen.clear();
        }
        if self.seen.insert(addr) {
            self.last_event = self.frame;
        }
    }

    /// The flushes between two judged flushes now: every [`LEARN_PERIOD`] while something new is found, every
    /// [`SLOW_PERIOD`] after [`QUIET_FRAMES`] flushes without an event.
    fn period(&self) -> u64 {
        if !self.fast_only && self.frame.saturating_sub(self.last_event) >= QUIET_FRAMES {
            SLOW_PERIOD
        } else {
            LEARN_PERIOD
        }
    }

    /// Adds a range that is only read. Its frames are judged for CPU writes in flight; it may become a snapshot.
    fn watch_read_only(&mut self, addr: u64, size: u32) {
        if let Some(c) = self.cands.get_mut(&addr) {
            if c.ro && size > c.size {
                c.size = size;
                c.sample = None;
                c.hits = 0;
                c.learn = Learn::watch();
            }
        } else if self.cands.len() < MAX_CANDIDATES {
            self.fresh(addr);
            self.cands.insert(
                addr,
                Cand {
                    ro: true,
                    hits: 0,
                    cmask: Vec::new(),
                    gmask: Vec::new(),
                    grown: 0,
                    rest_until: 0,
                    size,
                    learn: Learn::watch(),
                    sample: None,
                    last: self.frame,
                    keys: Vec::new(),
                    derived: false,
                },
            );
        }
    }

    /// Adds the range, which a shader may write, as a candidate, or grows it. A snapshot that is written turns into
    /// `Never` at once.
    fn candidate(&mut self, addr: u64, size: u32) {
        if let Some(c) = self.cands.get_mut(&addr) {
            if c.learn.class() == Class::Snapshot {
                // A binding that is written is no snapshot, for any address it binds.
                for k in &c.keys {
                    if let Some(ki) = self.keys.get_mut(k) {
                        ki.class = Class::Never;
                    }
                }
                c.learn.set(Class::Never);
                c.ro = false;
                c.sample = None;
            } else if size > c.size || c.ro {
                c.size = size.max(c.size);
                c.ro = false;
                c.learn = Learn::new();
                c.sample = None;
            }
        } else if self.cands.len() < MAX_CANDIDATES {
            self.fresh(addr);
            self.cands.insert(
                addr,
                Cand {
                    ro: false,
                    hits: 0,
                    cmask: Vec::new(),
                    gmask: Vec::new(),
                    grown: 0,
                    rest_until: 0,
                    size,
                    learn: Learn::new(),
                    sample: None,
                    last: self.frame,
                    keys: Vec::new(),
                    derived: false,
                },
            );
        }
    }

    /// A snapshot or merge range that the GPU or a copy writes in a way that the class cannot carry is state: `Never`.
    /// Returns whether it was one.
    fn demote(&mut self, addr: u64) -> bool {
        match self.cands.get_mut(&addr) {
            Some(c) if matches!(c.learn.class(), Class::Snapshot | Class::Merge) => {
                c.learn.set(Class::Never);
                true
            }
            _ => false,
        }
    }

    /// True when the range has the class that `kind` needs.
    fn has_kind(&self, addr: u64, kind: Kind) -> bool {
        let want = match kind {
            Kind::Rename => Class::Rename,
            Kind::Snapshot => Class::Snapshot,
            Kind::Merge => Class::Merge,
            // A probe is judged by the class that it measures; it has no class of its own.
            Kind::Probe => return true,
        };
        self.cands
            .get(&addr)
            .is_some_and(|c| c.learn.class() == want)
    }

    /// True when the range is a snapshot now.
    #[cfg(test)]
    fn is_snapshot(&self, addr: u64) -> bool {
        self.has_kind(addr, Kind::Snapshot)
    }

    /// The flush of a frame. `bound` are its storage bindings, `others` is asked for only when a range is to be renamed,
    /// `read(addr, buf)` reads guest bytes.
    fn flush(
        &mut self,
        fence: u64,
        bound: &[(u64, u32)],
        others: &BlockedBy,
        written: &dyn Fn() -> Vec<(u64, u64)>,
        read: &dyn Fn(u64, &mut [u8]) -> bool,
    ) -> Flushed {
        self.frame += 1;
        let mut out = Flushed::default();
        let t_judge = self.watch.then(std::time::Instant::now);
        for addr in std::mem::take(&mut self.pending_news) {
            if let Some(c) = self.cands.get(&addr) {
                out.news
                    .push((addr, c.size, c.learn.class(), c.learn.frames()));
            }
        }
        let mut touched: Vec<u64> = bound
            .iter()
            .filter(|(a, s)| self.cands.get(a).is_some_and(|c| *s <= c.size))
            .map(|(a, _)| *a)
            .collect();
        touched.sort_unstable();
        touched.dedup();
        let now = self.frame;
        let learn_frame = now.is_multiple_of(self.period());
        // A sample is judged at the flush right after its frame or never: its `post` and `pre_next` must be neighbours.
        for c in self.cands.values_mut() {
            if c.sample.as_ref().is_some_and(|s| s.frame + 1 != now) {
                c.sample = None;
            }
        }
        if now.is_multiple_of(60) {
            // The game allocates some of its storage ranges from rings, so addresses come and go: a range that is not
            // bound any more is forgotten, the sooner the less is known about it.
            self.cands.retain(|_, c| {
                let age = now - c.last;
                c.sample.is_some()
                    || match c.learn.class() {
                        Class::Learning | Class::Idle | Class::Watch => age <= 120,
                        Class::Never => age <= 600,
                        Class::Rename | Class::Snapshot | Class::Merge => age <= 3000,
                        Class::Conflict => age <= 600,
                    }
            });
        }
        let mut renaming: Vec<(u64, u32, Kind)> = Vec::new();
        let mut budget = WATCH_BUDGET;
        let mut copy_dsts: Option<Vec<(u64, u64)>> = None;
        for addr in touched {
            let Some(c) = self.cands.get_mut(&addr) else {
                continue;
            };
            let class = c.learn.class();
            // A range that only waits to be judged (or has nothing to do) is looked at once in 60 flushes, to keep it from
            // being forgotten; the others are looked at in every flush.
            if matches!(class, Class::Never | Class::Watch)
                && !learn_frame
                && !now.is_multiple_of(60)
                && !(self.conflict_wait && !c.ro)
            {
                continue;
            }
            c.last = self.frame;
            c.learn.tick();
            if class == Class::Snapshot {
                // A copy or fill that writes the range makes it state.
                let dsts = copy_dsts.get_or_insert_with(written);
                let r = (addr, addr + u64::from(c.size));
                if dsts.iter().any(|&d| overlaps(r, d)) {
                    c.learn.set(Class::Never);
                    out.news
                        .push((addr, c.size, Class::Never, c.learn.frames()));
                } else {
                    renaming.push((addr, c.size, Kind::Snapshot));
                }
                continue;
            }
            if class == Class::Conflict
                || (self.conflict_wait
                    && !c.ro
                    && c.hits >= SNAPSHOT_HITS
                    && matches!(class, Class::Never | Class::Merge))
            {
                out.conflict = true;
            }
            // A range whose frames are judged for CPU writes in flight only: the Watch class and, to list the conflicts,
            // `Never` and `Idle` with `NXKIT_DIAG=rename`. The bytes that all of them read in a flush are limited.
            let in_flight = learn_frame
                && now >= c.rest_until
                && (matches!(class, Class::Watch | Class::Never | Class::Merge)
                    || (self.watch && class == Class::Idle))
                && (c.ro || probeable(addr, c.size) || class == Class::Idle)
                && u64::from(c.size) <= budget;
            let learn_judge = matches!(class, Class::Learning | Class::Rename);
            // Only a range that is judged in this flush is read; most bound ranges are not.
            let want = (c.sample.is_some() && learn_judge)
                || (learn_frame && c.learn.sampling())
                || in_flight;
            let mut pre = if want {
                vec![0u8; c.size as usize]
            } else {
                Vec::new()
            };
            let have = want && read(addr, &mut pre);
            if have {
                budget = budget.saturating_sub(u64::from(c.size));
            }
            if let Some(prev) = c.sample.take()
                && learn_judge
                && let Some(post) = prev.post
                && have
                && let Some(class) = c.learn.observe(judge(&prev.pre, &post, &pre))
            {
                out.news.push((addr, c.size, class, c.learn.frames()));
            }
            let judged_now = learn_frame && c.learn.sampling();
            if c.learn.class() == Class::Rename && !judged_now {
                renaming.push((addr, c.size, Kind::Rename));
            } else if c.learn.class() == Class::Merge && !in_flight {
                if probeable(addr, c.size) {
                    renaming.push((addr, c.size, Kind::Merge));
                }
            } else if (judged_now || in_flight) && have {
                c.sample = Some(Sample {
                    fence,
                    frame: now,
                    pre,
                    post: None,
                    atwait: None,
                });
                if !c.ro && matches!(c.learn.class(), Class::Never | Class::Merge) {
                    // A written range is judged on a copy (`Kind::Probe`); the flush says below whether it got one.
                    renaming.push((addr, c.size, Kind::Probe));
                } else if c.ro {
                    // A read-only range is measured when the wait begins; the frame needs no full wait.
                    out.watching = true;
                } else {
                    out.sampling = true;
                    out.checking |= c.learn.class() == Class::Rename;
                }
            }
        }
        if !out.news.is_empty() {
            self.last_event = now;
            let addrs: Vec<u64> = out.news.iter().map(|n| n.0).collect();
            for a in addrs {
                self.push_keys(a);
            }
        }
        if let Some(t) = t_judge {
            out.judge_ns = t.elapsed().as_nanos() as u64;
        }
        if renaming.is_empty() {
            return out;
        }
        let t_capture = self.watch.then(std::time::Instant::now);
        renaming.sort_unstable();
        let ranges: Vec<(u64, u32)> = renaming.iter().map(|&(a, s, _)| (a, s)).collect();
        let (accepted, left) = plan(&ranges, bound, &others(&ranges));
        out.excluded = left;
        let mut snapshot_budget = SNAPSHOT_BUDGET;
        let mut probed: Vec<u64> = Vec::new();
        for &i in &accepted {
            let (addr, size, kind) = renaming[i];
            if kind == Kind::Probe {
                probed.push(addr);
            }
            let snapshot = kind == Kind::Snapshot;
            if snapshot {
                if u64::from(size) > snapshot_budget {
                    out.over_budget += 1;
                    continue;
                }
                snapshot_budget -= u64::from(size);
            }
            if snapshot {
                out.snapshot_bytes += u64::from(size);
            }
            let words = (size as usize / 4).div_ceil(32);
            let mut x = Renamed {
                addr,
                len: size,
                start: Vec::new(),
                kind,
                cmask: Vec::new(),
                gmask: Vec::new(),
                rmask: Vec::new(),
                cvals: Vec::new(),
                cprefix: Vec::new(),
            };
            let ok = match kind {
                // A probe's copy is the guest range, filled on the GPU.
                Kind::Probe => {
                    x.cmask = vec![0; words];
                    x.cprefix = vec![0; words];
                    true
                }
                // A merge range needs only the words that the CPU writes in flight.
                Kind::Merge => match self.cands.get(&addr) {
                    Some(c) => {
                        let (g, cm) = (&c.gmask, &c.cmask);
                        let at = |m: &Vec<u32>, i: usize| m.get(i).copied().unwrap_or(0);
                        x.cmask = (0..words).map(|i| at(cm, i)).collect();
                        x.gmask = (0..words).map(|i| at(g, i) & !at(cm, i)).collect();
                        x.rmask = (0..words).map(|i| at(g, i) & at(cm, i)).collect();
                        match read_packed(read, addr, &x.cmask) {
                            Some((vals, prefix)) => {
                                x.cvals = vals;
                                x.cprefix = prefix;
                                true
                            }
                            None => false,
                        }
                    }
                    None => false,
                },
                _ => {
                    x.start = vec![0u8; size as usize];
                    read(addr, &mut x.start)
                }
            };
            if ok {
                out.renamed.push(x);
            } else {
                out.excluded += 1;
            }
        }
        // A probe that was not given a copy (another use of the range, the budget) judges nothing, and its frame needs
        // no full wait.
        for &(addr, _, kind) in &renaming {
            if kind == Kind::Probe
                && !probed.contains(&addr)
                && let Some(c) = self.cands.get_mut(&addr)
            {
                c.sample = None;
            }
        }
        out.sampling |= !probed.is_empty();
        out.renamed
            .retain(|x| x.kind != Kind::Probe || probed.contains(&x.addr));
        if let Some(t) = t_capture {
            out.capture_ns = t.elapsed().as_nanos() as u64;
        }
        out
    }

    /// The CPU waited and the GPU is done with the frames up to `done`; `newest` is the newest flushed fence. Reads the
    /// bytes after the frame of each sample that no later flush has overtaken.
    fn waited(&mut self, done: u64, newest: u64, read: &dyn Fn(u64, &mut [u8]) -> bool) {
        let now = self.frame;
        let mut found: Vec<(u64, Inflight, usize)> = Vec::new();
        let mut news: Vec<u64> = Vec::new();
        for (addr, c) in &mut self.cands {
            // A written range that is judged on a copy is measured in `probed`.
            if c.ro || matches!(c.learn.class(), Class::Never | Class::Merge) {
                continue;
            }
            if let Some(s) = &mut c.sample
                && s.post.is_none()
                && s.fence == newest
                && done >= s.fence
            {
                let mut post = vec![0u8; c.size as usize];
                if read(*addr, &mut post) {
                    if let Some(aw) = &s.atwait {
                        let cm = cpu_mask(&s.pre, aw);
                        let n = cm.iter().map(|w| w.count_ones() as usize).sum::<usize>();
                        found.push((
                            *addr,
                            Inflight {
                                size: c.size,
                                class: c.learn.class().name(),
                                ro: c.ro,
                                ..Inflight::default()
                            },
                            n,
                        ));
                        if n > 0 {
                            c.hits += 1;
                            // A range that shaders only read and the CPU writes while frames run is a snapshot.
                            if c.ro && c.learn.class() == Class::Watch && c.hits >= SNAPSHOT_HITS {
                                c.learn.set(Class::Snapshot);
                                news.push(*addr);
                            }
                        } else {
                            c.rest_until = now + WATCH_PAUSE;
                        }
                    }
                    s.post = Some(post);
                }
            }
        }
        self.pending_news.extend(news);
        self.classify();
        for (addr, f, n) in found {
            let e = self.inflight.entry(addr).or_default();
            e.size = f.size;
            e.class = f.class;
            e.ro = f.ro;
            e.frames += 1;
            if n > 0 {
                e.hit += 1;
                e.words = e.words.max(n);
            }
        }
    }

    /// A written range that the CPU writes in flight is a merge range (one class for all of them): the words that only
    /// the GPU writes (`P`) are merged on the GPU, the words that both write (`R`) are written back by the host in the
    /// wait. A merge range whose masks keep growing after it was classified is not understood: `Conflict`, the wait for
    /// its frame is a full one. What cannot be seen: an `R` word that the CPU writes in flight after the GPU wrote it in
    /// the same frame looks like one written before; its GPU value is dropped, as for a word that the CPU re-initialises.
    fn classify(&mut self) {
        let now = self.frame;
        let mut news: Vec<u64> = Vec::new();
        for (addr, c) in &mut self.cands {
            if c.ro
                || c.hits < SNAPSHOT_HITS
                || !matches!(c.learn.class(), Class::Never | Class::Merge)
            {
                continue;
            }
            if c.learn.class() == Class::Merge {
                if c.grown > MAX_GROWN {
                    c.learn.set(Class::Conflict);
                    note_n(Reason::OverlapMergeConflict, 1);
                    news.push(*addr);
                }
            } else if !is_empty_mask(&c.gmask) && probeable(*addr, c.size) {
                c.learn.set(Class::Merge);
                c.grown = 0;
                c.rest_until = now + WATCH_PAUSE;
                news.push(*addr);
            }
        }
        self.pending_news.extend(news);
    }

    /// The GPU is done with the judged frame `fence`, in which the written range `addr` read and wrote a copy: `end` is the
    /// copy after the frame and `start` the copy as the kernel filled it. The words that the GPU wrote are those where the
    /// two differ. The words that the CPU wrote are those where guest memory at the start of the wait differs from `pre`,
    /// except where it holds what the GPU wrote (the frame can end before the wait begins and its words go back into guest
    /// memory).
    fn probed(&mut self, fence: u64, addr: u64, end: &[u8], start: &[u8]) {
        let now = self.frame;
        let Some(c) = self.cands.get_mut(&addr) else {
            return;
        };
        let Some(s) = c.sample.as_ref().filter(|s| s.fence == fence) else {
            return;
        };
        let Some(aw) = &s.atwait else {
            return;
        };
        let gm = gpu_words(end, start);
        let mut cm = cpu_mask(&s.pre, aw);
        for (i, (a, e)) in aw.chunks(4).zip(end.chunks(4)).enumerate() {
            if gm[i / 32] >> (i % 32) & 1 != 0 && a == e {
                cm[i / 32] &= !(1 << (i % 32));
            }
        }
        let n = cm.iter().map(|w| w.count_ones() as usize).sum::<usize>();
        let grew = union_mask(&mut c.cmask, &cm) | union_mask(&mut c.gmask, &gm);
        if grew && c.learn.class() == Class::Merge {
            c.grown += 1;
        }
        if c.learn.class() == Class::Merge {
            // A merge range is looked at again after a while.
            c.rest_until = now + WATCH_PAUSE;
        }
        if n > 0 {
            c.hits += 1;
        } else {
            c.rest_until = now + WATCH_PAUSE;
        }
        let e = self.inflight.entry(addr).or_default();
        e.size = c.size;
        e.class = c.learn.class().name();
        e.ro = false;
        e.frames += 1;
        if n > 0 {
            e.hit += 1;
            e.words = e.words.max(n);
        }
        self.classify();
        self.push_keys(addr);
    }

    /// The wait for `fence` begins: takes the bytes of the ranges that are judged in that frame before the GPU is known to
    /// be done. A read-only range is measured now: the words that changed since the flush are the CPU's writes in flight,
    /// and the GPU cannot have written them.
    fn before_wait(&mut self, fence: u64, read: &dyn Fn(u64, &mut [u8]) -> bool) {
        let now = self.frame;
        let mut seen: Vec<(u64, usize)> = Vec::new();
        for (addr, c) in &mut self.cands {
            if let Some(s) = &mut c.sample
                && s.fence == fence
                && s.atwait.is_none()
            {
                let mut b = vec![0u8; c.size as usize];
                if read(*addr, &mut b) {
                    if c.ro {
                        let n = cpu_mask(&s.pre, &b)
                            .iter()
                            .map(|w| w.count_ones() as usize)
                            .sum();
                        seen.push((*addr, n));
                    }
                    s.atwait = Some(b);
                }
            }
        }
        let mut news = Vec::new();
        for (addr, n) in seen {
            let Some(c) = self.cands.get_mut(&addr) else {
                continue;
            };
            if n > 0 {
                c.hits += 1;
                // A range that shaders only read and the CPU writes while frames run is a snapshot.
                if c.learn.class() == Class::Watch && c.hits >= SNAPSHOT_HITS {
                    c.learn.set(Class::Snapshot);
                    news.push(addr);
                }
            } else {
                c.rest_until = now + WATCH_PAUSE;
            }
            c.sample = None;
            let e = self.inflight.entry(addr).or_default();
            e.size = c.size;
            e.class = c.learn.class().name();
            e.ro = true;
            e.frames += 1;
            if n > 0 {
                e.hit += 1;
                e.words = e.words.max(n);
            }
        }
        self.pending_news.extend(news);
    }

    /// The classes of the candidates: learning, rename, never, idle.
    fn classes(&self) -> [usize; 8] {
        let mut n = [0; 8];
        for c in self.cands.values() {
            n[c.learn.class() as usize] += 1;
        }
        n
    }

    fn waiting(&self) -> bool {
        self.cands
            .values()
            .any(|c| c.sample.as_ref().is_some_and(|s| s.post.is_none()))
    }
}

/// The frame of a batch slot whose renamed ranges wait to be written back.
struct Pending {
    fence: u64,
    buf: Arc<DataBuffer>,
    cap: Arc<Capture>,
    /// For each range of the capture, its offset in the buffer.
    offsets: Vec<(usize, u64)>,
    /// The probes of the frame: the range of the capture and the offsets of the copy and of the copy as it started.
    probes: Vec<(usize, u64, u64)>,
}

/// The last write-back of a range.
struct Chain {
    epoch: u64,
    wrote: Vec<Option<u32>>,
}

#[derive(Default)]
struct Slot {
    buf: Option<Arc<DataBuffer>>,
    pending: Option<Pending>,
}

/// The entry points that `NXKIT_DIAG=rename` times.
const COST_NAMES: [&str; 9] = [
    "at_flush",
    "judge",
    "capture",
    "note_rw",
    "begin_slot",
    "before_wait",
    "after_wait",
    "merge",
    "bindings",
];

/// The time of each entry point since the last report: sum, longest and number of calls.
#[derive(Default)]
struct Cost {
    sum: [AtomicU64; 9],
    max: [AtomicU64; 9],
    calls: [AtomicU64; 9],
}

impl Cost {
    fn add(&self, k: usize, ns: u64) {
        self.sum[k].fetch_add(ns, Ordering::Relaxed);
        self.max[k].fetch_max(ns, Ordering::Relaxed);
        self.calls[k].fetch_add(1, Ordering::Relaxed);
    }

    fn take(&self) -> String {
        let ms = |ns: u64| ns as f64 / 1e6;
        COST_NAMES
            .iter()
            .enumerate()
            .map(|(k, name)| {
                let (sum, max, n) = (
                    self.sum[k].swap(0, Ordering::Relaxed),
                    self.max[k].swap(0, Ordering::Relaxed),
                    self.calls[k].swap(0, Ordering::Relaxed),
                );
                format!(
                    "{name} {:.3}/{:.3} ({n})",
                    ms(sum) / n.max(1) as f64,
                    ms(max)
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// What `begin_slot` gives the encode of a frame.
pub(crate) struct Slotted {
    /// Where the bindings of renamed ranges read.
    pub(crate) map: RenameMap,
    /// The buffer of the slot, which the batch keeps alive.
    pub(crate) buf: Option<Arc<DataBuffer>>,
    /// The merge ranges and probes: capture, index in it, guest address, size, the real address of the copy and, for a
    /// probe, of the copy as it started (0 for a merge range).
    pub(crate) merges: Vec<(Arc<Capture>, usize, u64, u32, u64, u64)>,
}

/// The learner and the copies of the renamed ranges. One per [`crate::Gpu`], shared with the backend.
pub(crate) struct Renamer {
    ctx: Context,
    guest: GuestSpace,
    state: Mutex<State>,
    /// The slots, one per batch in flight.
    slots: Mutex<Vec<Slot>>,
    /// The fence whose wait must be honest because a range is judged in its frame, 0 for none.
    honest: AtomicU64,
    /// The fence whose wait begins with a look at the read-only ranges that are measured in its frame, 0 for none.
    watching: AtomicU64,
    /// Why the wait for `honest` must be honest: 1 a range is learned, 2 a renamed range is checked.
    honest_why: AtomicU64,
    /// Waits by kind: honest because of learning, of a check, of another reason, overlapped, honest because of a conflict.
    waits: [AtomicU64; 5],
    /// The CPU waits so far (`note_wait`).
    wait_epoch: AtomicU64,
    /// For each renamed range, the words that the last write-back wrote and the epoch of its frame, to carry a sum over
    /// frames that no CPU wait separates.
    chains: Mutex<FastMap<u64, Chain>>,
    /// Fences up to which the flushes print their capture because a range became `Rename` lately.
    young_until: AtomicU64,
    /// `NXKIT_DIAG=rename`: the time of the entry points.
    cost: Cost,
    /// A sample waits for its bytes or a frame waits for its write-back: a wait has work.
    active: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Renamer {
    pub(crate) fn new(ctx: &Context, guest: GuestSpace) -> Self {
        Self {
            ctx: ctx.clone(),
            guest,
            state: Mutex::new(State::default()),
            slots: Mutex::new((0..BATCHES_IN_FLIGHT).map(|_| Slot::default()).collect()),
            honest: AtomicU64::new(0),
            honest_why: AtomicU64::new(0),
            watching: AtomicU64::new(0),
            waits: Default::default(),
            young_until: AtomicU64::new(0),
            cost: Cost::default(),
            wait_epoch: AtomicU64::new(0),
            chains: Mutex::new(FastMap::default()),
            active: AtomicBool::new(false),
        }
    }

    /// True when the wait for `fence` must be a full one: a range is judged in this frame, and its `post` bytes are read
    /// when the GPU finished exactly this frame.
    pub(crate) fn honest(&self, fence: u64) -> bool {
        fence != 0 && self.honest.load(Ordering::Acquire) == fence
    }

    /// The game called a wait for a fence: a frame flushed after it has a different epoch from one flushed before.
    pub(crate) fn note_wait(&self) {
        self.wait_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// Counts a wait for the report of `NXKIT_DIAG=rename`: `kind` 0 honest because a range is learned, 1 because a range is
    /// checked, 2 honest for another reason, 3 overlapped.
    /// A wait for `fence` begins (`NXKIT_DIAG=rename`): in a frame whose ranges are judged, the bytes of the ranges are taken
    /// now, before the wait for the GPU, to find what the CPU wrote in flight.
    pub(crate) fn before_wait(&self, fence: u64) {
        self.timed(5, || self.before_wait_inner(fence));
    }

    fn before_wait_inner(&self, fence: u64) {
        if self.honest(fence) || (fence != 0 && self.watching.load(Ordering::Acquire) == fence) {
            lock(&self.state).before_wait(fence, &self.reader());
        }
    }

    pub(crate) fn count_wait(&self, fence: u64, overlapped: bool) {
        let k = if overlapped {
            3
        } else if self.honest(fence) {
            match self.honest_why.load(Ordering::Relaxed) {
                2 => 1,
                3 => 4,
                _ => 0,
            }
        } else {
            2
        };
        self.waits[k].fetch_add(1, Ordering::Relaxed);
    }

    /// Runs `f`, and with `NXKIT_DIAG=rename` adds its time to entry point `k`.
    fn timed<R>(&self, k: usize, f: impl FnOnce() -> R) -> R {
        if !crate::diag("rename") {
            return f();
        }
        let t = std::time::Instant::now();
        let r = f();
        self.cost.add(k, t.elapsed().as_nanos() as u64);
        r
    }

    /// Adds `ns` to entry point `k` of the cost report (the merge passes are encoded by the encoder).
    pub(crate) fn cost_add(&self, k: usize, ns: u64) {
        self.cost.add(k, ns);
    }

    fn reader(&self) -> impl Fn(u64, &mut [u8]) -> bool + '_ {
        let base = self.guest.address_map().gpu_base();
        move |addr, buf| addr >= base && self.guest.read(addr - base, buf).is_ok()
    }

    /// Looks at the storage bindings of an encoded frame and adds those that a shader may write (`rw` of the binding) and
    /// that are small to the candidates. The bindings of a task or mesh stage are not looked at.
    pub(crate) fn note_rw(&self, detail: &FrameDetail, programs: &Programs) {
        self.timed(3, || {
            self.note_rw_with(
                detail,
                detail.rename_check.as_deref(),
                &|key, vertex, slot| {
                    programs.stage(key, vertex).is_ok_and(|p| {
                        p.binds
                            .iter()
                            .any(|b| b.kind == BindKind::Sbuf && b.reg == slot && b.rw)
                    })
                },
            );
        });
    }

    /// [`Renamer::note_rw`] with `rw_of(program key, vertex, slot)` saying whether the program may write the binding.
    ///
    /// A range that is a written candidate already needs nothing, so most bindings cost one lookup. The answer for a
    /// program is cached by the allocation of its key (the walker shares one per key) and the slot.
    fn note_rw_with(
        &self,
        detail: &FrameDetail,
        check: Option<&[Recheck]>,
        rw_of: &dyn Fn(&str, bool, u32) -> bool,
    ) {
        use std::hash::BuildHasher;
        let mut st = lock(&self.state);
        st.watch = crate::diag("rename");
        if st.rw_ptr.len() > 50_000 {
            st.rw_ptr.clear();
        }
        if st.rw_str.len() > 50_000 {
            st.rw_str.clear();
        }
        // The same binding comes draw after draw.
        // A direct-mapped table of the bindings seen lately drops the repeats.
        let mut seen = vec![(0u64, 0u32, 0u32, 0usize); 4096];
        let mut look =
            |st: &mut State,
             key: Result<&Arc<str>, &str>,
             vertex: bool,
             s: &crate::detail::UboDetail| {
                let slot_stage = s.slot | (s.stage as u32) << 24;
                let id = match key {
                    Ok(k) => Arc::as_ptr(k).cast::<u8>() as usize,
                    Err(k) => k.as_ptr() as usize,
                };
                if s.size == 0 || s.size > SNAPSHOT_MAX_BYTES || s.address == 0 {
                    return;
                }
                let at = ((s.address >> 4) ^ (s.address >> 16) ^ id as u64 ^ u64::from(slot_stage))
                    as usize
                    & 4095;
                if seen[at] == (s.address, s.size, slot_stage, id) {
                    return;
                }
                seen[at] = (s.address, s.size, slot_stage, id);
                if st.cands.get(&s.address).is_some_and(|c| {
                    !c.ro && s.size <= c.size && c.learn.class() != Class::Snapshot
                }) {
                    return;
                }
                let rw = match key {
                    Ok(k) => {
                        let id = (Arc::as_ptr(k).cast::<u8>() as usize, slot_stage);
                        match st.rw_ptr.get(&id) {
                            Some((_, rw)) => *rw,
                            None => {
                                let rw = rw_of(k, vertex, s.slot);
                                // The cache keeps the key alive, so its address names it for good.
                                st.rw_ptr.insert(id, (k.clone(), rw));
                                rw
                            }
                        }
                    }
                    Err(k) => {
                        let id = (
                            foldhash::fast::FixedState::default().hash_one(k),
                            slot_stage,
                        );
                        *st.rw_str
                            .entry(id)
                            .or_insert_with(|| rw_of(k, vertex, s.slot))
                    }
                };
                if st.rw_known.len() > 100_000 {
                    st.rw_known.clear();
                }
                let prog = match key {
                    Ok(k) => st.prog_of(k),
                    Err(k) => foldhash::fast::FixedState::default().hash_one(k),
                };
                st.rw_known.insert(
                    Key {
                        prog,
                        slot_stage,
                        size: s.size.next_multiple_of(256),
                    },
                    rw,
                );
                if rw && s.size <= MAX_BYTES {
                    st.candidate(s.address, s.size);
                } else if rw {
                    // Too large to rename; a snapshot that is written is state.
                    st.demote(s.address);
                } else {
                    st.watch_read_only(s.address, s.size);
                }
            };
        let mut visit = |st: &mut State, kind: u8, i: usize, j: usize| {
            if kind == 0 {
                let Some(d) = detail.draws.get(i) else { return };
                if d.args.is_mesh() {
                    return;
                }
                let Some(s) = d.sbufs.get(j) else { return };
                match s.stage {
                    Stage::Vertex => look(st, Ok(&d.program.vertex), true, s),
                    Stage::Fragment => {
                        if let Some(k) = &d.program.fragment {
                            look(st, Ok(k), false, s);
                        }
                    }
                    _ => {}
                }
            } else if let Some(d) = detail.dispatches.get(i)
                && let Some(s) = d.sbufs.get(j)
                && s.stage == Stage::Compute
            {
                look(st, Err(&d.program), false, s);
            }
        };
        match check {
            // Only the bindings that the flush could not place.
            Some(list) => {
                for &(kind, i, j) in list {
                    visit(&mut st, kind, i as usize, j as usize);
                }
            }
            None => {
                for (i, d) in detail.draws.iter().enumerate() {
                    for j in 0..d.sbufs.len() {
                        visit(&mut st, 0, i, j);
                    }
                }
                for (i, d) in detail.dispatches.iter().enumerate() {
                    for j in 0..d.sbufs.len() {
                        visit(&mut st, 1, i, j);
                    }
                }
            }
        }
    }

    /// The flush of a frame, on the CPU thread. Takes the `pre` bytes of the ranges that are judged and the `start`
    /// bytes of those that this frame renames. Returns the capture of the renamed ranges, when there are any.
    pub(crate) fn at_flush(
        &self,
        detail: &FrameDetail,
        fence: u64,
    ) -> (Option<Capture>, Option<Vec<Recheck>>) {
        self.timed(0, || self.at_flush_inner(detail, fence))
    }

    fn at_flush_inner(
        &self,
        detail: &FrameDetail,
        fence: u64,
    ) -> (Option<Capture>, Option<Vec<Recheck>>) {
        let mut st = lock(&self.state);
        if st.cands.is_empty() && st.keys.is_empty() {
            self.honest.store(0, Ordering::Release);
            return (None, None);
        }
        let t_bind = crate::diag("rename").then(std::time::Instant::now);
        let (bindings, bound) = st.bindings(detail);
        let check = st.adopt(&bindings);
        if let Some(t) = t_bind {
            self.cost.add(8, t.elapsed().as_nanos() as u64);
        }
        st.watch = crate::diag("rename");
        st.conflict_wait = crate::knob("conflictwait");
        let read = self.reader();
        let f = st.flush(
            fence,
            &bound,
            &|ranges| blocked_flags(ranges, others(detail)),
            &|| copy_writes(detail),
            &read,
        );
        if st.watch {
            self.cost.add(1, f.judge_ns);
            if f.capture_ns != 0 {
                self.cost.add(2, f.capture_ns);
            }
        }
        self.watching
            .store(if f.watching { fence } else { 0 }, Ordering::Release);
        self.honest.store(
            if f.sampling || f.conflict { fence } else { 0 },
            Ordering::Release,
        );
        self.honest_why.store(
            if f.checking {
                2
            } else if f.sampling {
                1
            } else {
                3
            },
            Ordering::Relaxed,
        );
        if f.news.iter().any(|n| n.2 == Class::Rename) {
            self.young_until.store(fence + 20, Ordering::Relaxed);
        }
        if st.watch && st.frame.is_multiple_of(120) {
            let mut v: Vec<(&u64, &Inflight)> =
                st.inflight.iter().filter(|(_, e)| e.hit > 0).collect();
            v.sort_by_key(|(a, e)| (std::cmp::Reverse(e.hit), **a));
            for (addr, e) in v.iter().take(40) {
                eprintln!(
                    "DIAG rename cpu-in-flight addr {addr:#x} size {} class {}{} words {} in {} of {} judged frames",
                    e.size,
                    e.class,
                    if e.ro { " (read-only)" } else { "" },
                    e.words,
                    e.hit,
                    e.frames
                );
            }
        }
        if st.watch && st.frame.is_multiple_of(120) {
            let mut conflicts: Vec<(&u64, &Inflight)> = st
                .inflight
                .iter()
                .filter(|(_, e)| !e.ro && e.hit >= 2)
                .collect();
            conflicts.sort_by_key(|(a, e)| (std::cmp::Reverse(e.words), **a));
            for (addr, e) in conflicts.iter().take(40) {
                eprintln!(
                    "DIAG rename conflict addr {addr:#x} size {} words {} frames {} of {} judged",
                    e.size, e.words, e.hit, e.frames
                );
            }
            eprintln!(
                "DIAG rename cost ms per call (mean/max (calls)): {}; period {}",
                self.cost.take(),
                st.period()
            );
            let [l, r, n, i, wa, sn, mg, cf] = st.classes();
            let w: Vec<u64> = self
                .waits
                .iter()
                .map(|w| w.swap(0, Ordering::Relaxed))
                .collect();
            eprintln!(
                "DIAG rename waits honest {} (learning range {}, re-check {}, conflict {}, other {}) overlapped {}; learning {l} candidates {} (rename {r} never {n} idle {i} watch {wa} snapshot {sn} merge {mg} conflict {cf})",
                w[0] + w[1] + w[2] + w[4],
                w[0],
                w[1],
                w[4],
                w[2],
                w[3],
                st.cands.len()
            );
        }
        if f.sampling {
            self.active.store(true, Ordering::Release);
        }
        for (addr, size, class, frames) in &f.news {
            if !matches!(class, Class::Learning | Class::Idle | Class::Watch) {
                eprintln!(
                    "DIAG overlap rename {addr:#x} {size} {} after {frames} frames",
                    class.name()
                );
            }
        }
        if f.excluded != 0 {
            note_n(Reason::OverlapRenameExcluded, f.excluded as u64);
        }
        if f.over_budget != 0 {
            note_n(Reason::OverlapSnapshotOverBudget, f.over_budget as u64);
        }
        if f.snapshot_bytes != 0 {
            note_n(Reason::OverlapSnapshotBytes, f.snapshot_bytes);
        }
        if f.renamed.is_empty() {
            return (None, Some(check));
        }
        let sampled = crate::diag("rename")
            && (fence.is_multiple_of(60) || fence < self.young_until.load(Ordering::Relaxed));
        let capture = Capture {
            fence,
            sampled,
            epoch: self.wait_epoch.load(Ordering::Acquire),
            ranges: f
                .renamed
                .into_iter()
                .map(|x| CapRange {
                    addr: x.addr,
                    len: x.len,
                    start: x.start,
                    kind: x.kind,
                    cmask: x.cmask,
                    gmask: x.gmask,
                    rmask: x.rmask,
                    cvals: x.cvals,
                    cprefix: x.cprefix,
                })
                .collect(),
        };
        (Some(capture), Some(check))
    }

    /// The encode of a frame in batch slot `region`, after the batch ring waited for the slot. Writes back the frame that
    /// the slot held before, puts the bytes of the capture into the slot's buffer and returns where the bindings read
    /// them, and the buffer, which the batch must keep alive. A range that lies in a pool that raw global writes
    /// touched (`learned`) is not renamed.
    pub(crate) fn begin_slot(
        &self,
        region: usize,
        capture: Option<&Arc<Capture>>,
        learned: &[(u64, u64)],
    ) -> Slotted {
        self.timed(4, || self.begin_slot_inner(region, capture, learned))
    }

    fn begin_slot_inner(
        &self,
        region: usize,
        capture: Option<&Arc<Capture>>,
        learned: &[(u64, u64)],
    ) -> Slotted {
        let none = || Slotted {
            map: RenameMap::new(),
            buf: None,
            merges: Vec::new(),
        };
        let off = crate::knob("renameoff");
        let mut slots = lock(&self.slots);
        let Some(slot) = slots.get_mut(region) else {
            return none();
        };
        if let Some(p) = slot.pending.take() {
            self.write_back(&p, format_args!("begin_slot {region}"));
            self.report_probes(&p);
        }
        let Some(cap) = capture.filter(|_| !off) else {
            return none();
        };
        let mut left = 0;
        let mut offsets = Vec::new();
        let mut total = 0u64;
        for (i, r) in cap.ranges.iter().enumerate() {
            let end = r.addr + u64::from(r.len);
            if learned.iter().any(|&l| overlaps((r.addr, end), l)) {
                // A snapshot or merge range that raw global stores can reach is state.
                if r.kind != Kind::Rename {
                    lock(&self.state).demote(r.addr);
                }
                left += 1;
                continue;
            }
            // A snapshot or merge range that was found written since the flush (a shader of this frame writes it, or the
            // masks overlap now) is bound as before.
            if matches!(r.kind, Kind::Snapshot | Kind::Merge)
                && !lock(&self.state).has_kind(r.addr, r.kind)
            {
                left += 1;
                continue;
            }
            offsets.push((i, total));
            let size = u64::from(r.len).next_multiple_of(ALIGN);
            // A probe has a second copy, which keeps the state at the start of the frame.
            total += if r.kind == Kind::Probe {
                2 * size
            } else {
                size
            };
        }
        if left != 0 {
            note_n(Reason::OverlapRenameExcluded, left);
        }
        if offsets.is_empty() {
            return none();
        }
        if slot.buf.as_ref().is_none_or(|b| b.len() < total) {
            match DataBuffer::new(&self.ctx, total.next_power_of_two().max(4096)) {
                Ok(b) => slot.buf = Some(Arc::new(b)),
                Err(_) => return none(),
            }
        }
        let Some(buf) = slot.buf.clone() else {
            return none();
        };
        let mut entries = Vec::with_capacity(offsets.len());
        let mut merges = Vec::new();
        let mut probes = Vec::new();
        for &(i, off) in &offsets {
            let r = &cap.ranges[i];
            // Merge ranges and probes are filled on the GPU; the others take their bytes now.
            if !r.start.is_empty() && buf.write(off, &r.start).is_err() {
                return none();
            }
            let real = buf.gpu_address() + off;
            let size = r.len;
            entries.push((r.addr, size, real));
            match r.kind {
                Kind::Merge => merges.push((i, r.addr, size, real, 0)),
                Kind::Probe => {
                    let second = off + u64::from(size).next_multiple_of(ALIGN);
                    probes.push((i, off, second));
                    merges.push((i, r.addr, size, real, buf.gpu_address() + second));
                }
                _ => {}
            }
        }
        // The ranges whose words the host writes back in the wait: a renamed range, and the words that the CPU and the GPU
        // both write of a merge range.
        let offsets: Vec<(usize, u64)> = offsets
            .into_iter()
            .filter(|&(i, _)| {
                let r = &cap.ranges[i];
                r.kind == Kind::Rename || (r.kind == Kind::Merge && !is_empty_mask(&r.rmask))
            })
            .collect();
        let merge_count = merges.iter().filter(|m| m.4 == 0).count();
        note_n(Reason::OverlapRenamed, offsets.len() as u64);
        if merge_count != 0 {
            note_n(Reason::OverlapMergeRanges, merge_count as u64);
        }
        if cap.sampled {
            for &(i, _) in &offsets {
                let r = &cap.ranges[i];
                eprintln!(
                    "DIAG rename capture fence {} addr {:#x} size {} slot {region} {:?} words {}",
                    cap.fence,
                    r.addr,
                    r.len,
                    r.kind,
                    if r.kind == Kind::Rename {
                        nonzero_words(&r.start)
                    } else {
                        format!("{} of the CPU", r.cvals.len())
                    }
                );
            }
        }
        if !offsets.is_empty() || !probes.is_empty() {
            slot.pending = Some(Pending {
                fence: cap.fence,
                buf: buf.clone(),
                cap: cap.clone(),
                offsets,
                probes,
            });
            self.active.store(true, Ordering::Release);
        }
        Slotted {
            map: RenameMap::from_entries(entries),
            buf: Some(buf),
            merges: merges
                .into_iter()
                .map(|(idx, addr, size, slot_real, copy2)| {
                    (cap.clone(), idx, addr, size, slot_real, copy2)
                })
                .collect(),
        }
    }

    /// A CPU wait for fence `waited` returned. The GPU is done with the frames up to `done`, and `newest` is the newest
    /// flushed fence. Writes back the renamed ranges of the frames up to `waited` and reads the `post` bytes of the ranges
    /// that are judged in the frame `newest`.
    pub(crate) fn after_wait(
        &self,
        waited: u64,
        done: u64,
        newest: u64,
        who: std::fmt::Arguments<'_>,
    ) {
        self.timed(6, || self.after_wait_inner(waited, done, newest, who));
    }

    fn after_wait_inner(&self, waited: u64, done: u64, newest: u64, who: std::fmt::Arguments<'_>) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        {
            // In the order of the frames: a sum that no CPU wait separates carries from one frame to the next.
            let mut slots = lock(&self.slots);
            let mut due: Vec<Pending> = slots
                .iter_mut()
                .filter(|s| s.pending.as_ref().is_some_and(|p| p.fence <= waited))
                .filter_map(|s| s.pending.take())
                .collect();
            due.sort_unstable_by_key(|p| p.fence);
            for p in &due {
                self.write_back(p, format_args!("{who}"));
                self.report_probes(p);
            }
        }
        let mut st = lock(&self.state);
        st.waited(done, newest, &self.reader());
        let waiting = st.waiting();
        drop(st);
        let pending = lock(&self.slots).iter().any(|s| s.pending.is_some());
        self.active.store(waiting || pending, Ordering::Release);
    }

    /// The GPU is done with a frame: tells the learner what the probes of the frame saw.
    fn report_probes(&self, p: &Pending) {
        let mut seen = Vec::new();
        for &(i, off, second) in &p.probes {
            let r = &p.cap.ranges[i];
            let n = r.len as usize;
            let (mut end, mut start) = (vec![0u8; n], vec![0u8; n]);
            if p.buf.read(off, &mut end).is_ok() && p.buf.read(second, &mut start).is_ok() {
                seen.push((r.addr, end, start));
            }
        }
        if !seen.is_empty() {
            let mut st = lock(&self.state);
            for (addr, end, start) in seen {
                st.probed(p.cap.fence, addr, &end, &start);
            }
        }
    }

    /// Writes the changed words of the ranges of a frame into guest memory, each only if the guest word still holds the
    /// word that the frame started with.
    fn write_back(&self, p: &Pending, who: std::fmt::Arguments<'_>) {
        if crate::knob("renamenowb") {
            return;
        }
        let base = self.guest.address_map().gpu_base();
        let (mut written, mut skipped) = (0, 0);
        for &(i, off) in &p.offsets {
            let r = &p.cap.ranges[i];
            let mut end = vec![0u8; r.len as usize];
            if p.buf.read(off, &mut end).is_err() || r.addr < base {
                continue;
            }
            let at = r.addr - base;
            let mut shown = 0;
            let words = r.len as usize / 4;
            let mut chains = lock(&self.chains);
            let prev = chains.remove(&r.addr);
            let mut wrote: Vec<Option<u32>> = vec![None; words];
            let (mut w, mut sk) = (0, 0);
            for (i, cur, new) in written_words(r, &end) {
                let word = at + 4 * i as u64;
                let guest = self.guest.load_u32(word).unwrap_or(0);
                let before = prev
                    .as_ref()
                    .and_then(|c| c.wrote.get(i).copied().flatten());
                let same = prev.as_ref().is_some_and(|c| c.epoch == p.cap.epoch);
                let swap = decide_word(guest, cur, new, before, same);
                let done = swap.is_some_and(|v| {
                    self.guest
                        .compare_exchange_u32(word, guest, v)
                        .unwrap_or(false)
                });
                if done {
                    wrote[i] = swap;
                    w += 1;
                } else {
                    sk += 1;
                }
                if p.cap.sampled && shown < 24 {
                    shown += 1;
                    let what = if done {
                        if guest == cur {
                            "swapped"
                        } else if same {
                            "accumulated"
                        } else {
                            "replaced"
                        }
                    } else if guest == new {
                        "unchanged"
                    } else {
                        "skipped"
                    };
                    eprintln!(
                        "DIAG rename wb fence {} addr {:#x} word {i} start {cur:#x} end {new:#x} guest {guest:#x} -> {what} ({}) by {who} on {}",
                        p.cap.fence,
                        r.addr,
                        swap.map_or(String::from("-"), |v| format!("{v:#x}")),
                        std::thread::current().name().unwrap_or("?")
                    );
                }
            }
            chains.insert(
                r.addr,
                Chain {
                    epoch: p.cap.epoch,
                    wrote,
                },
            );
            written += w;
            skipped += sk;
        }
        note_n(Reason::OverlapWritebackWords, written);
        note_n(Reason::OverlapWritebackSkipped, skipped);
    }
}

/// The words of `bytes` that are not 0, as `i:hex`, at most 24.
fn nonzero_words(bytes: &[u8]) -> String {
    let v: Vec<String> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter_map(|(i, w)| {
            let w = u32::from_le_bytes(*w);
            (w != 0).then(|| format!("{i}:{w:#x}"))
        })
        .take(24)
        .collect();
    v.join(" ")
}

/// The ranges that copies and fills of the frame write.
fn copy_writes(detail: &FrameDetail) -> Vec<(u64, u64)> {
    use crate::detail::CopyKind;
    detail
        .copies
        .iter()
        .filter_map(|c| {
            let len = if c.bytes != 0 { c.bytes } else { UNKNOWN_BYTES };
            let a = match c.kind {
                CopyKind::BufferFill | CopyKind::BufferToBuffer => c.dst_addr,
                CopyKind::TextureToBuffer => c.buffer,
                _ => 0,
            };
            (a != 0).then_some((a, a.saturating_add(len)))
        })
        .collect()
}

/// The ranges that a frame reads or writes in ways other than a storage binding: uniform buffers, vertex, index and
/// indirect buffers, and the buffer sides of copies and fills.
fn others(detail: &FrameDetail) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = Vec::new();
    // A direct-mapped table of the uses seen lately drops the repeats: draw after draw binds the same buffers.
    let mut seen = vec![(0u64, 0u64); 4096];
    let mut add = |addr: u64, len: u64| {
        let slot = ((addr >> 4) ^ (addr >> 16) ^ len) as usize & 4095;
        if addr != 0 && seen[slot] != (addr, len) {
            seen[slot] = (addr, len);
            v.push((addr, addr.saturating_add(len.max(1))));
        }
    };
    for d in &detail.draws {
        for u in &d.ubos {
            add(u.address, u64::from(u.size));
        }
        let verts = match d.args {
            DrawArgs::Arrays { first, count, .. } => Some(u64::from(first) + u64::from(count)),
            _ => None,
        };
        for st in &d.streams {
            let used = verts.map_or(UNKNOWN_BYTES, |n| n * u64::from(st.stride.max(1)));
            add(st.address, st.size.min(used).min(UNKNOWN_BYTES));
        }
        match d.args {
            DrawArgs::Indexed {
                index_type,
                address,
                count,
                ..
            } => add(address, u64::from(count) << index_type.min(2)),
            DrawArgs::ArraysIndirect { indirect } | DrawArgs::MeshTasksIndirect { indirect } => {
                add(indirect, 32);
            }
            DrawArgs::IndexedIndirect {
                address, indirect, ..
            } => {
                add(address, UNKNOWN_BYTES);
                add(indirect, 32);
            }
            DrawArgs::MultiIndirectCount {
                index,
                indirect,
                count,
                max,
                stride,
            } => {
                if let Some((_, a)) = index {
                    add(a, UNKNOWN_BYTES);
                }
                add(indirect, u64::from(max).saturating_mul(stride.max(32)));
                add(count, 4);
            }
            _ => {}
        }
    }
    for d in &detail.dispatches {
        for u in &d.ubos {
            add(u.address, u64::from(u.size));
        }
        if let Some(a) = d.indirect {
            add(a, 12);
        }
    }
    for c in &detail.copies {
        let len = if c.bytes != 0 { c.bytes } else { UNKNOWN_BYTES };
        for a in [c.buffer, c.src_addr, c.dst_addr] {
            add(a, len);
        }
    }
    v
}

#[cfg(test)]
mod tests;
