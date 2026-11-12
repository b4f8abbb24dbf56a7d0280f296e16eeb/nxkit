//! Private GPU memory: step 1, tracking (`NXKIT_PRIVATE=track`), step 2, the mirror, and step 3, early fences
//! (`NXKIT_PRIVATE=1`; see `docs/nxkit-gpu.md`).
//!
//! The question of step 1 is what a private copy of guest memory for the GPU would cost, before any copy exists. At
//! the flush of each frame the host computes the pages the frame may write on the GPU, the would-be members of the
//! private set:
//!
//! - the storage bindings of draws and dispatches whose shader may write them (a program the encoder has not reflected
//!   yet counts as writing),
//! - the destinations of buffer copies and fills in the frame,
//! - the pools that shaders wrote through raw pointers (`Gmem::learned`).
//!
//! Member pages are protected read only (clean) by the page tracker of `nxkit-metal` ([`nxkit_metal::Pages`]), so a CPU
//! write to one faults once per frame and is counted. The pages that the new frame may write are protected against all
//! access until its batch completes; a CPU access in that time is a read or a write in flight, counted with its program
//! counter. Guest memory stays the truth: the handler only opens the page, nothing is copied, nothing waits and
//! nothing is redirected.
//!
//! Step 2 builds the mirror. With `NXKIT_PRIVATE=1` the members are the pages above plus every page of a binding that
//! touches a member (a uniform, vertex, index or indirect range, a copy source: a mirror binding must be contiguous and
//! current). The GPU reads and writes a private copy of them ([`nxkit_metal::Mirror`], the same offsets in a host
//! reservation, no-copy buffers over runs of 2 MiB segments) and never guest memory:
//!
//! - when a page becomes a member its guest bytes are copied into the mirror (the page is read only first);
//! - the pages the CPU wrote since the last flush (write faults on read only pages) are copied into a staging buffer at
//!   the flush, after the page went read only again, and a compute pass at the start of the frame's first command
//!   buffer copies them into the mirror, so the bytes for frame N + 1 land after frame N;
//! - the pages a frame writes lose all access at its flush and stay tagged: guest memory is stale. A CPU access
//!   waits for the batch if it still runs, then copies the page from the mirror and goes on (`nxkit-metal` `pages`);
//! - every site that turns a guest address into a GPU address finds the mirror through the overlay of the frame's pool
//!   snapshot ([`crate::overlay`], [`crate::pools::PoolSnapshot::resolve`]); the table of raw global memory is split at
//!   the member pages.
//!
//! Step 3 answers the waits for fences when the flush has run, not when the GPU is done (`GpuBackend::wait_early`): the
//! inputs of the frame are in the staging buffer and its outputs are guarded, so the CPU goes on and waits only where it
//! touches a page that a frame in flight may write. The flush is the boundary for the data the frame reads; the one
//! exception is a flush that ran submits before the CPU signals they wait for ([`late_expect`], [`late_stage`]). The overlap
//! and the renamer are off. Without the variable nothing here runs.
//!
//! Step 4 keeps the CPU from waiting for accesses that are not in the bytes a frame writes. A page that a frame writes knows
//! which 64-byte blocks the frames since its last copy may have written, and the fault handler of `nxkit-metal` does a load
//! or store outside those blocks itself, through a second mapping of guest memory. The stores reach the mirror at the next flush:
//! the page is staged with the mask of its stale blocks (`Stager::stage_masked`) and the copy-in pass skips those blocks
//! ([`CopyJob`]).
//!
//! Step 6 gives the CPU's reads of the GPU's results a signal of their own: the pages that game threads waited for as the
//! writer of a frame are learned, the encoder cuts the frame after the last pass that writes each ([`early_cuts`]), and the
//! command buffers up to the cut are committed on their own; when they are done the waiting thread copies the page and goes on,
//! while the frame runs to its end.
//!
//! The counters print every [`REPORT_EVERY`] frames (`NXKIT_DIAG=private`, also with the variable set).

use std::collections::VecDeque;
use std::hash::BuildHasher;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use nxkit_metal::{
    AccessForm, AddressMap, ArgumentTable, CommandBuffer, ComputeCommand, Context, DataBuffer,
    Dependency, FallbackReason, FaultKind, FaultOrigin, GuestMemory, Mirror, PageStats, Pages,
    Scope, Scratch, SnapWhy, Stager, Stages, SubmittedBatch, Visibility,
};

use crate::detail::{CopyKind, DrawArgs, FrameDetail, Stage, StreamDetail};
use crate::draws::FastMap;
use crate::overlay::{LateCopies, Overlay, PAGE, Run, Staged};
use crate::pipelines::Pipelines;
use crate::programs::{BindKind, Programs};
use crate::stats::Skip;

mod copyin;
mod early;
mod fences;
mod flush;
mod late;
mod members;
mod report;
#[cfg(test)]
mod tests;
mod track;

use fences::*;
use late::*;
use members::*;
use report::*;
use track::*;

pub(crate) use copyin::{
    CopyJob, copy_in_jobs, encode_copy_in, gpu_owned, late_copies_encoded, late_copy_jobs,
    mirror_covers, note_copy_in,
};
pub(crate) use early::{
    EARLY_CUTS, EarlyPlan, early_cuts, early_enabled, early_exclude, early_release, note_split,
};
pub(crate) use fences::encoded;
pub(crate) use flush::at_flush;
pub(crate) use late::{late_enabled, late_expect, late_signal, late_stage};
pub(crate) use track::{attach, note_learned, note_rw};

/// The overlay of a frame, over the Metal buffers of guest memory.
pub(crate) type GuestOverlay = Overlay<Arc<GuestMemory>>;

/// Frames between two reports.
pub(crate) const REPORT_EVERY: u64 = 300;
/// The largest single range that counts (a binding that is larger is a placeholder for "the whole buffer").
const RANGE_MAX_BYTES: u64 = 256 << 20;
/// The most bytes of member pages that one flush may add to the pages it protects against all access.
const FRAME_MAX_BYTES: u64 = 1 << 30;
/// The part of a buffer of unknown size that a copy touches.
const UNKNOWN_BYTES: u64 = 1 << 20;
/// The most bytes that the bindings which touch a member may add to the members in one flush.
const CLOSURE_MAX_BYTES: u64 = 256 << 20;
/// Program counters listed for each kind.
const TOP: usize = 10;
/// The pattern of the knob `privpoison`: a quiet float NaN.
const POISON: u32 = 0x7fc0_0000;

/// What `NXKIT_PRIVATE` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Not set.
    Off,
    /// `track`: step 1, the counters only.
    Track,
    /// `1`: steps 2 and 3, the mirror and the early fences.
    Mirror,
}

#[cfg(test)]
thread_local! {
    /// A test sets the mode for its own thread, so that tests of the mirror do not disturb the others.
    static FORCE: std::cell::Cell<Option<Mode>> = const { std::cell::Cell::new(None) };
    /// A test turns the poison on for its own thread.
    static FORCE_POISON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The mode of `NXKIT_PRIVATE`. Read once. Other values warn once and leave it off.
pub(crate) fn mode() -> Mode {
    #[cfg(test)]
    if let Some(m) = FORCE.with(std::cell::Cell::get) {
        return m;
    }
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("NXKIT_PRIVATE") {
        Ok(v) if v == "track" => Mode::Track,
        Ok(v) if v == "1" => Mode::Mirror,
        Ok(v) if !v.is_empty() && v != "0" => {
            eprintln!("nxkit-gpu: NXKIT_PRIVATE={v} is not a mode that exists, only `track` and `1` do; it stays off");
            Mode::Off
        }
        _ => Mode::Off,
    })
}

/// One test at a time may use the page tracker, which is one per process.
#[cfg(test)]
pub(crate) static TEST_TURN: Mutex<()> = Mutex::new(());

/// A test sets the mirror mode for its own thread (`None` clears it).
#[cfg(test)]
pub(crate) fn test_mode(m: Option<Mode>) {
    FORCE.with(|f| f.set(m));
    FAILED.store(false, Ordering::Relaxed);
}

/// True when the knob `privpoison` is on (a test can set it for its thread).
fn poison_on() -> bool {
    #[cfg(test)]
    if FORCE_POISON.with(std::cell::Cell::get) {
        return true;
    }
    crate::knob("privpoison")
}

/// True when the tracker runs, in either mode.
pub(crate) fn on() -> bool {
    mode() != Mode::Off
}

/// Set when the mirror could not be made: the run goes on without it.
static FAILED: AtomicBool = AtomicBool::new(false);

/// True when the GPU works on the mirror (`NXKIT_PRIVATE=1`, and the mirror exists).
pub(crate) fn mirrored() -> bool {
    mode() == Mode::Mirror && !FAILED.load(Ordering::Relaxed)
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the encoder learned of the programs: whether a shader may write the storage binding of a slot.
#[derive(Default)]
struct Rw {
    /// The hash of a program key, by the address of its allocation (the cache keeps the key alive, so the address names
    /// it for good).
    prog_hash: FastMap<usize, (Arc<str>, u64)>,
    /// `(program hash, slot | stage << 24)` to whether the binding is written.
    known: FastMap<(u64, u32), bool>,
}

fn hash_str(k: &str) -> u64 {
    foldhash::fast::FixedState::default().hash_one(k)
}

impl Rw {
    fn hash_of(&mut self, k: &Arc<str>) -> u64 {
        let id = Arc::as_ptr(k).cast::<u8>() as usize;
        if let Some((_, h)) = self.prog_hash.get(&id) {
            return *h;
        }
        if self.prog_hash.len() > 50_000 {
            self.prog_hash.clear();
        }
        let h = hash_str(k);
        self.prog_hash.insert(id, (k.clone(), h));
        h
    }

    /// Whether the binding may be written: unknown counts as written.
    fn written(&self, prog: Option<u64>, slot_stage: u32) -> bool {
        prog.is_none_or(|h| self.known.get(&(h, slot_stage)) != Some(&false))
    }
}

/// The counters of the window since the last report.
#[derive(Default)]
struct Window {
    /// Step 6: frames cut for pass signals, cuts, the position of the cuts (pass index plus one over passes, summed), pages
    /// the cuts release, frames whose first part was committed while the rest was encoded, readers known and not served,
    /// the time of the planning.
    early_frames: u64,
    early_cuts: u64,
    early_pos: f64,
    early_pages: u64,
    early_ahead: u64,
    early_readers: u64,
    early_unserved: u64,
    early_plan_ns: u64,
    flushes: u64,
    written_pages: u64,
    written_runs: u64,
    new_members: u64,
    dirtied: u64,
    flush_ns: u64,
    plan_ns: u64,
    mprotects: u64,
    clipped: u64,
    scanned: u64,
    /// Mirror mode: pages staged for the GPU, pages the CPU wrote again after they were staged, pages that the closure
    /// over the reads added, waits at the flush for frames in flight, pages the copy-in pass moved and pages it could not.
    staged: u64,
    raced: u64,
    closure_pages: u64,
    enter_waits: u64,
    copy_in_pages: u64,
    copy_in_lost: u64,
    /// Pages the knob `privlatestage` staged a second time.
    late_staged: u64,
    /// Frames whose flush ran submits before the CPU signals they wait for, the time the encodes waited for the signals and
    /// the waits that ended at the limit.
    late_frames: u64,
    late_wait_ns: u64,
    late_timeouts: u64,
    /// The first writes of the CPU to member pages between the flush of a frame and its commit, by what the frame binds:
    /// frames counted, pages (all, written by the frame, read by it through a binding, in no binding of it), and the
    /// frames whose flush ran submits before their CPU event signal with the pages of theirs that the frame reads.
    between_frames: u64,
    between_pages: u64,
    between_written: u64,
    between_reads: [u64; 6],
    between_same_page: u64,
    between_other: u64,
    between_samples: u64,
    /// At the commit, the bound read ranges of a frame whose bytes in a page written since the flush differ from what the
    /// frame sees (by kind), the bytes of them, and the written pages that could not be compared.
    changed_ranges: [u64; 6],
    changed_bytes: u64,
    uncompared: u64,
    /// First accesses to pages of a frame in flight by the source of the frame's right to write them (binding or copy, raw
    /// pool, no source, frame not kept) and read or write, and the pages touched most with how often.
    flight_by_source: [[u64; 2]; 4],
    flight_pages: Vec<(u64, u64, bool, String)>,
    forced_frames: u64,
    forced_reads: u64,
    /// The partial resolves (`overlay::partial_table`) at the start of the window.
    partial_base: [[u64; 3]; 7],
    /// `overlay::partial_repeats` at the start of the window.
    repeats_base: u64,
    /// Where the CPU time of the flush went, summed over the window (ns): the plan of the written ranges, the read ranges
    /// and the closure, the growth of the mirror, the flush of the pages (its parts are the next fields), the rest
    /// (overlay, staging bookkeeping), and the whole.
    t_plan_written: u64,
    t_reads: u64,
    t_closure: u64,
    t_ensure: u64,
    t_pages: u64,
    t_rest: u64,
    t_total: u64,
    /// The parts of the flush of the pages: new members, the log, staging the dirty pages, the writable pages, the tags.
    t_new: u64,
    t_log: u64,
    t_stage: u64,
    t_hot: u64,
    t_tag: u64,
    /// The time of `mprotect` in the flush of the pages.
    t_prot: u64,
    /// The writable pages: their number after each flush (summed), the ones staged, the unchanged ones, promotions and
    /// demotions.
    hot_pages: u64,
    hot_staged: u64,
    hot_same: u64,
    promoted: u64,
    demoted: u64,
    base: PageStats,
}

/// The mirror and what belongs to it.
struct MirrorState {
    ctx: Context,
    mirror: Mirror,
    /// The guest GPU address of the first byte of guest memory.
    va_base: u64,
    pool: StagePool,
    /// The fence of the newest flush.
    last_fence: u64,
}

struct Private {
    rw: Mutex<Rw>,
    /// The pools shaders wrote through raw pointers, as the encoder last saw them.
    learned: Mutex<Vec<(u64, u64)>>,
    window: Mutex<Window>,
    frames: std::sync::atomic::AtomicU64,
    peak_members: std::sync::atomic::AtomicU64,
    mirror: Mutex<Option<MirrorState>>,
    /// What each flushed frame binds, until its commit, to sort the writes between flush and commit (`NXKIT_DIAG=private`).
    sets: Mutex<VecDeque<Arc<FrameSets>>>,
}

/// The pages a flushed frame reads and writes through its bindings, and whether its flush ran submits before their CPU
/// event signals.
struct FrameSets {
    fence: u64,
    /// The host byte ranges `(start, end, kind)` that the frame reads through a binding, and the ranges it may write.
    reads: Vec<(u64, u64, u8)>,
    written: Vec<(u64, u64)>,
    /// The ranges the frame may write by where they come from: 0 a storage binding or a copy, 1 a pool that shaders wrote
    /// through raw pointers.
    sources: Vec<(u64, u64, u8)>,
    forced: u32,
    /// What the frame sees of guest memory: the staged bytes of the flush, else the mirror.
    view: Arc<GuestOverlay>,
}

/// A set of page runs `(first, end)`, sorted and disjoint.
#[derive(Default, Clone)]
struct PageRuns(Vec<(u32, u32)>);

/// The member pages as the closure sees them: the bitmap of the tracker and the pages added in this flush.
struct Members<'a> {
    bits: &'a [u64],
    added: PageRuns,
}

/// The staging buffers: one per frame in flight, taken back when the frame is complete. A buffer is never dropped by the
/// last holder of a frame (that may be a completion handler): the pool keeps its own reference until it takes it back.
#[derive(Default)]
struct StagePool {
    free: Vec<Arc<DataBuffer>>,
    busy: Vec<(u64, Arc<DataBuffer>)>,
}

/// Takes the pages that the flush hands over into a staging buffer.
struct Sink<'a> {
    ctx: &'a Context,
    pool: &'a mut StagePool,
    buf: Option<Arc<DataBuffer>>,
    cap: usize,
    slots: Vec<(u32, u32)>,
    index: FastMap<u32, u32>,
    /// The stale blocks of the pages staged with a mask.
    skips: FastMap<u32, [u64; 4]>,
    failed: bool,
}

fn state() -> &'static Private {
    static S: OnceLock<Private> = OnceLock::new();
    S.get_or_init(|| Private {
        rw: Mutex::new(Rw::default()),
        learned: Mutex::new(Vec::new()),
        window: Mutex::new(Window::default()),
        frames: std::sync::atomic::AtomicU64::new(0),
        peak_members: std::sync::atomic::AtomicU64::new(0),
        mirror: Mutex::new(None),
        sets: Mutex::new(VecDeque::new()),
    })
}

/// What the fence waits did, for the report (`NXKIT_PRIVATE=1`).
struct WaitCounters {
    early: AtomicU64,
    bounded: AtomicU64,
    bounded_ns: AtomicU64,
    bounded_max_ns: AtomicU64,
    full: [AtomicU64; 4],
}

static WAITS: WaitCounters = WaitCounters {
    early: AtomicU64::new(0),
    bounded: AtomicU64::new(0),
    bounded_ns: AtomicU64::new(0),
    bounded_max_ns: AtomicU64::new(0),
    full: [const { AtomicU64::new(0) }; 4],
};

/// Why a wait for a fence waited for the GPU in full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FullWait {
    /// The knob `privhonest`.
    Knob = 0,
    /// A frame that copied back to CPU memory came shortly before.
    CopyBack = 1,
    /// `nvnQueueFinish` or another wait for an idle GPU (also the drain of a frame that makes a page a member).
    Idle = 2,
    /// The flush of the fence did not finish capturing.
    NotCaptured = 3,
}

/// True when a wait for a fence returns once the flush ran, not when the GPU is done (`NXKIT_PRIVATE=1`).
pub(crate) fn early() -> bool {
    mirrored()
}

/// How many frames the CPU may run ahead of the GPU (`NXKIT_PRIVATE_AHEAD`, default 1, at most the batch ring): a wait for
/// fence N returns once the flush of N is captured and frame N - n is complete. 0 is the honest wait.
pub(crate) fn run_ahead() -> u64 {
    static N: OnceLock<u64> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("NXKIT_PRIVATE_AHEAD")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(1)
            .min(nxkit_metal::BATCHES_IN_FLIGHT as u64)
    })
}

/// True when the CPU sees the GPU's results one frame late (`NXKIT_PRIVATE_LATE`, default on; 0 turns it off and a CPU access
/// to bytes a frame in flight writes waits for the frame again).
pub(crate) fn late_results() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| !std::env::var("NXKIT_PRIVATE_LATE").is_ok_and(|v| v.trim() == "0"))
}

/// A fence wait was answered at the flush.
pub(crate) fn note_early() {
    WAITS.early.fetch_add(1, Ordering::Relaxed);
}

/// A fence wait that was answered at the flush also waited `d` for the frame [`nxkit_metal::BATCHES_IN_FLIGHT`] before.
pub(crate) fn note_bounded(d: std::time::Duration) {
    let ns = d.as_nanos() as u64;
    WAITS
        .bounded
        .fetch_add(u64::from(ns > 50_000), Ordering::Relaxed);
    WAITS.bounded_ns.fetch_add(ns, Ordering::Relaxed);
    WAITS.bounded_max_ns.fetch_max(ns, Ordering::Relaxed);
}

/// A wait waited for the GPU in full, for `why`.
pub(crate) fn note_full(why: FullWait) {
    if on() {
        WAITS.full[why as usize].fetch_add(1, Ordering::Relaxed);
    }
}

/// The newest frame whose batch the GPU finished.
pub(crate) fn completed() -> u64 {
    Pages::global().completed()
}
