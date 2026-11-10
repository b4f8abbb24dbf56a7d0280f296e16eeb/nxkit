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
/// The largest range of a snapshot (a range that shaders only read and the CPU writes in flight).
pub(crate) const SNAPSHOT_MAX_BYTES: u32 = 1 << 20;
/// The bytes of all snapshots of one frame.
pub(crate) const SNAPSHOT_BUDGET: u64 = 8 << 20;
/// The bytes that the judged ranges of one flush may read.
const WATCH_BUDGET: u64 = 16 << 20;
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
/// - `others` are the bytes that the frame reads or writes in other ways (copies, vertex, index and indirect buffers,
///   uniform buffers) as `(start, end)`. A range that one of them overlaps is left as it is.
/// - A range that overlaps one that was already taken is left as it is.
///
/// Returns the indexes into `cands` that are renamed, and how many were left out.
pub(crate) fn plan(
    cands: &[(u64, u32)],
    bound: &[(u64, u32)],
    others: &[(u64, u64)],
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
            && !others.iter().any(|&o| overlaps(r, o))
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

/// The words that the GPU wrote after the wait began (`post` differs from `atwait`), one bit per word.
pub(crate) fn gpu_mask(post: &[u8], atwait: &[u8]) -> Vec<u32> {
    let mut m = vec![0u32; post.len().div_ceil(4).div_ceil(32)];
    for (i, (q, a)) in post.chunks(4).zip(atwait.chunks(4)).enumerate() {
        if q != a {
            m[i / 32] |= 1 << (i % 32);
        }
    }
    m
}

/// Adds the bits of `add` to `into`.
pub(crate) fn union_mask(into: &mut Vec<u32>, add: &[u32]) {
    if into.len() < add.len() {
        into.resize(add.len(), 0);
    }
    for (a, b) in into.iter_mut().zip(add) {
        *a |= b;
    }
}

/// True when a word is in both masks.
pub(crate) fn masks_overlap(a: &[u32], b: &[u32]) -> bool {
    a.iter().zip(b).any(|(x, y)| x & y != 0)
}

pub(crate) fn is_empty_mask(a: &[u32]) -> bool {
    a.iter().all(|w| *w == 0)
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
}

/// A renamed range of one frame with the guest bytes at its flush.
#[derive(Debug)]
pub(crate) struct CapRange {
    pub(crate) addr: u64,
    pub(crate) start: Vec<u8>,
    pub(crate) kind: Kind,
    /// For a merge range: the words that the CPU writes in flight, and the words that the GPU writes, one bit per word.
    pub(crate) cmask: Vec<u32>,
    pub(crate) gmask: Vec<u32>,
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
    /// The flush counter before which the range is not judged for CPU writes in flight again.
    rest_until: u64,
    size: u32,
    learn: Learn,
    sample: Option<Sample>,
    /// The flush counter at the last frame that bound the range.
    last: u64,
}

/// A range that a frame renames.
struct Renamed {
    addr: u64,
    start: Vec<u8>,
    kind: Kind,
    cmask: Vec<u32>,
    gmask: Vec<u32>,
}

/// One flush's result for the capture.
#[derive(Default)]
struct Flushed {
    /// The ranges to rename, with their bytes.
    renamed: Vec<Renamed>,
    /// A frame binds a range that needs a full wait.
    conflict: bool,
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
    /// The bindings whose program was looked at: address, size, slot and stage, hash of the program key.
    evaluated: FastSet<(u64, u32, u32, u64)>,
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
            self.cands.insert(
                addr,
                Cand {
                    ro: true,
                    hits: 0,
                    cmask: Vec::new(),
                    gmask: Vec::new(),
                    rest_until: 0,
                    size,
                    learn: Learn::watch(),
                    sample: None,
                    last: self.frame,
                },
            );
        }
    }

    /// Adds the range, which a shader may write, as a candidate, or grows it. A snapshot that is written turns into
    /// `Never` at once.
    fn candidate(&mut self, addr: u64, size: u32) {
        if let Some(c) = self.cands.get_mut(&addr) {
            if c.learn.class() == Class::Snapshot {
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
            self.cands.insert(
                addr,
                Cand {
                    ro: false,
                    hits: 0,
                    cmask: Vec::new(),
                    gmask: Vec::new(),
                    rest_until: 0,
                    size,
                    learn: Learn::new(),
                    sample: None,
                    last: self.frame,
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
        others: &dyn Fn() -> Vec<(u64, u64)>,
        written: &dyn Fn() -> Vec<(u64, u64)>,
        read: &dyn Fn(u64, &mut [u8]) -> bool,
    ) -> Flushed {
        self.frame += 1;
        let mut out = Flushed::default();
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
        let learn_frame = now.is_multiple_of(LEARN_PERIOD);
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
                        Class::Learning | Class::Idle | Class::Watch => age <= 60,
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
            c.last = self.frame;
            c.learn.tick();
            let class = c.learn.class();
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
                && u64::from(c.size) <= budget;
            let learn_judge = matches!(class, Class::Learning | Class::Rename);
            let mut pre = vec![0u8; c.size as usize];
            let have = ((c.sample.is_some() && learn_judge)
                || (learn_frame && c.learn.sampling())
                || in_flight)
                && read(addr, &mut pre);
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
                if c.size.is_multiple_of(4) && addr.is_multiple_of(4) {
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
                out.sampling = true;
                out.checking |= c.learn.class() == Class::Rename;
            }
        }
        if renaming.is_empty() {
            return out;
        }
        renaming.sort_unstable();
        let ranges: Vec<(u64, u32)> = renaming.iter().map(|&(a, s, _)| (a, s)).collect();
        let (accepted, left) = plan(&ranges, bound, &others());
        out.excluded = left;
        let mut snapshot_budget = SNAPSHOT_BUDGET;
        for i in accepted {
            let (addr, size, kind) = renaming[i];
            let snapshot = kind == Kind::Snapshot;
            if snapshot {
                if u64::from(size) > snapshot_budget {
                    out.over_budget += 1;
                    continue;
                }
                snapshot_budget -= u64::from(size);
            }
            let mut start = vec![0u8; size as usize];
            if read(addr, &mut start) {
                if snapshot {
                    out.snapshot_bytes += u64::from(size);
                }
                let (cmask, gmask) = match (kind, self.cands.get(&addr)) {
                    (Kind::Merge, Some(c)) => (c.cmask.clone(), c.gmask.clone()),
                    _ => (Vec::new(), Vec::new()),
                };
                out.renamed.push(Renamed {
                    addr,
                    start,
                    kind,
                    cmask,
                    gmask,
                });
            } else {
                out.excluded += 1;
            }
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
                        if !c.ro {
                            union_mask(&mut c.cmask, &cm);
                            union_mask(&mut c.gmask, &gpu_mask(&post, aw));
                        }
                        if c.learn.class() == Class::Merge {
                            // A merge range is looked at again after a while.
                            c.rest_until = now + WATCH_PAUSE;
                        }
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
        // A written range that the CPU writes in flight is a merge range when the words of the CPU and of the GPU are
        // disjoint, and one that needs a full wait when they are not.
        for (addr, c) in &mut self.cands {
            if c.ro
                || c.hits < SNAPSHOT_HITS
                || !matches!(c.learn.class(), Class::Never | Class::Merge)
            {
                continue;
            }
            if masks_overlap(&c.cmask, &c.gmask) {
                c.learn.set(Class::Conflict);
                note_n(Reason::OverlapMergeConflict, 1);
                news.push(*addr);
            } else if c.learn.class() == Class::Never
                && !is_empty_mask(&c.gmask)
                && c.size.is_multiple_of(4)
                && addr.is_multiple_of(4)
            {
                c.learn.set(Class::Merge);
                c.rest_until = now + WATCH_PAUSE;
                news.push(*addr);
            }
        }
        self.pending_news.extend(news);
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

    /// The wait for `fence` begins: takes the bytes of the ranges that are judged in that frame before the GPU is known to
    /// be done, for `NXKIT_DIAG=rename`.
    fn before_wait(&mut self, fence: u64, read: &dyn Fn(u64, &mut [u8]) -> bool) {
        for (addr, c) in &mut self.cands {
            if let Some(s) = &mut c.sample
                && s.fence == fence
                && s.atwait.is_none()
            {
                let mut b = vec![0u8; c.size as usize];
                if read(*addr, &mut b) {
                    s.atwait = Some(b);
                }
            }
        }
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

/// What `begin_slot` gives the encode of a frame.
pub(crate) struct Slotted {
    /// Where the bindings of renamed ranges read.
    pub(crate) map: RenameMap,
    /// The buffer of the slot, which the batch keeps alive.
    pub(crate) buf: Option<Arc<DataBuffer>>,
    /// The merge ranges: capture, index in it, guest address, size and the real address of the copy.
    pub(crate) merges: Vec<(Arc<Capture>, usize, u64, u32, u64)>,
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
            waits: Default::default(),
            young_until: AtomicU64::new(0),
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
        if self.honest(fence) {
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

    fn reader(&self) -> impl Fn(u64, &mut [u8]) -> bool + '_ {
        let base = self.guest.address_map().gpu_base();
        move |addr, buf| addr >= base && self.guest.read(addr - base, buf).is_ok()
    }

    /// Looks at the storage bindings of an encoded frame and adds those that a shader may write (`rw` of the binding) and
    /// that are small to the candidates. The bindings of a task or mesh stage are not looked at.
    pub(crate) fn note_rw(&self, detail: &FrameDetail, programs: &Programs) {
        use std::hash::BuildHasher;
        let mut st = lock(&self.state);
        st.watch = crate::diag("rename");
        if st.evaluated.len() > 100_000 {
            st.evaluated.clear();
        }
        let hash = |key: &str| foldhash::fast::FixedState::default().hash_one(key);
        let look = |st: &mut State, key: &str, vertex: bool, s: &crate::detail::UboDetail| {
            if s.size == 0 || s.size > SNAPSHOT_MAX_BYTES || s.address == 0 {
                return;
            }
            let id = (
                s.address,
                s.size,
                s.slot | (s.stage as u32) << 24,
                hash(key),
            );
            if !st.evaluated.insert(id) {
                return;
            }
            let rw = programs.stage(key, vertex).is_ok_and(|p| {
                p.binds
                    .iter()
                    .any(|b| b.kind == BindKind::Sbuf && b.reg == s.slot && b.rw)
            });
            if rw && s.size <= MAX_BYTES {
                st.candidate(s.address, s.size);
            } else if rw {
                // Too large to rename; a snapshot that is written is state.
                st.demote(s.address);
            } else {
                st.watch_read_only(s.address, s.size);
            }
        };
        for d in &detail.draws {
            if d.args.is_mesh() {
                continue;
            }
            for s in &d.sbufs {
                match s.stage {
                    Stage::Vertex => look(&mut st, &d.program.vertex, true, s),
                    Stage::Fragment => {
                        if let Some(k) = &d.program.fragment {
                            look(&mut st, k, false, s);
                        }
                    }
                    _ => {}
                }
            }
        }
        for d in &detail.dispatches {
            for s in d.sbufs.iter().filter(|s| s.stage == Stage::Compute) {
                look(&mut st, &d.program, false, s);
            }
        }
    }

    /// The flush of a frame, on the CPU thread. Takes the `pre` bytes of the ranges that are judged and the `start`
    /// bytes of those that this frame renames. Returns the capture of the renamed ranges, when there are any.
    pub(crate) fn at_flush(&self, detail: &FrameDetail, fence: u64) -> Option<Capture> {
        let mut st = lock(&self.state);
        if st.cands.is_empty() {
            self.honest.store(0, Ordering::Release);
            return None;
        }
        let bound = bound_sbufs(detail);
        st.watch = crate::diag("rename");
        st.conflict_wait = crate::knob("conflictwait");
        let read = self.reader();
        let f = st.flush(
            fence,
            &bound,
            &|| others(detail),
            &|| copy_writes(detail),
            &read,
        );
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
            return None;
        }
        let sampled = crate::diag("rename")
            && (fence.is_multiple_of(60) || fence < self.young_until.load(Ordering::Relaxed));
        Some(Capture {
            fence,
            sampled,
            epoch: self.wait_epoch.load(Ordering::Acquire),
            ranges: f
                .renamed
                .into_iter()
                .map(|x| CapRange {
                    addr: x.addr,
                    start: x.start,
                    kind: x.kind,
                    cmask: x.cmask,
                    gmask: x.gmask,
                })
                .collect(),
        })
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
        }
        let Some(cap) = capture.filter(|_| !off) else {
            return none();
        };
        let mut left = 0;
        let mut offsets = Vec::new();
        let mut total = 0u64;
        for (i, r) in cap.ranges.iter().enumerate() {
            let end = r.addr + r.start.len() as u64;
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
            if r.kind != Kind::Rename && !lock(&self.state).has_kind(r.addr, r.kind) {
                left += 1;
                continue;
            }
            offsets.push((i, total));
            total += (r.start.len() as u64).next_multiple_of(ALIGN);
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
        for &(i, off) in &offsets {
            let r = &cap.ranges[i];
            // A merge range is filled on the GPU; the bytes are the fallback if its kernel is not encoded.
            if buf.write(off, &r.start).is_err() {
                return none();
            }
            let real = buf.gpu_address() + off;
            entries.push((r.addr, r.start.len() as u32, real));
            if r.kind == Kind::Merge {
                merges.push((i, r.addr, r.start.len() as u32, real));
            }
        }
        let offsets: Vec<(usize, u64)> = offsets
            .into_iter()
            .filter(|&(i, _)| cap.ranges[i].kind == Kind::Rename)
            .collect();
        note_n(Reason::OverlapRenamed, offsets.len() as u64);
        if !merges.is_empty() {
            note_n(Reason::OverlapMergeRanges, merges.len() as u64);
        }
        if cap.sampled {
            for &(i, _) in &offsets {
                let r = &cap.ranges[i];
                eprintln!(
                    "DIAG rename capture fence {} addr {:#x} size {} slot {region} words {}",
                    cap.fence,
                    r.addr,
                    r.start.len(),
                    nonzero_words(&r.start)
                );
            }
        }
        if !offsets.is_empty() {
            slot.pending = Some(Pending {
                fence: cap.fence,
                buf: buf.clone(),
                cap: cap.clone(),
                offsets,
            });
            self.active.store(true, Ordering::Release);
        }
        Slotted {
            map: RenameMap::from_entries(entries),
            buf: Some(buf),
            merges: merges
                .into_iter()
                .map(|(idx, addr, size, slot_real)| (cap.clone(), idx, addr, size, slot_real))
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
            }
        }
        let mut st = lock(&self.state);
        st.waited(done, newest, &self.reader());
        let waiting = st.waiting();
        drop(st);
        let pending = lock(&self.slots).iter().any(|s| s.pending.is_some());
        self.active.store(waiting || pending, Ordering::Release);
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
            let mut end = vec![0u8; r.start.len()];
            if p.buf.read(off, &mut end).is_err() || r.addr < base {
                continue;
            }
            let at = r.addr - base;
            let mut shown = 0;
            let words = r.start.len() / 4;
            let mut chains = lock(&self.chains);
            let prev = chains.remove(&r.addr);
            let mut wrote: Vec<Option<u32>> = vec![None; words];
            let (mut w, mut sk) = (0, 0);
            for (i, cur, new) in changed_words(&r.start, &end) {
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

/// The storage bindings of a frame as `(address, size)`.
fn bound_sbufs(detail: &FrameDetail) -> Vec<(u64, u32)> {
    let mut v: Vec<(u64, u32)> = detail
        .draws
        .iter()
        .flat_map(|d| d.sbufs.iter())
        .chain(detail.dispatches.iter().flat_map(|d| d.sbufs.iter()))
        .filter(|s| s.address != 0 && s.size != 0)
        .map(|s| (s.address, s.size))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The ranges that a frame reads or writes in ways other than a storage binding: uniform buffers, vertex, index and
/// indirect buffers, and the buffer sides of copies and fills.
fn others(detail: &FrameDetail) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = Vec::new();
    let mut add = |addr: u64, len: u64| {
        if addr != 0 {
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
