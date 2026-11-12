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

mod capture;
mod copies;
mod judging;
mod keys;
mod learn;
mod renamer;
mod waits;
mod words;

use learn::*;
use renamer::*;
use words::*;

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
pub(crate) const SLOW_PERIOD: u64 = 120;
/// The largest range of a snapshot (a range that shaders only read and the CPU writes in flight).
pub(crate) const SNAPSHOT_MAX_BYTES: u32 = 1 << 20;
/// The bytes of all snapshots of one frame.
pub(crate) const SNAPSHOT_BUDGET: u64 = 8 << 20;
/// The bytes that the judged ranges of one flush may read.
const WATCH_BUDGET: u64 = 8 << 20;
/// Judged frames in which the masks of a merge range may grow before it is given up.
const MAX_GROWN: u32 = 3;
/// Judged frames in which the CPU wrote a read-only range in flight before it becomes a snapshot.
const SNAPSHOT_HITS: u32 = 1;
/// Frames that a range rests after a judged frame in which the CPU did not write it in flight.
const WATCH_PAUSE: u64 = 600;
/// Judged frames in a row without a CPU write in flight after which a written range is settled as `Never`.
pub(crate) const SETTLE_PROBES: u32 = 30;
/// Flushes after which a settled range is judged again.
const REPROBE: u64 = 120;
/// Flushes in a row in which a classified range passed the check against other uses before the check is skipped for it
/// (it is made again once in 60 flushes).
const CLEAR_RUN: u16 = 8;
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
    /// Judged frames in a row without a CPU write in flight.
    quiet: u32,
    /// Flushes in a row in which the range passed the check against other uses.
    clear_run: u16,
    /// The bindings that bound the range, up to [`MAX_KEYS`]: what it teaches and what teaches it.
    keys: Vec<Key>,
    /// The class comes from the keys of its bindings, not from judging the range.
    derived: bool,
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

/// The knobs that force a full wait for a frame, by the bit of `Flushed::forced`: the classes of the ranges that the frame
/// binds (0 to 4), changed buffers that the renamer does not cover (5) and every frame (6).
const FORCE_KNOBS: [&str; 7] = [
    "waitnever",
    "waitwatch",
    "waitmerge",
    "waitsnap",
    "waitrename",
    "waitother",
    "waitall",
];

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
    /// The classes of the ranges that this frame binds, as the bits of the knobs that ask for a full wait for them.
    forced: u8,
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
    /// The knobs `waitnever` to `waitrename`, by bit.
    force: u8,
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
    /// The knobs that forced the full wait for `honest`, by bit, and how many waits each forced.
    forced_mask: AtomicU64,
    forced: [AtomicU64; 7],
    /// The hash of each buffer of the last flush that `waitother` looks at.
    other_hashes: Mutex<FastMap<(u64, u64), u64>>,
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

#[cfg(test)]
mod tests;
