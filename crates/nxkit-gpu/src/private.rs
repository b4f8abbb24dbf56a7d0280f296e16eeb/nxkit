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
    AccessForm, AddressMap, ArgumentTable, CommandBuffer, ComputeCommand, Context, DataBuffer, Dependency, FaultKind,
    FallbackReason, FaultOrigin, GuestMemory, Mirror, PageStats, Pages, Scope, Scratch, Stager, Stages, SubmittedBatch,
    Visibility,
};

use crate::detail::{CopyKind, DrawArgs, FrameDetail, Stage, StreamDetail};
use crate::draws::FastMap;
use crate::overlay::{Overlay, PAGE, Run, Staged};
use crate::pipelines::Pipelines;
use crate::programs::{BindKind, Programs};
use crate::stats::Skip;

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
    WAITS.bounded.fetch_add(u64::from(ns > 50_000), Ordering::Relaxed);
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

/// Names the guest memory to the page tracker, when the tracker is on, and makes the mirror in the mirror mode. Called
/// once, when the backend is made. `va_base` is the guest GPU address of the first byte.
pub(crate) fn attach(ctx: &Context, host_base: u64, len: u64, va_base: u64) {
    if !on() {
        if crate::diag("private") {
            eprintln!("DIAG private: NXKIT_PRIVATE is not set, nothing is tracked");
        }
        return;
    }
    let pages = Pages::global();
    pages.attach(host_base, len);
    if mode() == Mode::Mirror {
        match Mirror::new(ctx, len) {
            Ok(m) => {
                pages.set_mirror(m.host_base());
                let (no_eager, no_hot) = (crate::knob("privnoeager"), crate::knob("privnohot"));
                pages.set_adaptive(!no_eager, !no_hot);
                if no_eager || no_hot {
                    eprintln!(
                        "nxkit-gpu: NXKIT_PRIVATE=1: adaptive page handling off: copy back after completion {}, writable hot pages {}",
                        if no_eager { "off (privnoeager)" } else { "on" },
                        if no_hot { "off (privnohot)" } else { "on" }
                    );
                }
                match pages.make_alias() {
                    Ok(()) => {
                        let off = crate::knob("privnoemu");
                        pages.set_emulation(!off);
                        if !off && late_results() {
                            pages.set_late(true);
                            eprintln!(
                                "nxkit-gpu: NXKIT_PRIVATE=1: the CPU sees the GPU's results one frame late: a game thread's access to a \
                                 page that a frame in flight writes is served from guest memory, which holds the last completed frame's \
                                 bytes, without a wait (NXKIT_PRIVATE_LATE=0 turns it off)"
                            );
                        }
                        eprintln!(
                            "nxkit-gpu: NXKIT_PRIVATE=1: accesses of the CPU to a page that a frame writes, outside the bytes it \
                             writes, are done by the fault handler through a second mapping of guest memory{}",
                            if off { " (off: the knob privnoemu)" } else { "" }
                        );
                    }
                    Err(e) => eprintln!(
                        "nxkit-gpu: NXKIT_PRIVATE=1: no second mapping of guest memory ({e}); every CPU access to a page that a \
                         frame writes waits for the frame"
                    ),
                }
                *locked(&state().mirror) = Some(MirrorState {
                    ctx: ctx.clone(),
                    mirror: m,
                    va_base,
                    pool: StagePool::default(),
                    last_fence: 0,
                });
                eprintln!(
                    "nxkit-gpu: NXKIT_PRIVATE=1, the GPU works on a mirror of {} MiB of guest memory in pages of {} KiB; \
                     the overlap and the renamer are off",
                    len >> 20,
                    pages.page_size() >> 10
                );
            }
            Err(e) => {
                FAILED.store(true, Ordering::Relaxed);
                eprintln!("nxkit-gpu: NXKIT_PRIVATE=1: no mirror ({e}); the run goes on without it");
            }
        }
        return;
    }
    eprintln!(
        "nxkit-gpu: NXKIT_PRIVATE=track, tracking {} MiB of guest memory in pages of {} KiB",
        len >> 20,
        pages.page_size() >> 10
    );
}

/// The encoder tells which storage bindings of the programs of `detail` a shader writes. A program that was not
/// reflected yet is left out and counts as writing.
pub(crate) fn note_rw(detail: &FrameDetail, programs: &Programs) {
    if !on() {
        return;
    }
    let mut rw = locked(&state().rw);
    let rw = &mut *rw;
    let learn = |rw: &mut Rw, prog: u64, key: &str, vertex: bool, slot: u32, slot_stage: u32| {
        if rw.known.contains_key(&(prog, slot_stage)) {
            return;
        }
        if rw.known.len() > 100_000 {
            rw.known.clear();
        }
        if let Ok(p) = programs.stage(key, vertex) {
            let written = p
                .binds
                .iter()
                .any(|b| b.kind == BindKind::Sbuf && b.reg == slot && b.rw);
            rw.known.insert((prog, slot_stage), written);
        }
    };
    // Draws that share their programs bind the same slots: one look at each pair of programs is enough.
    let mut seen = vec![(0usize, 0usize); 1024];
    for d in &detail.draws {
        if d.args.is_mesh() || d.sbufs.is_empty() {
            continue;
        }
        let v = Arc::as_ptr(&d.program.vertex).cast::<u8>() as usize;
        let f = d
            .program
            .fragment
            .as_ref()
            .map_or(0, |f| Arc::as_ptr(f).cast::<u8>() as usize);
        let at = (v >> 4 ^ f >> 5) & 1023;
        if seen[at] == (v, f) {
            continue;
        }
        seen[at] = (v, f);
        for s in &d.sbufs {
            let slot_stage = s.slot | (s.stage as u32) << 24;
            match s.stage {
                Stage::Vertex => {
                    let h = rw.hash_of(&d.program.vertex);
                    learn(rw, h, &d.program.vertex, true, s.slot, slot_stage);
                }
                Stage::Fragment => {
                    if let Some(k) = &d.program.fragment {
                        let h = rw.hash_of(k);
                        learn(rw, h, k, false, s.slot, slot_stage);
                    }
                }
                _ => {}
            }
        }
    }
    let mut last: Option<*const u8> = None;
    for d in &detail.dispatches {
        if last == Some(d.program.as_ptr()) {
            continue;
        }
        last = Some(d.program.as_ptr());
        let h = hash_str(&d.program);
        for s in &d.sbufs {
            if s.stage == Stage::Compute {
                learn(rw, h, &d.program, false, s.slot, s.slot | (s.stage as u32) << 24);
            }
        }
    }
}

/// The encoder tells which pools shaders wrote through raw pointers (the table only grows).
pub(crate) fn note_learned(learned: &[(u64, u64)]) {
    if !on() {
        return;
    }
    let mut l = locked(&state().learned);
    if l.len() != learned.len() {
        *l = learned.to_vec();
    }
}

/// The ranges (GPU address, length) that the frame may write.
fn written_ranges(detail: &FrameDetail, learned: &[(u64, u64)], rw: &Rw) -> Vec<(u64, u64)> {
    let mut seen = vec![(0u64, 0u32, 0u32, 0u64); 4096];
    let mut out = written_in(
        detail,
        rw,
        0..detail.draws.len(),
        0..detail.dispatches.len(),
        0..detail.copies.len(),
        &mut seen,
    );
    out.extend(learned.iter().map(|&(lo, hi)| (lo, hi.saturating_sub(lo))));
    out
}

/// The ranges (GPU address, length) that the draws, dispatches and copies in the given index ranges of `detail` may write
/// (not the pools that shaders wrote through raw pointers). `seen` is a direct-mapped table of the bindings seen lately: draws
/// after draws bind the same buffers, and it drops the repeats; a power of two long.
fn written_in(
    detail: &FrameDetail,
    rw: &Rw,
    draws: std::ops::Range<usize>,
    dispatches: std::ops::Range<usize>,
    copies: std::ops::Range<usize>,
    seen: &mut [(u64, u32, u32, u64)],
) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mask = seen.len() - 1;
    let mut add = |s: &crate::detail::UboDetail, prog: Option<u64>| {
        if s.address == 0 || s.size == 0 {
            return;
        }
        let slot_stage = s.slot | (s.stage as u32) << 24;
        if !rw.written(prog, slot_stage) {
            return;
        }
        let p = prog.unwrap_or(0);
        let at = ((s.address >> 4) ^ (s.address >> 16) ^ p ^ u64::from(slot_stage)) as usize & mask;
        if seen[at] == (s.address, s.size, slot_stage, p) {
            return;
        }
        seen[at] = (s.address, s.size, slot_stage, p);
        out.push((s.address, u64::from(s.size)));
    };
    // The hash of the last program key of each stage, by its allocation (no lock: the cache is read only here).
    let hash = |k: &Arc<str>| {
        rw.prog_hash
            .get(&(Arc::as_ptr(k).cast::<u8>() as usize))
            .map(|(_, h)| *h)
    };
    let mut prev_ptr = 0usize;
    for d in detail.draws.get(draws).unwrap_or(&[]) {
        if d.sbufs.is_empty() {
            continue;
        }
        let ptr = Arc::as_ptr(d) as usize;
        if ptr == prev_ptr {
            continue;
        }
        prev_ptr = ptr;
        let mesh = d.args.is_mesh();
        for s in &d.sbufs {
            let prog = if mesh {
                None
            } else {
                match s.stage {
                    Stage::Vertex => hash(&d.program.vertex),
                    Stage::Fragment => d.program.fragment.as_ref().and_then(hash),
                    _ => None,
                }
            };
            add(s, prog);
        }
    }
    for d in detail.dispatches.get(dispatches).unwrap_or(&[]) {
        let h = hash_str(&d.program);
        for s in &d.sbufs {
            if s.stage == Stage::Compute {
                add(s, Some(h));
            } else {
                add(s, None);
            }
        }
    }
    for c in detail.copies.get(copies).unwrap_or(&[]) {
        let (a, len) = match c.kind {
            CopyKind::BufferFill | CopyKind::BufferToBuffer => (c.dst_addr, c.bytes),
            CopyKind::TextureToBuffer => (c.buffer, 0),
            _ => (0, 0),
        };
        if a != 0 {
            out.push((a, if len != 0 { len } else { UNKNOWN_BYTES }));
        }
    }
    out
}

/// The host ranges (address, length) that the frame may write, with the limits of one range and of the frame applied, and
/// how many ranges were left out for them.
fn plan_written(
    detail: &FrameDetail,
    learned: &[(u64, u64)],
    rw: &Rw,
    map: &AddressMap,
) -> (Vec<(u64, u64)>, u64) {
    let mut gpu = written_ranges(detail, learned, rw);
    gpu.sort_unstable();
    gpu.dedup();
    let mut host: Vec<(u64, u64)> = Vec::with_capacity(gpu.len());
    let (mut clipped, mut bytes) = (0u64, 0u64);
    for (a, len) in gpu {
        let len = len.max(1);
        if len > RANGE_MAX_BYTES || bytes + len > FRAME_MAX_BYTES {
            clipped += 1;
            continue;
        }
        if let Ok(h) = map.host_address(a, len) {
            bytes += len;
            host.push((h, len));
        }
    }
    (host, clipped)
}

// ---------------------------------------------------------------------------------------------------------------------
// Step 6: pass signals for the pages that the CPU reads from the GPU's results.

/// How many extra command buffers a frame is cut into at most, for pass signals.
pub(crate) const EARLY_CUTS: usize = 3;

/// A cut of a frame: after pass `after_pass` the command buffers so far go to the GPU on their own, and when they are done the
/// writer of `pages` (runs of page numbers) is done too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EarlyCut {
    pub(crate) after_pass: usize,
    pub(crate) pages: Vec<(u32, u32)>,
}

/// What [`early_cuts`] found for a frame.
#[derive(Default)]
pub(crate) struct EarlyPlan {
    pub(crate) cuts: Vec<EarlyCut>,
    /// The CPU-read GPU results the planner knew, and how many of them no cut serves (a pass writes them through a pool that
    /// shaders wrote through raw pointers, or no pass of the frame writes them).
    pub(crate) readers: usize,
    pub(crate) blocked: usize,
}

/// True when frames are cut for pass signals (`NXKIT_PRIVATE=1`; the knob `privnosplit` turns it off).
pub(crate) fn early_enabled() -> bool {
    mirrored() && !crate::knob("privnosplit")
}

/// Page numbers to runs.
fn runs_of_pages(pages: &[u32]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &p in pages {
        match out.last_mut() {
            Some(last) if last.1 == p => last.1 = p + 1,
            _ => out.push((p, p + 1)),
        }
    }
    out
}

/// The cuts for `readers` (sorted page numbers): `pass_runs[i]` are the page runs that pass `i` may write, `blocked` the runs
/// that some pass writes without saying which (pools that shaders wrote through raw pointers) or that no pass was found for.
/// A reader goes to the cut after the last pass that writes its page. At most `max_cuts` cuts come out: the earliest
/// `max_cuts - 1` and the last one, the pages of the others join the next cut after them. A cut after the last pass is no
/// cut. Returns the cuts and how many readers no cut serves.
pub(crate) fn plan_cuts(
    readers: &[u32],
    pass_runs: &[Vec<(u32, u32)>],
    blocked: &[(u32, u32)],
    max_cuts: usize,
) -> (Vec<EarlyCut>, usize) {
    let mut last: Vec<Option<usize>> = vec![None; readers.len()];
    let mut stop = vec![false; readers.len()];
    let span = |runs: &[(u32, u32)], f: &mut dyn FnMut(usize)| {
        for &(a, b) in runs {
            let lo = readers.partition_point(|&p| p < a);
            let hi = readers.partition_point(|&p| p < b);
            (lo..hi).for_each(&mut *f);
        }
    };
    for (i, runs) in pass_runs.iter().enumerate() {
        span(runs, &mut |k| last[k] = Some(i));
    }
    span(blocked, &mut |k| stop[k] = true);
    let mut by_pass: std::collections::BTreeMap<usize, Vec<u32>> = std::collections::BTreeMap::new();
    let mut unserved = 0;
    for (k, &page) in readers.iter().enumerate() {
        match last[k] {
            Some(i) if !stop[k] && i + 1 < pass_runs.len() => by_pass.entry(i).or_default().push(page),
            _ => unserved += 1,
        }
    }
    let keys: Vec<usize> = by_pass.keys().copied().collect();
    let keep = max_cuts.max(1);
    let mut cuts: Vec<EarlyCut> = Vec::new();
    let mut pending: Vec<u32> = Vec::new();
    for (n, key) in keys.iter().enumerate() {
        pending.extend(by_pass.remove(key).unwrap_or_default());
        if n + 1 < keep || n + 1 == keys.len() {
            pending.sort_unstable();
            cuts.push(EarlyCut {
                after_pass: *key,
                pages: runs_of_pages(&pending),
            });
            pending.clear();
        }
    }
    (cuts, unserved)
}

/// The cuts of the frame of `fence`: finds the CPU-read GPU results that game threads waited for lately
/// ([`Pages::readers`]), the last pass of the frame that may write each, and the cuts after those passes.
pub(crate) fn early_cuts(
    frame: &nxkit_nvn::frame::Frame,
    plan: &nxkit_nvn::passes::PassPlan,
    detail: &FrameDetail,
    fence: u64,
    map: &AddressMap,
    learned: &[(u64, u64)],
) -> EarlyPlan {
    use nxkit_nvn::passes::PassKind;
    let pages = Pages::global();
    let readers = pages.readers(fence);
    if readers.is_empty() || plan.passes.len() < 2 {
        return EarlyPlan::default();
    }
    let t0 = Instant::now();
    let ords = crate::draws::ordinals_with_end(frame);
    let runs_of_ranges = |ranges: &[(u64, u64)]| -> Vec<(u32, u32)> {
        ranges
            .iter()
            .filter(|&&(_, len)| len <= RANGE_MAX_BYTES)
            .filter_map(|&(a, len)| {
                let len = len.max(1);
                let h = map.host_address(a, len).ok()?;
                pages.page_range(h, len)
            })
            .collect()
    };
    let rw = locked(&state().rw);
    let mut seen = [(0u64, 0u32, 0u32, 0u64); 64];
    let mut attributed: Vec<(u32, u32)> = Vec::new();
    let mut pass_runs: Vec<Vec<(u32, u32)>> = Vec::with_capacity(plan.passes.len());
    for pass in &plan.passes {
        if !matches!(pass.kind, PassKind::Render | PassKind::Compute | PassKind::Copy) {
            pass_runs.push(Vec::new());
            continue;
        }
        let (Some(lo), Some(hi)) = (ords.get(pass.cmds.start as usize), ords.get(pass.cmds.end as usize)) else {
            // A pass that names commands outside the frame: nothing can be said about it.
            return EarlyPlan {
                readers: readers.len(),
                blocked: readers.len(),
                ..Default::default()
            };
        };
        seen.fill((0, 0, 0, 0));
        let ranges = written_in(
            detail,
            &rw,
            lo.draw as usize..hi.draw as usize,
            lo.dispatch as usize..hi.dispatch as usize,
            lo.copy as usize..hi.copy as usize,
            &mut seen,
        );
        let runs = runs_of_ranges(&ranges);
        attributed.extend_from_slice(&runs);
        pass_runs.push(runs);
    }
    // What the flush counts as written, whatever the passes say: the pools that shaders wrote through raw pointers can be
    // written by any pass, and a range that no pass was found for belongs to no cut.
    let mut seen = vec![(0u64, 0u32, 0u32, 0u64); 4096];
    let whole = written_in(
        detail,
        &rw,
        0..detail.draws.len(),
        0..detail.dispatches.len(),
        0..detail.copies.len(),
        &mut seen,
    );
    drop(rw);
    let mut blocked = runs_of_ranges(&learned.iter().map(|&(lo, hi)| (lo, hi.saturating_sub(lo))).collect::<Vec<_>>());
    attributed.sort_unstable();
    let known = |p: u32| {
        let i = attributed.partition_point(|&(_, e)| e <= p);
        attributed.get(i).is_some_and(|&(s, _)| s <= p)
    };
    for (a, b) in runs_of_ranges(&whole) {
        let mut p = a;
        while p < b {
            if !known(p) {
                let from = p;
                while p < b && !known(p) {
                    p += 1;
                }
                blocked.push((from, p));
            } else {
                p += 1;
            }
        }
    }
    blocked.sort_unstable();
    let (cuts, unserved) = plan_cuts(&readers, &pass_runs, &blocked, EARLY_CUTS);
    let mut w = locked(&state().window);
    w.early_plan_ns += t0.elapsed().as_nanos() as u64;
    EarlyPlan {
        cuts,
        readers: readers.len(),
        blocked: unserved,
    }
}

/// Removes the pages of the GPU address ranges `excluded` from the cuts (they are written in the last command buffer, by the
/// copy-back of textures), and drops the cuts that serve no page any more.
pub(crate) fn early_exclude(cuts: &mut Vec<EarlyCut>, excluded: &[(u64, u64)], map: &AddressMap) {
    if excluded.is_empty() {
        return;
    }
    let pages = Pages::global();
    let mut ex: Vec<(u32, u32)> = excluded
        .iter()
        .filter_map(|&(s, e)| {
            let h = map.host_address(s, e.saturating_sub(s).max(1)).ok()?;
            pages.page_range(h, e.saturating_sub(s).max(1))
        })
        .collect();
    ex.sort_unstable();
    for cut in cuts.iter_mut() {
        let mut out: Vec<(u32, u32)> = Vec::new();
        for &(a, b) in &cut.pages {
            let mut from = a;
            for &(s, e) in ex.iter().filter(|&&(s, e)| s < b && a < e) {
                if s > from {
                    out.push((from, s));
                }
                from = from.max(e);
            }
            if from < b {
                out.push((from, b));
            }
        }
        cut.pages = out;
    }
    cuts.retain(|c| !c.pages.is_empty());
}

/// What the encode did with the plan of a frame, for the report.
pub(crate) fn note_split(plan: &EarlyPlan, passes: usize, ahead: bool) {
    let cuts = &plan.cuts;
    let mut w = locked(&state().window);
    w.early_readers += plan.readers as u64;
    w.early_unserved += plan.blocked as u64;
    if cuts.is_empty() {
        return;
    }
    w.early_frames += 1;
    w.early_cuts += cuts.len() as u64;
    w.early_ahead += u64::from(ahead);
    for c in cuts {
        w.early_pos += (c.after_pass + 1) as f64 / passes.max(1) as f64;
        w.early_pages += c.pages.iter().map(|&(a, b)| u64::from(b - a)).sum::<u64>();
    }
}

/// The callback of a cut: the writer of its pages in the frame of `fence` is done.
pub(crate) fn early_release(fence: u64, pages: Vec<(u32, u32)>) -> impl FnOnce() + Send + 'static {
    move || Pages::global().release_early(fence, &pages)
}

/// The flush of a frame, on the CPU thread: protects the member pages and the pages the frame may write, and counts. The
/// frame is complete when [`encoded`] reports its batch done.
///
/// With the mirror (`NXKIT_PRIVATE=1`) it also makes the pages that the frame's bindings touch members, copies the pages
/// that the CPU wrote into a staging buffer and returns the overlay of the frame: its pool snapshot resolves member
/// ranges to the mirror, and the first command buffer copies the staged pages into the mirror. `drain` waits for every
/// frame in flight; it is called when a page becomes a member that such a frame may still write in guest memory.
pub(crate) fn at_flush(
    detail: &FrameDetail,
    fence: u64,
    map: &AddressMap,
    forced: u32,
    late: bool,
    drain: &dyn Fn(),
) -> Option<Arc<GuestOverlay>> {
    if !on() || fence == 0 {
        return None;
    }
    if mirrored() {
        return flush_mirror(detail, fence, map, forced, late, drain);
    }
    let st = state();
    let t_plan = Instant::now();
    let learned = locked(&st.learned).clone();
    let (ranges, clipped) = {
        let rw = locked(&st.rw);
        plan_written(detail, &learned, &rw, map)
    };
    let plan_ns = t_plan.elapsed().as_nanos() as u64;
    let f = Pages::global().flush(fence, &ranges);
    let n = st.frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let report = {
        let mut w = locked(&st.window);
        w.flushes += 1;
        w.written_pages += u64::from(f.written);
        w.written_runs += u64::from(f.written_runs);
        w.new_members += u64::from(f.new_members);
        w.dirtied += u64::from(f.dirtied);
        w.flush_ns += f.ns;
        w.plan_ns += plan_ns;
        w.mprotects += u64::from(f.mprotects);
        w.clipped += clipped;
        w.scanned += u64::from(f.scanned);
        n.is_multiple_of(REPORT_EVERY)
    };
    if report {
        print_report(n);
    }
    None
}

// ---------------------------------------------------------------------------------------------------------------------
// Step 2: the mirror.

/// A set of page runs `(first, end)`, sorted and disjoint.
#[derive(Default, Clone)]
struct PageRuns(Vec<(u32, u32)>);

impl PageRuns {
    fn contains(&self, p: u32) -> bool {
        let i = self.0.partition_point(|&(_, e)| e <= p);
        self.0.get(i).is_some_and(|&(s, _)| s <= p)
    }

    fn add(&mut self, runs: &[(u32, u32)]) {
        if runs.is_empty() {
            return;
        }
        let mut v = std::mem::take(&mut self.0);
        v.extend_from_slice(runs);
        v.sort_unstable();
        let mut out: Vec<(u32, u32)> = Vec::with_capacity(v.len());
        for (s, e) in v {
            match out.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => out.push((s, e)),
            }
        }
        self.0 = out;
    }
}

/// The member pages as the closure sees them: the bitmap of the tracker and the pages added in this flush.
struct Members<'a> {
    bits: &'a [u64],
    added: PageRuns,
}

impl Members<'_> {
    fn is(&self, p: u32) -> bool {
        self.bits
            .get(p as usize / 64)
            .is_some_and(|w| w >> (p % 64) & 1 != 0)
            || self.added.contains(p)
    }

    fn any(&self, first: u32, end: u32) -> bool {
        (first..end).any(|p| self.is(p))
    }

    fn all(&self, first: u32, end: u32) -> bool {
        (first..end).all(|p| self.is(p))
    }
}

/// How much of a vertex stream a draw reads, at most: the vertices of an arrays draw, the instances of an instanced one.
/// An indexed draw reads what its indices reach, which the host does not know here: a part of `UNKNOWN_BYTES`.
fn stream_extent(s: &StreamDetail, args: &DrawArgs) -> u64 {
    let stride = u64::from(s.stride).max(16);
    let per_instance = |instances: u32, base: u32| {
        stride * (u64::from(base) + u64::from(instances)).div_ceil(u64::from(s.divisor.max(1)))
    };
    let want = match *args {
        DrawArgs::Arrays {
            first,
            count,
            instances,
            base_instance,
        } => {
            if s.divisor == 0 {
                stride * (u64::from(first) + u64::from(count))
            } else {
                per_instance(instances, base_instance)
            }
        }
        DrawArgs::Indexed {
            instances,
            base_instance,
            ..
        } if s.divisor != 0 => per_instance(instances, base_instance),
        _ => UNKNOWN_BYTES,
    };
    let bound = if s.size == 0 { UNKNOWN_BYTES } else { s.size };
    want.min(bound).max(1)
}

/// The ranges (GPU address, length) that the frame's GPU work reads through a binding: uniform and storage buffers,
/// vertex streams, indices, indirect arguments and copy sources. A binding that touches a member page makes the rest
/// of its range members too.
///
/// The third number is the kind of binding: [`UBO`], [`SBUF`], [`STREAM`], [`INDEX`], [`INDIRECT`] or [`COPY`].
fn read_ranges(detail: &FrameDetail) -> Vec<(u64, u64, u8)> {
    let mut out: Vec<(u64, u64, u8)> = Vec::new();
    let mut seen = vec![(0u64, 0u64, 0u8); 4096];
    let mut add = |a: u64, len: u64, kind: u8| {
        if a == 0 || len == 0 {
            return;
        }
        let at = ((a >> 4) ^ (a >> 17) ^ len) as usize & 4095;
        if seen[at] == (a, len, kind) {
            return;
        }
        seen[at] = (a, len, kind);
        out.push((a, len, kind));
    };
    let mut prev = 0usize;
    for d in &detail.draws {
        let ptr = Arc::as_ptr(d) as usize;
        if ptr == prev {
            continue;
        }
        prev = ptr;
        for u in &d.ubos {
            add(u.address, u64::from(u.size), BIND_UBO);
        }
        for u in &d.sbufs {
            add(u.address, u64::from(u.size), BIND_SBUF);
        }
        for s in &d.streams {
            add(s.address, stream_extent(s, &d.args), BIND_STREAM);
        }
        match d.args {
            DrawArgs::Indexed {
                index_type,
                address,
                count,
                ..
            } => add(address, u64::from(count) << index_type.min(2), BIND_INDEX),
            DrawArgs::ArraysIndirect { indirect } => add(indirect, 16, BIND_INDIRECT),
            DrawArgs::IndexedIndirect { address, indirect, .. } => {
                add(address, UNKNOWN_BYTES, BIND_INDEX);
                add(indirect, 20, BIND_INDIRECT);
            }
            DrawArgs::MultiIndirectCount {
                index,
                indirect,
                count,
                max,
                stride,
            } => {
                if let Some((_, a)) = index {
                    add(a, UNKNOWN_BYTES, BIND_INDEX);
                }
                add(indirect, (u64::from(max) * stride.max(20)).min(RANGE_MAX_BYTES), BIND_INDIRECT);
                add(count, 4, BIND_INDIRECT);
            }
            DrawArgs::MeshTasksIndirect { indirect } => add(indirect, 8, BIND_INDIRECT),
            _ => {}
        }
    }
    for d in &detail.dispatches {
        for u in &d.ubos {
            add(u.address, u64::from(u.size), BIND_UBO);
        }
        for u in &d.sbufs {
            add(u.address, u64::from(u.size), BIND_SBUF);
        }
        if let Some(a) = d.indirect {
            add(a, 12, BIND_INDIRECT);
        }
    }
    for c in &detail.copies {
        match c.kind {
            CopyKind::BufferToBuffer => add(c.src_addr, c.bytes, BIND_COPY),
            CopyKind::BufferToTexture => add(c.buffer, UNKNOWN_BYTES, BIND_COPY),
            _ => {}
        }
    }
    out
}

/// The kinds of binding of [`read_ranges`], and their names for the report.
const BIND_UBO: u8 = 0;
const BIND_SBUF: u8 = 1;
const BIND_STREAM: u8 = 2;
const BIND_INDEX: u8 = 3;
const BIND_INDIRECT: u8 = 4;
const BIND_COPY: u8 = 5;
const BIND_NAMES: [&str; 6] = ["uniform", "storage", "vertex", "index", "indirect", "copy source"];

/// The host address of the GPU range `[a, a + len)` and its length cut to the end of guest memory.
fn clamped_host(map: &AddressMap, a: u64, len: u64) -> Option<(u64, u64)> {
    let h = map.host_address(a, 1).ok()?;
    let room = map.host_base() + map.len() - h;
    Some((h, len.min(room)))
}

/// The most rounds of the closure: a range that is made whole can touch another binding, which makes its range whole in
/// the next round. The rounds stop when a round adds nothing.
const CLOSURE_ROUNDS: usize = 8;

/// The pages that become members because a binding of the frame touches a member (the written ranges included): rounds
/// until nothing is added, at most [`CLOSURE_ROUNDS`].
fn closure(
    pages: &Pages,
    reads: &[(u64, u64, u8)],
    map: &AddressMap,
    members: &mut Members<'_>,
) -> u64 {
    // The page range of each binding, once. Members only grow, so a range that is whole stays whole and leaves the list;
    // the others are looked at again in the next round.
    let mut todo: Vec<(u32, u32)> = Vec::with_capacity(reads.len());
    for &(a, len, _) in reads {
        let len = len.min(RANGE_MAX_BYTES);
        // A range that reaches past the end of guest memory (a placeholder size) is cut to it.
        let Some((h, len)) = clamped_host(map, a, len) else {
            continue;
        };
        if let Some((first, end)) = pages.page_range(h, len)
            && !members.all(first, end)
        {
            todo.push((first, end));
        }
    }
    let page = pages.page_size();
    let mut added_pages = 0u64;
    for _ in 0..CLOSURE_ROUNDS {
        let mut new: Vec<(u32, u32)> = Vec::new();
        for &(first, end) in &todo {
            if members.any(first, end) && !members.all(first, end) {
                new.push((first, end));
            }
        }
        if new.is_empty() {
            break;
        }
        let mut budget = CLOSURE_MAX_BYTES.saturating_sub(added_pages * page);
        new.retain(|&(s, e)| {
            let bytes = u64::from(e - s) * page;
            let keep = bytes <= budget;
            if keep {
                budget -= bytes;
            }
            keep
        });
        let before: u64 = members.added.0.iter().map(|&(s, e)| u64::from(e - s)).sum();
        members.added.add(&new);
        let after: u64 = members.added.0.iter().map(|&(s, e)| u64::from(e - s)).sum();
        added_pages += after - before;
        if after == before {
            break;
        }
        todo.retain(|&(first, end)| !members.all(first, end));
    }
    added_pages
}

/// The staging buffers: one per frame in flight, taken back when the frame is complete. A buffer is never dropped by the
/// last holder of a frame (that may be a completion handler): the pool keeps its own reference until it takes it back.
#[derive(Default)]
struct StagePool {
    free: Vec<Arc<DataBuffer>>,
    busy: Vec<(u64, Arc<DataBuffer>)>,
}

/// The most staging buffers that wait for their frames.
const STAGE_BUSY_MAX: usize = 16;

impl StagePool {
    fn reclaim(&mut self, completed: u64) {
        let mut i = 0;
        while i < self.busy.len() {
            if self.busy[i].0 <= completed {
                let (_, b) = self.busy.swap_remove(i);
                self.free.push(b);
            } else {
                i += 1;
            }
        }
        while self.busy.len() > STAGE_BUSY_MAX {
            self.busy.remove(0);
        }
        self.free.truncate(4);
    }

    /// A buffer for `pages` pages.
    fn take(&mut self, ctx: &Context, pages: usize) -> Result<Arc<DataBuffer>, nxkit_metal::MetalError> {
        let need = pages.max(1) as u64 * PAGE;
        if let Some(i) = self.free.iter().position(|b| b.len() >= need) {
            return Ok(self.free.swap_remove(i));
        }
        // None fits: the smaller ones go, a larger one is made.
        self.free.clear();
        let pages = (pages.max(256) as u64).next_power_of_two();
        DataBuffer::new_unfilled(ctx, pages * PAGE).map(Arc::new)
    }
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

impl<'a> Sink<'a> {
    fn new(ctx: &'a Context, pool: &'a mut StagePool) -> Self {
        Self {
            ctx,
            pool,
            buf: None,
            cap: 0,
            slots: Vec::new(),
            index: FastMap::default(),
            skips: FastMap::default(),
            failed: false,
        }
    }

    /// What the sink took: the buffer, the slots, the masks sorted by page, and whether it failed.
    #[allow(clippy::type_complexity)]
    fn finish(&mut self) -> (Option<Arc<DataBuffer>>, Vec<(u32, u32)>, Vec<(u32, [u64; 4])>, bool) {
        let mut skips: Vec<(u32, [u64; 4])> = self.skips.drain().collect();
        skips.sort_unstable_by_key(|s| s.0);
        (self.buf.take(), std::mem::take(&mut self.slots), skips, self.failed)
    }

    fn grow(&mut self) {
        let Some(old) = self.buf.take() else {
            self.failed = true;
            return;
        };
        let used = self.slots.len() as u64 * PAGE;
        let bigger = DataBuffer::new_unfilled(self.ctx, old.len() * 2).map(Arc::new);
        let moved = bigger.and_then(|b| {
            let mut tmp = vec![0u8; used as usize];
            old.read(0, &mut tmp)?;
            b.write(0, &tmp)?;
            Ok(b)
        });
        match moved {
            Ok(b) => {
                self.cap = (b.len() / PAGE) as usize;
                self.buf = Some(b);
                self.pool.free.push(old);
            }
            Err(_) => {
                self.buf = Some(old);
                self.failed = true;
            }
        }
    }
}

impl Stager for Sink<'_> {
    fn begin(&mut self, max_pages: usize) {
        match self.pool.take(self.ctx, max_pages) {
            Ok(b) => {
                self.cap = (b.len() / PAGE) as usize;
                self.buf = Some(b);
            }
            Err(_) => self.failed = true,
        }
    }

    fn stage(&mut self, page: u32, bytes: &[u8]) {
        self.stage_masked(page, bytes, [0; 4]);
    }

    fn stage_masked(&mut self, page: u32, bytes: &[u8], skip: [u64; 4]) {
        if self.failed {
            return;
        }
        if skip.iter().any(|w| *w != 0) {
            self.skips.insert(page, skip);
        } else {
            self.skips.remove(&page);
        }
        let slot = match self.index.get(&page) {
            Some(&s) => s,
            None => {
                if self.slots.len() >= self.cap {
                    self.grow();
                    if self.failed {
                        return;
                    }
                }
                let s = self.slots.len() as u32;
                self.slots.push((page, s));
                self.index.insert(page, s);
                s
            }
        };
        if let Some(b) = &self.buf
            && b.write(u64::from(slot) * PAGE, bytes).is_err()
        {
            self.failed = true;
        }
    }
}

fn flush_mirror(
    detail: &FrameDetail,
    fence: u64,
    map: &AddressMap,
    forced: u32,
    late: bool,
    drain: &dyn Fn(),
) -> Option<Arc<GuestOverlay>> {
    let st = state();
    let pages = Pages::global();
    let t_all = Instant::now();
    let t_plan = Instant::now();
    let learned = locked(&st.learned).clone();
    let (written, clipped) = {
        let rw = locked(&st.rw);
        plan_written(detail, &learned, &rw, map)
    };
    let t_written_ns = t_plan.elapsed().as_nanos() as u64;
    // The members after this flush: the tracked pages, the written ranges and what the bindings touch.
    let (bits, _) = pages.member_snapshot();
    let mut members = Members {
        bits: &bits,
        added: PageRuns::default(),
    };
    let written_runs: Vec<(u32, u32)> = written
        .iter()
        .filter_map(|&(h, l)| pages.page_range(h, l))
        .collect();
    members.added.add(&written_runs);
    let t_r = Instant::now();
    let mut reads = read_ranges(detail);
    // Ranges that the encoder found split between members and non-members in an earlier frame and that no binding of
    // this one announces (texture upload sources, ranges of recordings): they become whole too.
    let learned_partial = crate::overlay::partial_ranges();
    reads.extend(learned_partial.iter().map(|&(a, l)| (a, l, BIND_COPY)));
    let t_reads_ns = t_r.elapsed().as_nanos() as u64;
    let t_c = Instant::now();
    let closure_pages = closure(pages, &reads, map, &mut members);
    let t_closure_ns = t_c.elapsed().as_nanos() as u64;
    let psize = pages.page_size();
    let host_of = |p: u32| pages.host_base() + u64::from(p) * psize;
    let member_ranges: Vec<(u64, u64)> = members
        .added
        .0
        .iter()
        .map(|&(s, e)| (host_of(s), u64::from(e - s) * psize))
        .collect();
    let plan_ns = t_plan.elapsed().as_nanos() as u64;
    let mut guard = locked(&st.mirror);
    let ms = guard.as_mut()?;
    let t_e = Instant::now();
    // The segments of the mirror that hold them.
    let host0 = map.host_base();
    let offsets: Vec<(u64, u64)> = member_ranges
        .iter()
        .map(|&(h, l)| (h - host0, h - host0 + l))
        .collect();
    if let Err(e) = ms.mirror.ensure(&offsets) {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| eprintln!("nxkit-gpu: NXKIT_PRIVATE=1: the mirror cannot grow: {e}"));
        // From here on guest memory is the truth again: the page tracker stops copying from the mirror.
        pages.set_mirror(0);
        FAILED.store(true, Ordering::Relaxed);
        return None;
    }
    let t_ensure_ns = t_e.elapsed().as_nanos() as u64;
    // A page that becomes a member in a pool that raw stores wrote may still be written in guest memory by a frame in
    // flight: wait for those frames once, before the page is copied.
    let enters_learned = members.added.0.iter().any(|&(s, e)| {
        (s..e).any(|p| {
            let wasn_t = members.bits.get(p as usize / 64).is_none_or(|w| w >> (p % 64) & 1 == 0);
            wasn_t
                && learned.iter().any(|&(lo, hi)| {
                    map.host_address(lo, hi - lo)
                        .ok()
                        .and_then(|h| pages.page_range(h, hi - lo))
                        .is_some_and(|(a, b)| a <= p && p < b)
                })
        })
    });
    let mut enter_wait = false;
    if enters_learned && pages.completed() + 1 < fence {
        enter_wait = true;
        // The mirror lock is not held across the wait: the encoder takes it to read the runs.
        drop(guard);
        drain();
        guard = locked(&st.mirror);
    }
    let ms = guard.as_mut()?;
    ms.pool.reclaim(pages.completed());
    let poison = poison_on().then_some(POISON);
    let mut sink = Sink::new(&ms.ctx, &mut ms.pool);
    let f = pages.flush_mirrored(fence, &member_ranges, &written, poison, &mut sink);
    let (buf, slots, skips, failed) = sink.finish();
    drop(sink);
    if failed {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| eprintln!("nxkit-gpu: NXKIT_PRIVATE=1: the staging buffer failed; pages the CPU wrote are lost to the GPU"));
    }
    let staged = match buf {
        Some(b) if !slots.is_empty() && !failed => {
            ms.pool.busy.push((fence, b.clone()));
            let mut slots = slots;
            slots.sort_unstable();
            Some(Staged { buf: b, slots, skips })
        }
        Some(b) => {
            ms.pool.free.push(b);
            None
        }
        None => None,
    };
    let mut diag_sets: Option<DiagSets> = None;
    if crate::diag("private") {
        // What the frame binds, to sort the writes of the CPU between its flush and its commit.
        let r: Vec<(u64, u64, u8)> = reads
            .iter()
            .filter_map(|&(a, len, kind)| {
                let len = len.min(RANGE_MAX_BYTES);
                let h = map.host_address(a, len).ok()?;
                Some((h, h + len, kind))
            })
            .collect();
        let mut sources: Vec<(u64, u64, u8)> = {
            let rw = locked(&st.rw);
            written_ranges(detail, &[], &rw)
                .iter()
                .filter_map(|&(a, len)| {
                    let h = map.host_address(a, len.max(1)).ok()?;
                    Some((h, h + len.max(1), 0))
                })
                .collect()
        };
        sources.extend(learned.iter().filter_map(|&(lo, hi)| {
            let h = map.host_address(lo, hi - lo).ok()?;
            Some((h, h + (hi - lo), 1))
        }));
        diag_sets = Some((r, written.iter().map(|&(h, l)| (h, h + l)).collect(), sources));
    }
    let (bits, member_gen) = pages.member_snapshot();
    let runs: Vec<Run<Arc<GuestMemory>>> = ms
        .mirror
        .runs()
        .iter()
        .map(|r| Run {
            va_start: ms.va_base + r.start,
            va_end: ms.va_base + r.end,
            mem: r.mem.clone(),
        })
        .collect();
    let generation = member_gen << 24 | (ms.mirror.generation() & 0xff_ffff);
    ms.last_fence = fence;
    // The knobs that bind a class of resolves to guest memory, to find which class a defect comes from.
    let off = [
        ("privnoubo", crate::overlay::UBO),
        ("privnosbuf", crate::overlay::SBUF),
        ("privnostream", crate::overlay::STREAM),
        ("privnoupload", crate::overlay::UPLOAD),
        ("privnogmem", crate::overlay::GMEM),
        ("privnocopy", crate::overlay::COPY),
    ]
    .iter()
    .filter(|(k, _)| crate::knob(k))
    .fold(0u8, |a, (_, c)| a | c);
    // With a class bound to guest memory, guest memory must be current: every page a frame wrote is copied back when the
    // frame completes.
    pages.set_eager_sync(off != 0);
    let overlay = Overlay::new(ms.va_base, bits, runs, Some(ms.mirror.view()), staged, generation)
        .with_off(off)
        .with_frame(fence, late);
    drop(guard);
    let n = st.frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let report = {
        let mut w = locked(&st.window);
        w.flushes += 1;
        w.written_pages += u64::from(f.written);
        w.written_runs += u64::from(f.written_runs);
        w.new_members += u64::from(f.new_members);
        w.dirtied += u64::from(f.dirtied);
        w.flush_ns += f.ns;
        w.plan_ns += plan_ns;
        w.mprotects += u64::from(f.mprotects);
        w.clipped += clipped;
        w.scanned += u64::from(f.scanned);
        w.staged += u64::from(f.staged);
        w.raced += u64::from(f.raced);
        w.closure_pages += closure_pages;
        w.enter_waits += u64::from(enter_wait);
        w.t_plan_written += t_written_ns;
        w.t_reads += t_reads_ns;
        w.t_closure += t_closure_ns;
        w.t_ensure += t_ensure_ns;
        w.t_pages += f.ns;
        let total = t_all.elapsed().as_nanos() as u64;
        w.t_total += total;
        w.t_rest += total.saturating_sub(t_written_ns + t_reads_ns + t_closure_ns + t_ensure_ns + f.ns);
        w.t_new += f.new_ns;
        w.t_log += f.log_ns;
        w.t_stage += f.stage_ns;
        w.t_hot += f.hot_ns;
        w.t_tag += f.tag_ns;
        w.t_prot += f.mprotect_ns;
        w.hot_pages += u64::from(f.hot);
        w.hot_staged += u64::from(f.hot_staged);
        w.hot_same += u64::from(f.hot_same);
        w.promoted += u64::from(f.promoted);
        w.demoted += u64::from(f.demoted);
        n.is_multiple_of(REPORT_EVERY)
    };
    if report {
        print_report(n);
    }
    let overlay = Arc::new(overlay);
    if let Some((reads, written, sources)) = diag_sets {
        let mut q = locked(&st.sets);
        q.push_back(Arc::new(FrameSets {
            fence,
            reads,
            written,
            sources,
            forced,
            view: overlay.clone(),
        }));
        while q.len() > 12 {
            q.pop_front();
        }
    }
    Some(overlay)
}

/// One page of the copy into the mirror: the mirror address, the staging address, and the 64-byte blocks to leave alone
/// (bit `b % 64` of word `b / 64`; they hold what a frame in flight writes).
pub(crate) type CopyJob = (u64, u64, [u64; 4]);

/// The kernel that copies staged pages into the mirror: one page per 256 threads, 16 bytes each, four times, except the
/// blocks of 64 bytes that the job skips.
const COPY_IN_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct page_job {
    device uint4 *dst;
    device const uint4 *src;
    ulong skip[4];
};
kernel void nxkit_private_in(device const page_job *jobs [[buffer(0)]], uint t [[thread_position_in_grid]]) {
    page_job j = jobs[t >> 8];
    uint lane = t & 255u;
    for (uint i = lane; i < 1024u; i += 256u) {
        uint b = i >> 2;
        if (((j.skip[b >> 6] >> (b & 63u)) & 1ul) != 0ul) continue;
        j.dst[i] = j.src[i];
    }
}
"#;

/// The [`CopyJob`] of every page that the frame's first command buffer copies, and the number of
/// staged pages whose page is no longer a member of the view (they are dropped).
pub(crate) fn copy_in_jobs(o: &GuestOverlay) -> (Vec<CopyJob>, u32) {
    let Some(st) = o.staged() else {
        return (Vec::new(), 0);
    };
    let base = st.buf.gpu_address();
    let mut lost = 0;
    let jobs = st
        .slots
        .iter()
        .filter_map(|&(page, slot)| {
            let va = o.va_base() + u64::from(page) * PAGE;
            match o.locate(va) {
                Some((_, real, _)) => Some((real, base + u64::from(slot) * PAGE, st.skip_of(page).unwrap_or([0; 4]))),
                None => {
                    lost += 1;
                    None
                }
            }
        })
        .collect();
    (jobs, lost)
}

/// Encodes the copy of the staged pages into the mirror as a compute pass of `cb`, with a barrier from all earlier work
/// (the frame before still reads and writes the mirror) and one for all later work. Returns the pages copied.
pub(crate) fn encode_copy_in(
    pipelines: &Pipelines,
    cb: &mut CommandBuffer,
    table: &ArgumentTable,
    scratch: &Scratch,
    jobs: &[CopyJob],
) -> Result<u32, Skip> {
    if jobs.is_empty() {
        return Ok(0);
    }
    let bytes: Vec<u8> = jobs
        .iter()
        .flat_map(|&(dst, src, skip)| {
            dst.to_le_bytes()
                .into_iter()
                .chain(src.to_le_bytes())
                .chain(skip.into_iter().flat_map(u64::to_le_bytes))
        })
        .collect();
    let job = scratch.push(&bytes).map_err(|_| Skip::Buffer)?;
    let pipeline = pipelines.host_compute("nxkit-private", COPY_IN_MSL, "nxkit_private_in")?;
    let mut cp = cb.begin_compute_pass(table).map_err(|_| Skip::Encode)?;
    let enc = |cp: &mut nxkit_metal::ComputePass<'_>, c: ComputeCommand| cp.encode(&c).map_err(|_| Skip::Encode);
    enc(
        &mut cp,
        ComputeCommand::Depend(Dependency {
            scope: Scope::FromEarlierPasses,
            after: Stages::ALL,
            before: Stages::DISPATCH,
            visibility: Visibility::Device,
        }),
    )?;
    enc(&mut cp, ComputeCommand::SetPipeline(pipeline))?;
    enc(
        &mut cp,
        ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: job,
        },
    )?;
    enc(
        &mut cp,
        ComputeCommand::DispatchThreads {
            grid: [256 * jobs.len() as u32, 1, 1],
            threads: [256, 1, 1],
        },
    )?;
    enc(
        &mut cp,
        ComputeCommand::Depend(Dependency {
            scope: Scope::ToLaterPasses,
            after: Stages::DISPATCH,
            before: Stages::ALL,
            visibility: Visibility::Device,
        }),
    )?;
    cp.end();
    Ok(jobs.len() as u32)
}

/// Counts what the copy-in of a frame did, for the report.
pub(crate) fn note_copy_in(pages: u32, lost: u32) {
    let mut w = locked(&state().window);
    w.copy_in_pages += u64::from(pages);
    w.copy_in_lost += u64::from(lost);
}

/// True when a page of the guest GPU range `[addr, addr + len)` may hold stale guest bytes because a frame wrote it on the
/// GPU (the walker does not snapshot such a range from guest memory).
pub(crate) fn gpu_owned(host_addr: u64, len: u64) -> bool {
    mirrored() && Pages::global().any_gpu_owned(host_addr, len)
}

/// True when every page of the guest range is a member, so a frame that binds it reads the mirror (the walk takes no
/// snapshot of the range).
pub(crate) fn mirror_covers(host_addr: u64, len: u64) -> bool {
    mirrored() && Pages::global().all_members(host_addr, len)
}

/// The encoder submitted the frame of `fence`: its pages are released when the batch is complete on the GPU. A frame
/// without a batch is complete at once.
pub(crate) fn encoded(fence: u64, batch: Option<&SubmittedBatch>) {
    if !on() {
        return;
    }
    // The frame is committed and runs; the completion says it is done.
    Pages::global().note_committed(fence);
    note_between(fence);
    let done = move || Pages::global().complete(fence);
    match batch {
        Some(b) => b.on_complete(done),
        None => done(),
    }
}

/// The commit of `fence` is reported: sorts the writes that the CPU first made to member pages since its flush by what the
/// frame binds. A write is in a byte range that the frame writes, in one it reads through a binding (by kind), in a page of
/// such a range but outside its bytes, or in no binding of the frame.
fn note_between(fence: u64) {
    if !crate::diag("private") {
        return;
    }
    drain_in_flight();
    let pages = Pages::global();
    let (writes, total) = pages.take_between(fence);
    let sets = locked(&state().sets).iter().find(|f| f.fence == fence).cloned();
    let mut w = locked(&state().window);
    w.between_frames += 1;
    w.between_pages += u64::from(total);
    let Some(sets) = sets else {
        w.between_other += u64::from(total);
        return;
    };
    let psize = pages.page_size();
    let page_of = |a: u64| (a - pages.host_base()) / psize;
    let mut reads_forced = 0u64;
    for &a in &writes {
        if sets.written.iter().any(|&(s, e)| s <= a && a < e) {
            w.between_written += 1;
        } else if let Some(&(_, _, kind)) = sets.reads.iter().find(|&&(s, e, _)| s <= a && a < e) {
            w.between_reads[kind as usize] += 1;
            reads_forced += 1;
            if w.between_samples < 12 {
                w.between_samples += 1;
                let (s, e, _) = *sets.reads.iter().find(|&&(s, e, _)| s <= a && a < e).unwrap_or(&(0, 0, 0));
                eprintln!(
                    "DIAG private between sample: fence {fence} write at {a:#x} (offset {:#x} in its page) inside a {} binding \
                     {s:#x}..{e:#x} ({} bytes)",
                    (a - pages.host_base()) % psize,
                    BIND_NAMES[kind as usize],
                    e - s,
                );
            }
        } else if sets.written.iter().any(|&(s, e)| page_of(s) <= page_of(a) && page_of(a) <= page_of(e - 1))
            || sets.reads.iter().any(|&(s, e, _)| page_of(s) <= page_of(a) && page_of(a) <= page_of(e - 1))
        {
            w.between_same_page += 1;
        } else {
            w.between_other += 1;
        }
    }
    w.between_other += u64::from(total).saturating_sub(writes.len() as u64);
    // The exact test: the bytes of the bound ranges in those pages now against what the frame sees.
    let mut seen_pages: Vec<u64> = writes.iter().map(|&a| page_of(a)).collect();
    seen_pages.sort_unstable();
    seen_pages.dedup();
    for page in seen_pages {
        let pa = pages.host_base() + page * psize;
        for &(s, e, kind) in &sets.reads {
            let (lo, hi) = (s.max(pa), e.min(pa + psize));
            if lo >= hi {
                continue;
            }
            let mut now = vec![0u8; (hi - lo) as usize];
            let mut seen = vec![0u8; (hi - lo) as usize];
            if !pages.peek(lo, &mut now) {
                w.uncompared += 1;
                continue;
            }
            if !sets.view.read(sets.view.va_base() + (lo - pages.host_base()), &mut seen) {
                w.uncompared += 1;
                continue;
            }
            let diff = now.iter().zip(&seen).filter(|(a, b)| a != b).count() as u64;
            if diff > 0 {
                w.changed_ranges[kind as usize] += 1;
                w.changed_bytes += diff;
            }
        }
    }
    if sets.forced > 0 {
        w.forced_frames += 1;
        w.forced_reads += reads_forced;
    }
}

/// Sorts the first accesses of game threads to pages of frames in flight, as of now, by where the frame's right to write the
/// page comes from, into the window (`NXKIT_DIAG=private`). Called at each commit, while the sets of the frames are kept.
fn drain_in_flight() {
    let pages = Pages::global();
    let (log, _) = pages.take_in_flight();
    if log.is_empty() {
        return;
    }
    let psize = pages.page_size();
    let sets = locked(&state().sets).clone();
    let mut w = locked(&state().window);
    for &(frame, a, write) in &log {
        let pa = a - (a - pages.host_base()) % psize;
        let set = sets.iter().find(|f| (f.fence & 0x0fff_ffff) as u32 == frame);
        let exact = set.and_then(|f| f.sources.iter().find(|&&(s, e, _)| s <= a && a < e));
        let (class, what) = match (set, exact) {
            (_, Some(&(s, e, 0))) => (0usize, format!("inside a binding or copy {s:#x}..{e:#x} ({} bytes)", e - s)),
            (_, Some(&(s, e, _))) => (1, format!("inside a raw-store pool {s:#x}..{e:#x} ({} MiB)", (e - s) >> 20)),
            (Some(f), None) => {
                let near = f.sources.iter().find(|&&(s, e, _)| s < pa + psize && pa < e);
                match near {
                    Some(&(s, e, _)) => (2, format!("outside the bytes of the range {s:#x}..{e:#x} that shares the page")),
                    None => (2, "in no source of its frame".to_string()),
                }
            }
            (None, None) => (3, format!("frame {frame} not kept")),
        };
        w.flight_by_source[class][usize::from(write)] += 1;
        if let Some(e) = w.flight_pages.iter_mut().find(|e| e.0 == a) {
            e.1 += 1;
        } else if w.flight_pages.len() < 64 {
            w.flight_pages.push((a, 1, write, what));
        }
    }
}

/// `DIAG private in flight`: what game threads touched while the frame that tags it was in flight.
fn report_in_flight(w: &mut Window) {
    let t = |c: usize| w.flight_by_source[c][0] + w.flight_by_source[c][1];
    if (0..4).all(|c| t(c) == 0) {
        return;
    }
    w.flight_pages.sort_unstable_by_key(|e| std::cmp::Reverse(e.1));
    let top: Vec<String> = w
        .flight_pages
        .iter()
        .take(6)
        .map(|(a, c, write, what)| format!("{a:#x} {c} first {} ({what})", if *write { "writes" } else { "reads" }))
        .collect();
    eprintln!(
        "DIAG private in flight (first accesses of game threads to a page of a frame in flight, in the window; read/write): \
         at an address inside the bytes of a binding or copy the frame may write {}/{}, inside a raw-store pool {}/{}, outside \
         those bytes in a page they share (false sharing) {}/{}, the frame was not kept {}/{}; most touched addresses: {}",
        w.flight_by_source[0][0],
        w.flight_by_source[0][1],
        w.flight_by_source[1][0],
        w.flight_by_source[1][1],
        w.flight_by_source[2][0],
        w.flight_by_source[2][1],
        w.flight_by_source[3][0],
        w.flight_by_source[3][1],
        top.join("; "),
    );
}

/// A staging buffer and the `(page, slot)` of the pages in it.
type Captured = (Arc<DataBuffer>, Vec<(u32, u32)>, Vec<(u32, [u64; 4])>);

/// The byte ranges a flushed frame reads (with the kind of binding), may write, and may write by source, for the report.
type DiagSets = (Vec<(u64, u64, u8)>, Vec<(u64, u64)>, Vec<(u64, u64, u8)>);

/// A frame whose flush ran submits before the CPU signals they wait for: the capture of the CPU's writes stays open until
/// the signals come (the data those submits read is written until then).
#[derive(Default)]
struct LateFrame {
    /// The signals the frame still waits for; negative when signals came before the flush counted them.
    waiting: i32,
    /// The flush counted the signals.
    expected: bool,
    /// The capture ran. `staged` holds the pages: the buffer and `(page, slot)`.
    done: bool,
    staged: Option<Captured>,
}

static LATE: Mutex<Vec<(u64, LateFrame)>> = Mutex::new(Vec::new());
static LATE_CV: std::sync::Condvar = std::sync::Condvar::new();
/// Consecutive frames whose signals did not come in time, and the number after which the capture stops waiting for them.
static LATE_MISSED: AtomicU64 = AtomicU64::new(0);
const LATE_MISSED_MAX: u64 = 4;
/// How long the encode of a frame waits for the signals of its forced submits.
const LATE_WAIT: std::time::Duration = std::time::Duration::from_millis(15);

/// True when the capture still waits for CPU signals (it stops after [`LATE_MISSED_MAX`] frames in a row whose signals did
/// not come in time: the game does not signal what the submits wait for, or very late).
pub(crate) fn late_enabled() -> bool {
    mirrored() && LATE_MISSED.load(Ordering::Relaxed) < LATE_MISSED_MAX
}

fn late_entry(v: &mut Vec<(u64, LateFrame)>, fence: u64) -> &mut LateFrame {
    if let Some(i) = v.iter().position(|(f, _)| *f == fence) {
        return &mut v[i].1;
    }
    v.push((fence, LateFrame::default()));
    if v.len() > 16 {
        v.remove(0);
    }
    let i = v.iter().position(|(f, _)| *f == fence).unwrap_or(0);
    &mut v[i].1
}

/// The pages the CPU wrote since the last capture go into a staging buffer for `fence`.
fn late_capture(fence: u64, fr: &mut LateFrame) {
    fr.done = true;
    let mut guard = locked(&state().mirror);
    let Some(ms) = guard.as_mut() else { return };
    let mut sink = Sink::new(&ms.ctx, &mut ms.pool);
    Pages::global().flush_late(&mut sink);
    let (buf, slots, skips, failed) = sink.finish();
    drop(sink);
    let Some(b) = buf else { return };
    if slots.is_empty() || failed {
        ms.pool.free.push(b);
        return;
    }
    ms.pool.busy.push((fence, b.clone()));
    drop(guard);
    locked(&state().window).late_staged += slots.len() as u64;
    fr.staged = Some((b, slots, skips));
}

/// The flush of `fence` counted `n` CPU signals that its forced submits wait for (before the flush captures anything, so a
/// signal that comes meanwhile is not lost).
pub(crate) fn late_expect(fence: u64, n: u32) {
    let mut v = locked(&LATE);
    let fr = late_entry(&mut v, fence);
    fr.expected = true;
    fr.waiting += n as i32;
    if fr.waiting <= 0 && !fr.done {
        late_capture(fence, fr);
        LATE_CV.notify_all();
    }
    locked(&state().window).late_frames += 1;
}

/// A signal that a submit of `fence` waited for came: the last one closes the capture.
pub(crate) fn late_signal(fence: u64) {
    let mut v = locked(&LATE);
    let fr = late_entry(&mut v, fence);
    fr.waiting -= 1;
    if fr.expected && fr.waiting <= 0 && !fr.done {
        late_capture(fence, fr);
        LATE_CV.notify_all();
    }
}

/// The jobs ([`CopyJob`]) of a second copy-in pass for the frame of `o`, to encode before its first
/// command buffer is committed:
///
/// - a frame whose flush ran submits before the CPU signals they wait for waits here (at most [`LATE_WAIT`]) until the
///   signals came, and gets the pages the CPU wrote until the last of them: those submits read what the CPU wrote for
///   them, which the flush could not capture;
/// - with the knob `privlatestage`, every frame gets the pages the CPU wrote since the flush, whatever they were written
///   for (for comparing; with the CPU ahead of the encoder it also hands the frame the data of the next one).
pub(crate) fn late_stage(o: &GuestOverlay) -> Vec<CopyJob> {
    if !mirrored() {
        return Vec::new();
    }
    let mut taken: Vec<Captured> = Vec::new();
    if o.forced() {
        let t0 = Instant::now();
        let fence = o.frame();
        let mut v = locked(&LATE);
        let mut timed_out = false;
        loop {
            let fr = late_entry(&mut v, fence);
            if fr.done {
                break;
            }
            let left = LATE_WAIT.saturating_sub(t0.elapsed());
            if left.is_zero() {
                timed_out = true;
                late_capture(fence, fr);
                break;
            }
            v = LATE_CV.wait_timeout(v, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        let fr = late_entry(&mut v, fence);
        if let Some(s) = fr.staged.take() {
            taken.push(s);
        }
        drop(v);
        if timed_out {
            LATE_MISSED.fetch_add(1, Ordering::Relaxed);
        } else {
            LATE_MISSED.store(0, Ordering::Relaxed);
        }
        let mut w = locked(&state().window);
        w.late_wait_ns += t0.elapsed().as_nanos() as u64;
        w.late_timeouts += u64::from(timed_out);
    }
    if crate::knob("privlatestage") {
        let mut guard = locked(&state().mirror);
        if let Some(ms) = guard.as_mut() {
            let fence = o.frame();
            let mut sink = Sink::new(&ms.ctx, &mut ms.pool);
            Pages::global().flush_late(&mut sink);
            let (buf, slots, skips, failed) = sink.finish();
            drop(sink);
            if let Some(b) = buf {
                if slots.is_empty() || failed {
                    ms.pool.free.push(b);
                } else {
                    ms.pool.busy.push((fence, b.clone()));
                    drop(guard);
                    locked(&state().window).late_staged += slots.len() as u64;
                    taken.push((b, slots, skips));
                }
            }
        }
    }
    taken
        .iter()
        .flat_map(|(b, slots, skips)| {
            let base = b.gpu_address();
            slots.iter().filter_map(move |&(page, slot)| {
                let skip = skips
                    .binary_search_by_key(&page, |s| s.0)
                    .map_or([0; 4], |i| skips[i].1);
                o.locate(o.va_base() + u64::from(page) * PAGE)
                    .map(|(_, real, _)| (real, base + u64::from(slot) * PAGE, skip))
            })
        })
        .collect()
}

fn kind_sum(s: &PageStats, k: FaultKind) -> u64 {
    s.faults[k as usize].iter().sum()
}

fn print_report(frame: u64) {
    let st = state();
    let pages = Pages::global();
    let now = pages.stats();
    let mut w = locked(&st.window);
    let base = w.base;
    let n = w.flushes.max(1) as f64;
    let page = pages.page_size() as f64;
    let mib = |p: f64| p * page / (1024.0 * 1024.0);
    let d = |k: FaultKind| kind_sum(&now, k) - kind_sum(&base, k);
    let by_origin = |k: FaultKind| {
        let g = |o: FaultOrigin| now.faults[k as usize][o as usize] - base.faults[k as usize][o as usize];
        format!(
            "guest {:.1} host {:.1} internal {:.1}",
            g(FaultOrigin::Guest) as f64 / n,
            g(FaultOrigin::Host) as f64 / n,
            g(FaultOrigin::Internal) as f64 / n
        )
    };
    let peak = st
        .peak_members
        .fetch_max(now.members, std::sync::atomic::Ordering::Relaxed)
        .max(now.members);
    eprintln!(
        "DIAG private track frame {frame}: members {} pages ({:.1} MiB, peak {:.1} MiB, {} new in {} frames), \
         the frame may write {:.0} pages ({:.2} MiB) in {:.1} runs, {} ranges over the limits",
        now.members,
        mib(now.members as f64),
        mib(peak as f64),
        w.new_members,
        w.flushes,
        w.written_pages as f64 / n,
        mib(w.written_pages as f64 / n),
        w.written_runs as f64 / n,
        w.clipped,
    );
    eprintln!(
        "DIAG private faults per frame: cpu write to a clean page {:.1} ({}), write in flight {:.1} ({}), \
         read in flight {:.1} ({}), other (races{}) {:.1}; in the handler {:.3} ms per frame",
        d(FaultKind::CleanWrite) as f64 / n,
        by_origin(FaultKind::CleanWrite),
        d(FaultKind::WriteInFlight) as f64 / n,
        by_origin(FaultKind::WriteInFlight),
        d(FaultKind::ReadInFlight) as f64 / n,
        by_origin(FaultKind::ReadInFlight),
        if mirrored() { " and pages copied from the mirror" } else { "" },
        (d(FaultKind::Other) + d(FaultKind::Sync)) as f64 / n,
        (now.handler_ns - base.handler_ns) as f64 / n / 1e6,
    );
    eprintln!(
        "DIAG private per frame: pages dirtied {:.1} ({:.2} MiB; {:.1} of them by system calls), \
         system call accesses in flight read {:.1} write {:.1}; flush {:.3} ms (plan {:.3}), \
         mprotect {:.1} calls ({:.1} in the flush) {:.3} ms outside the handler, failed {}, \
         log overflows {}{}",
        w.dirtied as f64 / n,
        mib(w.dirtied as f64 / n),
        (now.hook_dirtied - base.hook_dirtied) as f64 / n,
        (now.host_access[0] - base.host_access[0]) as f64 / n,
        (now.host_access[1] - base.host_access[1]) as f64 / n,
        w.flush_ns as f64 / n / 1e6,
        w.plan_ns as f64 / n / 1e6,
        (now.mprotects - base.mprotects) as f64 / n,
        w.mprotects as f64 / n,
        (now.mprotect_ns - base.mprotect_ns) as f64 / n / 1e6,
        now.mprotect_failed - base.mprotect_failed,
        now.log_overflows - base.log_overflows,
        if now.pc_overflow > base.pc_overflow {
            format!(", program counters over the table {}", now.pc_overflow - base.pc_overflow)
        } else {
            String::new()
        },
    );
    if mirrored() {
        let (usable, runs, retired) = locked(&st.mirror)
            .as_ref()
            .map_or((0, 0, 0), |m| (m.mirror.usable_bytes(), m.mirror.runs().len(), m.mirror.retired()));
        let partial_now = crate::overlay::partial_table();
        let partial: u64 = partial_now.iter().flatten().sum::<u64>() - w.partial_base.iter().flatten().sum::<u64>();
        let sync = |o: FaultOrigin| now.faults[FaultKind::Sync as usize][o as usize]
            - base.faults[FaultKind::Sync as usize][o as usize];
        eprintln!(
            "DIAG private mirror frame {frame}: mirror {:.1} MiB usable in {runs} runs ({retired} buffers retired), \
             members {} pages; per frame: staged {:.1} pages ({:.2} MiB, {:.2} written again), copy-in {:.1} pages \
             (lost {}), pages copied mirror to guest {:.1} (by a fault of guest code {:.1}), closure added {:.1} pages, \
             waits for a writer {:.2} ({:.3} ms; {} timeouts), internal opens {:.2}, enter waits {}, \
             partial resolves {} in the window, poison {}",
            usable as f64 / (1024.0 * 1024.0),
            now.members,
            w.staged as f64 / n,
            mib(w.staged as f64 / n),
            w.raced as f64 / n,
            w.copy_in_pages as f64 / n,
            w.copy_in_lost,
            (now.synced - base.synced) as f64 / n,
            sync(FaultOrigin::Guest) as f64 / n,
            w.closure_pages as f64 / n,
            (now.writer_waits - base.writer_waits) as f64 / n,
            (now.writer_wait_ns - base.writer_wait_ns) as f64 / n / 1e6,
            now.wait_timeouts - base.wait_timeouts,
            (now.internal_opens - base.internal_opens) as f64 / n,
            w.enter_waits,
            partial,
            if poison_on() { "on" } else { "off" },
        );
    }
    if mirrored() {
        let t = crate::overlay::partial_table();
        let mut parts: Vec<String> = Vec::new();
        for (c, row) in t.iter().enumerate() {
            for (k, v) in row.iter().enumerate() {
                let d = v - w.partial_base[c][k];
                if d != 0 {
                    parts.push(format!(
                        "{} {}: {:.2}",
                        crate::overlay::CLASS_NAMES[c],
                        crate::overlay::SPLIT_NAMES[k],
                        d as f64 / n
                    ));
                }
            }
        }
        eprintln!(
            "DIAG private partial resolves per frame: {} ({} ranges learned for the closure, {} partial resolves of a \
             learned range in the window: a limit of the closure kept it split)",
            if parts.is_empty() { "none".to_string() } else { parts.join(", ") },
            crate::overlay::partial_range_count(),
            crate::overlay::partial_repeats() - w.repeats_base
        );
        let ms = |v: u64| v as f64 / n / 1e6;
        let handler = (now.handler_ns - base.handler_ns) as f64 / n / 1e6;
        let waited = (now.writer_wait_ns - base.writer_wait_ns) as f64 / n / 1e6;
        eprintln!(
            "DIAG private cost per frame, game thread: flush {:.3} ms = plan of written ranges {:.3} + read ranges {:.3} + \
             closure {:.3} + mirror growth {:.3} + pages {:.3} (new members {:.3}, dirty log {:.3}, staging the dirty pages \
             {:.3}, writable pages {:.3}, tags of the written pages {:.3}; mprotect in all of it {:.3}) + rest {:.3}; fault handler {:.3} ms (of it \
             waiting for writers {:.3}); the model costs {:.3} ms per frame",
            ms(w.t_total),
            ms(w.t_plan_written),
            ms(w.t_reads),
            ms(w.t_closure),
            ms(w.t_ensure),
            ms(w.t_pages),
            ms(w.t_new),
            ms(w.t_log),
            ms(w.t_stage),
            ms(w.t_hot),
            ms(w.t_tag),
            ms(w.t_prot),
            ms(w.t_rest),
            handler,
            waited,
            ms(w.t_total) + handler,
        );
        eprintln!(
            "DIAG private adaptive per frame: writable pages {:.1} (staged {:.1}, unchanged {:.1}; {:.2} promoted, {:.2} \
             protected again); copy-back after completion {:.1} pages in {:.2} mprotect calls, {:.3} ms on its own thread",
            w.hot_pages as f64 / n,
            w.hot_staged as f64 / n,
            w.hot_same as f64 / n,
            w.promoted as f64 / n,
            w.demoted as f64 / n,
            (now.eager_pages - base.eager_pages) as f64 / n,
            (now.eager_runs - base.eager_runs) as f64 / n,
            (now.eager_ns - base.eager_ns) as f64 / n / 1e6,
        );
        let ph = |i: usize| (now.phase_writes[i] - base.phase_writes[i]) as f64 / n;
        let longest = pages.take_wait_max();
        let waits = |k: usize| {
            format!(
                "{:.2} ({:.2} ms, longest {:.2} ms, {} began before the commit)",
                (now.waits[k] - base.waits[k]) as f64 / n,
                (now.wait_ns[k] - base.wait_ns[k]) as f64 / n / 1e6,
                longest[k] as f64 / 1e6,
                now.wait_uncommitted[k] - base.wait_uncommitted[k],
            )
        };
        let full = |i: usize| WAITS.full[i].swap(0, Ordering::Relaxed);
        let (early, bounded) = (WAITS.early.swap(0, Ordering::Relaxed), WAITS.bounded.swap(0, Ordering::Relaxed));
        let (bounded_ns, bounded_max) = (
            WAITS.bounded_ns.swap(0, Ordering::Relaxed),
            WAITS.bounded_max_ns.swap(0, Ordering::Relaxed),
        );
        eprintln!(
            "DIAG private fences frame {frame}: {early} waits answered at the flush ({bounded} of them also waited for \
             the frame {} before (NXKIT_PRIVATE_AHEAD), {:.3} ms per frame, longest {:.2} ms); full waits: privhonest {}, copy-back {}, \
             idle GPU {}, flush not finished {}",
            run_ahead(),
            bounded_ns as f64 / n / 1e6,
            bounded_max as f64 / 1e6,
            full(0),
            full(1),
            full(2),
            full(3),
        );
        eprintln!(
            "DIAG private CPU waits for a writer, per frame: read in flight {}; write in flight {}",
            waits(0),
            waits(1),
        );
        if pages.late() {
            let g = |f: fn(&PageStats) -> u64| f(&now) - f(&base);
            let (reads, writes) = (g(|s| s.late_reads), g(|s| s.late_writes));
            let hist = |i: usize| now.late_age[i] - base.late_age[i];
            eprintln!(
                "DIAG private late results, per frame: late reads {:.2}, late writes {:.2} ({:.2} blocks taken from the frame, \
                 {:.2} blocks of copies left alone for them); age of the bytes served: mean {:.2} frames, longest {} (1 frame {}, \
                 2 {}, 3 {}, 4 or more {}); copied at completions {:.2} pages ({:.1} blocks), by the flush {:.2} pages; waits \
                 for a writer {:.2} per frame (should be about 0)",
                reads as f64 / n,
                writes as f64 / n,
                g(|s| s.late_owned) as f64 / n,
                g(|s| s.late_kept) as f64 / n,
                g(|s| s.late_age_sum) as f64 / (reads + writes).max(1) as f64,
                pages.take_late_age_max(),
                hist(0),
                hist(1),
                hist(2),
                hist(3),
                g(|s| s.late_copied) as f64 / n,
                g(|s| s.late_copied_blocks) as f64 / n,
                g(|s| s.late_flush_copies) as f64 / n,
                g(|s| s.writer_waits) as f64 / n,
            );
        }
        let by_end = |k: usize| {
            format!(
                "{:.2} ({:.2} ms)",
                (now.wait_ends[k] - base.wait_ends[k]) as f64 / n,
                (now.wait_end_ns[k] - base.wait_end_ns[k]) as f64 / n / 1e6,
            )
        };
        eprintln!(
            "DIAG private pass signals, per frame: waits ended by the signal of the page's last pass (the frame still ran) {}, \
             by the completion of the whole frame {}; accesses that found their page already released when they first faulted (the signal came before the reader) {:.2}; {} CPU-read GPU results \
             known now, the set changed {} times in the window; frames cut {:.2} ({:.2} cuts each, at {:.0}% of the passes on \
             average, {:.1} pages released per frame, {} of them committed while the rest was encoded); readers per frame {:.2}, \
             without a cut {:.2}; planning {:.3} ms",
            by_end(0),
            by_end(1),
            (now.early_syncs - base.early_syncs) as f64 / n,
            now.readers,
            now.reader_changes - base.reader_changes,
            w.early_frames as f64 / n,
            w.early_cuts as f64 / w.early_frames.max(1) as f64,
            100.0 * w.early_pos / w.early_cuts.max(1) as f64,
            w.early_pages as f64 / n,
            w.early_ahead,
            w.early_readers as f64 / n,
            w.early_unserved as f64 / n,
            w.early_plan_ns as f64 / n / 1e6,
        );
        if pages.emulating() {
            let per = |f: AccessForm| now.emulated[f as usize] - base.emulated[f as usize];
            let part = |loads: bool| {
                let list: Vec<String> = AccessForm::ALL
                    .iter()
                    .filter(|f| f.is_load() == loads && per(**f) > 0)
                    .map(|f| format!("{} {:.2}", f.name(), per(*f) as f64 / n))
                    .collect();
                let sum: u64 = AccessForm::ALL.iter().filter(|f| f.is_load() == loads).map(|f| per(*f)).sum();
                format!(
                    "{:.2} ({})",
                    sum as f64 / n,
                    if list.is_empty() { "none".to_string() } else { list.join(", ") }
                )
            };
            let missed: Vec<String> = FallbackReason::ALL
                .iter()
                .filter(|r| now.fallbacks[**r as usize] > base.fallbacks[**r as usize])
                .map(|r| {
                    format!(
                        "{} {:.2}",
                        r.name(),
                        (now.fallbacks[*r as usize] - base.fallbacks[*r as usize]) as f64 / n
                    )
                })
                .collect();
            eprintln!(
                "DIAG private emulated accesses per frame (outside the bytes a frame in flight writes, done in the handler \
                 without a wait): loads {}; stores {}; {:.3} ms in the handler for them; pages the flush staged for the \
                 stores {:.2}; not emulated, they waited for the writer: {}",
                part(true),
                part(false),
                (now.emulated_ns - base.emulated_ns) as f64 / n / 1e6,
                (now.emu_staged - base.emu_staged) as f64 / n,
                if missed.is_empty() { "none".to_string() } else { missed.join(", ") },
            );
            let tops = pages.take_top_fallback_pcs(TOP);
            if !tops.is_empty() {
                let line: Vec<String> = tops
                    .iter()
                    .map(|&(pc, c)| format!("{} {:.1}", nxkit_metal::name_pc(pc), c as f64 / n))
                    .collect();
                eprintln!("DIAG private top program counters, not emulated (per frame): {}", line.join(", "));
            }
        }
        eprintln!(
            "DIAG private writes to member pages per frame (first write of a page): after the GPU was done or before the \
             next flush {:.1}, between the flush and the commit {:.1}, after the commit before the GPU was done {:.1}; \
             staged a second time (late captures for forced submits, privlatestage) {:.1}",
            ph(0),
            ph(1),
            ph(2),
            w.late_staged as f64 / n,
        );
        let n_between = w.between_frames.max(1) as f64;
        if w.late_frames > 0 || !late_enabled() {
            eprintln!(
                "DIAG private forced submits: {} frames had a flush that ran submits before the CPU signals they wait for; the \
                 encodes waited {:.2} ms per such frame for the signals, {} of the waits ended at the limit of {} ms{}",
                w.late_frames,
                w.late_wait_ns as f64 / w.late_frames.max(1) as f64 / 1e6,
                w.late_timeouts,
                LATE_WAIT.as_millis(),
                if late_enabled() { "" } else { "; the capture no longer waits for them (signals missed)" },
            );
        }
        report_in_flight(&mut w);
        let by_kind: Vec<String> = BIND_NAMES
            .iter()
            .zip(&w.between_reads)
            .filter(|(_, c)| **c > 0)
            .map(|(k, c)| format!("{k} {:.2}", *c as f64 / n_between))
            .collect();
        let reads_total: u64 = w.between_reads.iter().sum();
        let changed: Vec<String> = BIND_NAMES
            .iter()
            .zip(&w.changed_ranges)
            .filter(|(_, c)| **c > 0)
            .map(|(k, c)| format!("{k} {:.2}", *c as f64 / n_between))
            .collect();
        eprintln!(
            "DIAG private between flush and commit, exact: bound read ranges per frame whose bytes in a page the CPU wrote \
             since the flush differ at the commit from what the frame sees: {} ({} bytes in all; {} ranges could not be \
             compared)",
            if changed.is_empty() { "none".to_string() } else { changed.join(", ") },
            w.changed_bytes,
            w.uncompared,
        );
        eprintln!(
            "DIAG private between flush and commit, per frame, first writes of the CPU to member pages: {:.2} in all: {:.2} in \
             bytes the frame writes, {:.2} in bytes it reads through a binding ({}), {:.2} in a page of one of its bindings but \
             outside its bytes, {:.2} in no binding of the frame; {} of {} frames had a flush that ran submits before the CPU \
             signal they wait for ({:.2} writes into read bytes per such frame)",
            w.between_pages as f64 / n_between,
            w.between_written as f64 / n_between,
            reads_total as f64 / n_between,
            if by_kind.is_empty() { "none".to_string() } else { by_kind.join(", ") },
            w.between_same_page as f64 / n_between,
            w.between_other as f64 / n_between,
            w.forced_frames,
            w.between_frames,
            w.forced_reads as f64 / w.forced_frames.max(1) as f64,
        );
        let tops = pages.take_top_between_pcs(TOP);
        if !tops.is_empty() {
            let line: Vec<String> = tops
                .iter()
                .map(|&(pc, c)| format!("{} {:.1}", nxkit_metal::name_pc(pc), c as f64 / n))
                .collect();
            eprintln!("DIAG private top program counters, write between flush and commit (per frame): {}", line.join(", "));
        }
    }
    for (kind, label) in [
        (FaultKind::WriteInFlight, "write in flight"),
        (FaultKind::ReadInFlight, "read in flight"),
        (FaultKind::CleanWrite, "write to a clean page"),
    ] {
        let tops = pages.take_top_pcs(kind, TOP);
        if tops.is_empty() {
            continue;
        }
        let line: Vec<String> = tops
            .iter()
            .map(|&(pc, c)| format!("{} {:.1}", nxkit_metal::name_pc(pc), c as f64 / n))
            .collect();
        eprintln!("DIAG private top program counters, {label} (per frame): {}", line.join(", "));
    }
    *w = Window {
        base: now,
        partial_base: crate::overlay::partial_table(),
        repeats_base: crate::overlay::partial_repeats(),
        ..Window::default()
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::{DispatchDetail, UboDetail};

    #[test]
    fn unknown_programs_count_as_writing_and_known_read_only_ones_do_not() {
        let mut rw = Rw::default();
        let slot_stage = 3 | (Stage::Fragment as u32) << 24;
        assert!(rw.written(Some(7), slot_stage), "unknown");
        assert!(rw.written(None, slot_stage), "no program (mesh stage)");
        rw.known.insert((7, slot_stage), false);
        assert!(!rw.written(Some(7), slot_stage));
        rw.known.insert((8, slot_stage), true);
        assert!(rw.written(Some(8), slot_stage));
    }

    #[test]
    fn the_written_ranges_cover_stores_copies_and_learned_pools() {
        use crate::detail::{CopyDetail, DispatchDetail, UboDetail};
        let ubo = |address, size, slot| UboDetail {
            stage: Stage::Compute,
            slot,
            address,
            size,
            snapshot: None,
        };
        let detail = FrameDetail {
            dispatches: vec![DispatchDetail {
                program: "cs".into(),
                push: None,
                groups: [1, 1, 1],
                indirect: None,
                ubos: Vec::new(),
                sbufs: vec![ubo(0x1000, 64, 0), ubo(0x2000, 64, 1)],
                textures: Vec::new(),
                images: Vec::new(),
            }],
            copies: vec![
                CopyDetail {
                    kind: CopyKind::BufferFill,
                    dst_addr: 0x3000,
                    bytes: 32,
                    ..Default::default()
                },
                CopyDetail {
                    kind: CopyKind::TextureToTexture,
                    dst_addr: 0x9000,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut rw = Rw::default();
        // Slot 1 of the compute program is read only.
        rw.known
            .insert((hash_str("cs"), 1 | (Stage::Compute as u32) << 24), false);
        let r = written_ranges(&detail, &[(0x5000, 0x6000)], &rw);
        assert!(r.contains(&(0x1000, 64)), "unknown slot counts as writing");
        assert!(!r.iter().any(|x| x.0 == 0x2000), "read only slot is left out");
        assert!(r.contains(&(0x3000, 32)));
        assert!(!r.iter().any(|x| x.0 == 0x9000), "a texture copy writes no buffer");
        assert!(r.contains(&(0x5000, 0x1000)), "a learned pool is whole");
    }

    // ----- the mirror -----

    use super::TEST_TURN as TURN;
    const VA: u64 = nxkit_metal::GUEST_VA_BASE;

    struct Guest {
        mem: Arc<GuestMemory>,
        map: AddressMap,
        ctx: Context,
    }

    fn guest(len: u64) -> Option<Guest> {
        let ctx = Context::system_default().ok()?;
        let mem = Arc::new(GuestMemory::allocate(&ctx, len).ok()?);
        let map = AddressMap::new(mem.host_base(), VA, mem.len()).ok()?;
        FORCE.with(|f| f.set(Some(Mode::Mirror)));
        FAILED.store(false, Ordering::Relaxed);
        attach(&ctx, mem.host_base(), mem.len(), VA);
        // These tests are about the waits for a writer; the late results are tested in `nxkit-metal`.
        Pages::global().set_late(false);
        Some(Guest { mem, map, ctx })
    }

    fn dispatch(addr: u64, size: u32) -> FrameDetail {
        let ubo = |address, size, slot| UboDetail {
            stage: Stage::Compute,
            slot,
            address,
            size,
            snapshot: None,
        };
        FrameDetail {
            dispatches: vec![DispatchDetail {
                program: "cs".into(),
                push: None,
                groups: [1, 1, 1],
                indirect: None,
                ubos: vec![ubo(addr + 0x8000, 64, 0)],
                sbufs: vec![ubo(addr, size, 0)],
                textures: Vec::new(),
                images: Vec::new(),
            }],
            ..Default::default()
        }
    }

    fn pools_of(g: &Guest, o: Arc<GuestOverlay>) -> crate::guestpools::GuestPools {
        crate::guestpools::GuestPools::whole(VA, g.mem.len(), g.mem.clone()).with_overlay(o)
    }

    #[test]
    fn a_written_binding_and_a_uniform_that_touches_it_resolve_to_the_mirror() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        let page = PAGE;
        // A storage binding of 1 KiB in page 10; its uniform is 0x8000 further (page 12), not a member.
        let addr = VA + 10 * page + 0x100;
        g.mem.write(10 * page + 0x100, &[7; 16]).unwrap();
        let detail = dispatch(addr, 0x400);
        let o = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
        let pools = pools_of(&g, o.clone());
        let loc = pools.resolve(addr, 0x400).unwrap();
        assert_ne!(loc.run, crate::pools::NO_RUN, "the written range lies in the mirror");
        let loc_ubo = pools.resolve_start(addr + 0x8000).unwrap();
        assert_eq!(loc_ubo.run, crate::pools::NO_RUN, "an untouched uniform stays in guest memory");
        // The mirror run holds the guest bytes as they were at the flush.
        let run = &o.runs()[loc.run as usize];
        let mut b = [0u8; 16];
        run.mem.read(loc.offset, &mut b).unwrap();
        assert_eq!(b, [7; 16]);
        assert_eq!(Pages::global().state_of(g.mem.host_base() + 10 * page), "gpu(1)");
        // The GPU writes the mirror and the frame completes; the CPU's read copies the page.
        run.mem.write(loc.offset, &[9; 16]).unwrap();
        encoded(1, None);
        let mut seen = [0u8; 16];
        g.mem.read(10 * page + 0x100, &mut seen).unwrap();
        assert_eq!(seen, [9; 16], "the read of the stale page found the GPU's bytes");
        assert_eq!(Pages::global().state_of(g.mem.host_base() + 10 * page), "clean");
        FORCE.with(|f| f.set(None));
    }

    #[test]
    fn a_range_that_a_binding_reaches_into_a_member_makes_its_pages_members_too() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        let page = PAGE;
        // The storage binding is in page 20; a uniform that starts in page 19 and ends in page 20 touches it.
        let mut detail = dispatch(VA + 20 * page, 64);
        detail.dispatches[0].ubos[0].address = VA + 20 * page - 0x100;
        detail.dispatches[0].ubos[0].size = 0x200;
        let o = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
        assert!(o.member(VA + 19 * page + 0x3f00), "the part before the member is a member");
        assert!(o.all_members(VA + 20 * page - 0x100, 0x200));
        let pools = pools_of(&g, o);
        let loc = pools.resolve(VA + 20 * page - 0x100, 0x200).unwrap();
        assert_ne!(loc.run, crate::pools::NO_RUN);
        assert_eq!(crate::overlay::partial(), crate::overlay::partial());
        encoded(1, None);
        FORCE.with(|f| f.set(None));
    }

    #[test]
    fn a_page_the_cpu_wrote_is_staged_and_the_copy_in_pass_moves_it_into_the_mirror() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        let Ok(pipelines) = Pipelines::new(&g.ctx, &nxkit_metal::ArchiveConfig::default()) else {
            return;
        };
        let page = PAGE;
        let addr = VA + 30 * page;
        let detail = dispatch(addr, 0x100);
        // Frame 1 makes page 30 a member and the GPU writes it.
        let o1 = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
        let pools = pools_of(&g, o1.clone());
        let loc = pools.resolve(addr, 0x100).unwrap();
        let run = &o1.runs()[loc.run as usize];
        run.mem.write(loc.offset, &[5; 8]).unwrap();
        encoded(1, None);
        // The CPU reads (the page is copied), then writes a byte next to the GPU's: the page is dirty.
        let mut b = [0u8; 8];
        g.mem.read(30 * page, &mut b).unwrap();
        assert_eq!(b, [5; 8]);
        g.mem.write(30 * page + 8, &[6; 8]).unwrap();
        assert_eq!(Pages::global().state_of(g.mem.host_base() + 30 * page), "dirty");
        // Frame 2 stages the page whole, and the first command buffer of the frame copies it into the mirror.
        let o2 = at_flush(&detail, 2, &g.map, 0, false, &|| {}).expect("overlay");
        let staged = o2.staged().expect("a staged page");
        assert_eq!(staged.slots.len(), 1);
        assert_eq!(Pages::global().state_of(g.mem.host_base() + 30 * page), "gpu(2, fresh)");
        // The mirror still holds the state before the CPU's write.
        let pools2 = pools_of(&g, o2.clone());
        let loc2 = pools2.resolve(addr, 0x100).unwrap();
        let run2 = &o2.runs()[loc2.run as usize];
        let mut m = [0u8; 16];
        run2.mem.read(loc2.offset, &mut m).unwrap();
        assert_eq!(&m[8..], &[0; 8]);
        // The frame's view reads the staged bytes for the CPU's page.
        let mut v = [0u8; 16];
        assert!(o2.read(addr, &mut v));
        assert_eq!(&v[..8], &[5; 8]);
        assert_eq!(&v[8..], &[6; 8]);
        let (jobs, lost) = copy_in_jobs(&o2);
        assert_eq!((jobs.len(), lost), (1, 0));
        let scratch = Scratch::new(&g.ctx, 1 << 16).unwrap();
        let table = ArgumentTable::new(&g.ctx, 31, 0).unwrap();
        let mut ring = nxkit_metal::FrameRing::new(&g.ctx).unwrap();
        let mut frame = ring.begin().unwrap();
        assert_eq!(encode_copy_in(&pipelines, frame.commands(), &table, &scratch, &jobs).unwrap(), 1);
        g.ctx.commit_residency();
        frame.submit().unwrap().wait().unwrap();
        run2.mem.read(loc2.offset, &mut m).unwrap();
        assert_eq!(&m[..8], &[5; 8], "the GPU's bytes are kept");
        assert_eq!(&m[8..], &[6; 8], "and the CPU's landed");
        encoded(2, None);
        FORCE.with(|f| f.set(None));
    }

    #[test]
    fn a_cpu_store_outside_the_bytes_a_frame_writes_needs_no_wait_and_reaches_the_next_frame_beside_the_gpus_bytes() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        let Ok(pipelines) = Pipelines::new(&g.ctx, &nxkit_metal::ArchiveConfig::default()) else {
            return;
        };
        if !Pages::global().emulating() {
            eprintln!("skipped: no second mapping of guest memory here");
            return;
        }
        let page = PAGE;
        let addr = VA + 50 * page;
        let detail = dispatch(addr, 0x100);
        // Frame 1 writes the first 0x100 bytes of page 50; the GPU writes eight of them in the mirror.
        let o1 = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
        let pools = pools_of(&g, o1.clone());
        let loc = pools.resolve(addr, 0x100).unwrap();
        o1.runs()[loc.run as usize].mem.write(loc.offset, &[5; 8]).unwrap();
        // The frame is in flight. The CPU stores and loads elsewhere in the page and does not wait.
        let s0 = Pages::global().stats();
        let t0 = Instant::now();
        g.mem.write(50 * page + 0x2000, &[9; 8]).unwrap();
        let mut b = [0u8; 8];
        g.mem.read(50 * page + 0x3000, &mut b).unwrap();
        g.mem.read(50 * page + 0x2000, &mut b).unwrap();
        assert_eq!(b, [9; 8], "a load sees the store");
        assert!(t0.elapsed() < std::time::Duration::from_millis(500), "no wait for the frame");
        let s1 = Pages::global().stats();
        assert_eq!(s1.writer_waits, s0.writer_waits);
        assert!(s1.emulated.iter().sum::<u64>() > s0.emulated.iter().sum::<u64>(), "the handler did them");
        // Frame 2 stages the page without the blocks that frame 1 writes.
        let o2 = at_flush(&detail, 2, &g.map, 0, false, &|| {}).expect("overlay");
        let staged = o2.staged().expect("a staged page");
        assert_eq!(staged.slots.len(), 1);
        assert_eq!(staged.skip_of(50), Some([0b1111, 0, 0, 0]));
        // The frame's view: the CPU's bytes from the staging buffer, the GPU's from the mirror.
        let mut v = [0u8; 8];
        assert!(o2.read(addr + 0x2000, &mut v));
        assert_eq!(v, [9; 8]);
        assert!(o2.read(addr, &mut v));
        assert_eq!(v, [5; 8]);
        let (jobs, lost) = copy_in_jobs(&o2);
        assert_eq!((jobs.len(), lost), (1, 0));
        assert_eq!(jobs[0].2, [0b1111, 0, 0, 0]);
        let scratch = Scratch::new(&g.ctx, 1 << 16).unwrap();
        let table = ArgumentTable::new(&g.ctx, 31, 0).unwrap();
        let mut ring = nxkit_metal::FrameRing::new(&g.ctx).unwrap();
        let mut frame = ring.begin().unwrap();
        assert_eq!(encode_copy_in(&pipelines, frame.commands(), &table, &scratch, &jobs).unwrap(), 1);
        g.ctx.commit_residency();
        frame.submit().unwrap().wait().unwrap();
        let pools2 = pools_of(&g, o2.clone());
        let loc2 = pools2.resolve(addr, 0x100).unwrap();
        let run2 = &o2.runs()[loc2.run as usize];
        let mut m = [0u8; 8];
        run2.mem.read(loc2.offset, &mut m).unwrap();
        assert_eq!(m, [5; 8], "the GPU's bytes were not overwritten by the stale guest bytes");
        let loc3 = pools2.resolve(addr + 0x2000, 8).unwrap();
        o2.runs()[loc3.run as usize].mem.read(loc3.offset, &mut m).unwrap();
        assert_eq!(m, [9; 8], "the CPU's store landed in the mirror");
        encoded(1, None);
        // After frame 1 the CPU reads its bytes: the GPU's arrive, the store stays.
        encoded(2, None);
        g.mem.read(50 * page, &mut b).unwrap();
        assert_eq!(b, [5; 8]);
        g.mem.read(50 * page + 0x2000, &mut b).unwrap();
        assert_eq!(b, [9; 8]);
        FORCE.with(|f| f.set(None));
    }

    #[test]
    fn the_poison_knob_fills_the_stale_guest_bytes_of_a_page_a_host_thread_reads_in_flight() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        FORCE_POISON.with(|f| f.set(true));
        let page = PAGE;
        let addr = VA + 40 * page;
        // The page is a member; the CPU writes it, and the frame that writes it starts: it is fresh, guest memory holds
        // its bytes, and the poison covers them.
        let o = at_flush(&dispatch(addr, 0x100), 1, &g.map, 0, false, &|| {}).expect("overlay");
        encoded(1, None);
        g.mem.write(40 * page + 4, &[3; 4]).unwrap();
        let o2 = at_flush(&dispatch(addr, 0x100), 2, &g.map, 0, false, &|| {}).expect("overlay");
        assert!(o2.staged().is_some());
        drop(o);
        let host = g.mem.host_base() + 40 * page;
        let seen = std::thread::spawn(move || {
            let _g = nxkit_metal::mark_internal_thread();
            // SAFETY of the read is the tracker's: the page is open for reading after the fault.
            let mut b = [0u8; 8];
            // The page is in flight and fresh: the internal thread gets guest memory as it is.
            nxkit_abi::guest::set_range(host - 40 * page, 64 << 20);
            nxkit_abi::guest::with_slice(host, 8, |s| b.copy_from_slice(s)).unwrap();
            b
        })
        .join()
        .unwrap();
        nxkit_abi::guest::set_range(0, 0);
        assert_eq!(seen, [0x00, 0x00, 0xc0, 0x7f, 0x00, 0x00, 0xc0, 0x7f], "the pattern, not the CPU's bytes");
        encoded(2, None);
        FORCE_POISON.with(|f| f.set(false));
        FORCE.with(|f| f.set(None));
    }

    #[test]
    fn a_may_write_page_the_gpu_did_not_write_equals_the_mirror_after_the_frame() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        let page = PAGE;
        // The program is unknown, so the binding counts as written; the GPU writes nothing.
        let addr = VA + 60 * page;
        g.mem.write(60 * page + 8, &[0x21; 8]).unwrap();
        let o = at_flush(&dispatch(addr, 0x100), 1, &g.map, 0, false, &|| {}).expect("overlay");
        encoded(1, None);
        let mut b = [0u8; 8];
        g.mem.read(60 * page + 8, &mut b).unwrap();
        assert_eq!(b, [0x21; 8]);
        let pools = pools_of(&g, o.clone());
        let loc = pools.resolve(addr, 0x100).unwrap();
        let mut m = [0u8; 16];
        o.runs()[loc.run as usize].mem.read(loc.offset + 8 - 8, &mut m).unwrap();
        assert_eq!(&m[8..], &[0x21; 8]);
        FORCE.with(|f| f.set(None));
    }

    #[test]
    fn with_poison_threads_that_read_after_the_flush_wait_and_get_the_real_bytes() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        FORCE_POISON.with(|f| f.set(true));
        let page = PAGE;
        // 200 member pages that the frame writes; the CPU wrote each before.
        let first = 100u64;
        for p in 0..200u64 {
            g.mem.write((first + p) * page, &[p as u8 + 1; 8]).unwrap();
        }
        let detail = dispatch(VA + first * page, (200 * page) as u32);
        let _o = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
        let mem = g.mem.clone();
        let readers: Vec<_> = (0..4)
            .map(|t| {
                let mem = mem.clone();
                std::thread::spawn(move || {
                    let mut bad = 0;
                    for p in (t..200u64).step_by(4) {
                        let mut b = [0u8; 8];
                        mem.read((first + p) * page, &mut b).unwrap();
                        if b != [p as u8 + 1; 8] {
                            bad += 1;
                        }
                    }
                    bad
                })
            })
            .collect();
        std::thread::sleep(std::time::Duration::from_millis(50));
        encoded(1, None);
        let bad: u32 = readers.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(bad, 0, "no reader saw the pattern");
        FORCE_POISON.with(|f| f.set(false));
        FORCE.with(|f| f.set(None));
    }

    #[test]
    fn a_frame_with_forced_submits_gets_the_pages_the_cpu_writes_until_their_signal() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        let page = PAGE;
        // Page 90 is a member that frame 40 wrote and finished; frame 41 writes page 80 only.
        let _o40 = at_flush(&dispatch(VA + 90 * page, 0x100), 40, &g.map, 0, false, &|| {}).expect("overlay");
        encoded(40, None);
        late_expect(41, 1);
        let o = at_flush(&dispatch(VA + 80 * page, 0x100), 41, &g.map, 1, true, &|| {}).expect("overlay");
        assert!(o.forced() && o.frame() == 41);
        // The CPU writes page 90 after the flush and before the signal that the forced submit waits for.
        g.mem.write(90 * page + 16, &[5; 8]).unwrap();
        assert_eq!(Pages::global().state_of(g.mem.host_base() + 90 * page), "dirty");
        late_signal(41);
        assert_eq!(Pages::global().state_of(g.mem.host_base() + 90 * page), "clean", "the capture took the page");
        let t0 = Instant::now();
        let jobs = late_stage(&o);
        assert!(t0.elapsed() < std::time::Duration::from_millis(10), "the signal came, no wait");
        assert_eq!(jobs.len(), 1, "one page goes into the mirror before the commit");
        // A write after the signal is the next frame's: it stays dirty for the next flush.
        g.mem.write(90 * page + 32, &[6; 8]).unwrap();
        assert_eq!(Pages::global().state_of(g.mem.host_base() + 90 * page), "dirty");
        encoded(41, None);
        FORCE.with(|f| f.set(None));
    }

    #[test]
    fn a_signal_that_comes_before_the_flush_counted_it_closes_the_capture_and_one_that_never_comes_times_out() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        let page = PAGE;
        let _o = at_flush(&dispatch(VA + 100 * page, 0x100), 50, &g.map, 0, false, &|| {}).expect("overlay");
        encoded(50, None);
        // The signal first, the count second.
        late_signal(51);
        late_expect(51, 1);
        let o = at_flush(&dispatch(VA + 110 * page, 0x100), 51, &g.map, 1, true, &|| {}).expect("overlay");
        let t0 = Instant::now();
        assert!(late_stage(&o).is_empty());
        assert!(t0.elapsed() < std::time::Duration::from_millis(10));
        encoded(51, None);
        // No signal: the encode waits for the limit, then goes on, and the miss is counted.
        let missed = LATE_MISSED.load(Ordering::Relaxed);
        late_expect(52, 1);
        let o = at_flush(&dispatch(VA + 120 * page, 0x100), 52, &g.map, 1, true, &|| {}).expect("overlay");
        let t0 = Instant::now();
        assert!(late_stage(&o).is_empty());
        assert!(t0.elapsed() >= std::time::Duration::from_millis(10), "waited for the signal");
        assert_eq!(LATE_MISSED.load(Ordering::Relaxed), missed + 1);
        LATE_MISSED.store(0, Ordering::Relaxed);
        encoded(52, None);
        FORCE.with(|f| f.set(None));
    }

    #[test]
    fn a_range_that_resolved_partly_on_members_is_made_whole_by_the_next_flush() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        let page = PAGE;
        // Page 50 is written by the frame and so a member; a texture-like source starts in page 49, which no binding
        // announces, and reaches into page 50.
        let detail = dispatch(VA + 50 * page, 0x100);
        let o1 = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
        let src = VA + 49 * page + 0x3000;
        let len = 0x2000u64;
        assert!(!o1.all_members(src, len));
        let pools = pools_of(&g, o1);
        let before = crate::overlay::partial_table();
        let _ = pools.resolve_c(src, len, crate::overlay::UPLOAD);
        let after = crate::overlay::partial_table();
        let upload = 4usize;
        assert_eq!(after[upload][1] - before[upload][1], 1, "counted as starting out and reaching in");
        encoded(1, None);
        // The next flush has the range from the encoder and makes it whole.
        let o2 = at_flush(&detail, 2, &g.map, 0, false, &|| {}).expect("overlay");
        assert!(o2.all_members(src, len), "the range is whole on members now");
        let pools = pools_of(&g, o2);
        let before = crate::overlay::partial_table();
        let loc = pools.resolve_c(src, len, crate::overlay::UPLOAD).unwrap();
        assert_ne!(loc.run, crate::pools::NO_RUN);
        assert_eq!(crate::overlay::partial_table(), before, "no partial resolve any more");
        encoded(2, None);
        FORCE.with(|f| f.set(None));
    }

    /// The live case in small: 71 bindings of 72 pages that every frame may write, 154 pages the CPU writes every frame
    /// that no frame touches and 77 pages of the written ones that it reads and writes every frame. The three settings give
    /// the same bytes; the times are printed.
    #[test]
    fn the_adaptive_settings_keep_the_bytes_exact_and_the_numbers_are_printed() {
        let _t = locked(&TURN);
        // The tests before this one left pages of their guest tracked and without access; the tracker still covers them.
        Pages::global().release_all();
        let Some(g) = guest(256 << 20) else { return };
        let page = PAGE;
        let mut dispatches = Vec::new();
        let mut written_pages: Vec<u64> = Vec::new();
        let mut hot: Vec<u64> = Vec::new();
        for i in 0..71u64 {
            let first = 400 + i * 100;
            let mut d = dispatch(VA + first * page, (72 * page) as u32);
            // A uniform that starts in the last written page and reaches 2 or 3 pages past it: those pages are members
            // that no frame writes.
            let extra = if i < 12 { 4 } else { 3 };
            d.dispatches[0].ubos = vec![UboDetail {
                stage: Stage::Compute,
                slot: 0,
                address: VA + (first + 71) * page + 0x100,
                size: (extra * page) as u32,
                snapshot: None,
            }];
            hot.extend((0..extra - 1).map(|k| first + 72 + k));
            dispatches.push(d.dispatches.remove(0));
            written_pages.push(first);
            if i < 6 {
                written_pages.push(first + 1);
            }
        }
        let detail = FrameDetail {
            dispatches,
            ..Default::default()
        };
        let pages = Pages::global();
        let mut fence = 0u64;
        let mut out = Vec::new();
        let mut results: Vec<(bool, bool, u64, u64)> = Vec::new();
        for (name, eager, hot_on) in [("off", false, false), ("copy-back", true, false), ("copy-back and writable", true, true)] {
            pages.set_adaptive(eager, hot_on);
            let (mut cpu_ns, mut flush_ns) = (0u128, 0u128);
            let frames = 80u64;
            let mut prev = 0u8;
            let s0 = pages.stats();
            for k in 0..frames {
                fence += 1;
                let v = (fence % 251) as u8 + 1;
                let t = Instant::now();
                for &h in &hot {
                    g.mem.write(h * page + 100, &[v]).unwrap();
                }
                for &w in &written_pages {
                    if k > 0 {
                        let mut b = [0u8; 1];
                        g.mem.read(w * page, &mut b).unwrap();
                        assert_eq!(b[0], prev, "{name}: the GPU's byte of the last frame");
                    }
                    g.mem.write(w * page + 512, &[v]).unwrap();
                }
                cpu_ns += t.elapsed().as_nanos();
                let t = Instant::now();
                let o = at_flush(&detail, fence, &g.map, 0, false, &|| {}).expect("overlay");
                flush_ns += t.elapsed().as_nanos();
                // The hot pages reach the frame's view with the CPU's byte.
                for &h in hot.iter().step_by(17) {
                    let mut b = [0u8; 1];
                    assert!(o.read(VA + h * page + 100, &mut b));
                    assert_eq!(b[0], v, "{name}: staged or mirrored byte of page {h}");
                }
                // The GPU writes the first byte of every written page.
                for &w in &written_pages {
                    let va = VA + w * page;
                    let run = o.runs().iter().find(|r| r.va_start <= va && va < r.va_end).expect("run");
                    run.mem.write(va - run.va_start, &[v]).unwrap();
                }
                encoded(fence, None);
                prev = v;
                std::thread::sleep(std::time::Duration::from_millis(3));
            }
            let s1 = pages.stats();
            eprintln!("--- {name}");
            print_report(fence);
            results.push((
                eager,
                hot_on,
                s1.faults.iter().flatten().sum::<u64>() - s0.faults.iter().flatten().sum::<u64>(),
                s1.eager_pages - s0.eager_pages,
            ));
            out.push(format!(
                "{name}: cpu {:.3} ms, flush {:.3} ms per frame, faults {:.1}, copied by faults {:.1}, copy-back {:.1} pages",
                cpu_ns as f64 / frames as f64 / 1e6,
                flush_ns as f64 / frames as f64 / 1e6,
                (s1.faults.iter().flatten().sum::<u64>() - s0.faults.iter().flatten().sum::<u64>()) as f64 / frames as f64,
                (s1.synced - s0.synced) as f64 / frames as f64,
                (s1.eager_pages - s0.eager_pages) as f64 / frames as f64,
            ));
        }
        pages.set_adaptive(false, false);
        pages.quiesce();
        // The next test allocates from the same heap: leave no page of this guest without access (the allocator may write
        // to the block again before the next `attach`).
        pages.release_all();
        assert_eq!(results[0].3, 0, "nothing copies back with the setting off");
        assert!(results[1].3 > 0, "the completion copies back the pages game threads waited for");
        assert!(results[1].2 < results[0].2, "fewer faults with the copy-back");
        assert!(results[2].2 < results[1].2, "fewer faults with writable pages");
        for l in out {
            eprintln!("adaptive test {l}");
        }
        FORCE.with(|f| f.set(None));
    }

    #[test]
    fn the_stage_pool_gives_a_buffer_back_when_its_frame_is_complete() {
        let Ok(ctx) = Context::system_default() else { return };
        let mut pool = StagePool::default();
        let a = pool.take(&ctx, 10).unwrap();
        assert!(a.len() >= 10 * PAGE);
        pool.busy.push((5, a.clone()));
        pool.reclaim(4);
        assert_eq!((pool.busy.len(), pool.free.len()), (1, 0));
        pool.reclaim(5);
        assert_eq!((pool.busy.len(), pool.free.len()), (0, 1));
        let b = pool.take(&ctx, 20).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "the same buffer serves a smaller need");
        // A bigger need makes a bigger buffer and drops the small ones.
        pool.free.push(b);
        let c = pool.take(&ctx, 100_000).unwrap();
        assert!(c.len() >= 100_000 * PAGE);
        assert!(pool.free.is_empty());
    }

    #[test]
    fn the_sink_grows_when_more_pages_come_than_it_was_told() {
        let Ok(ctx) = Context::system_default() else { return };
        let mut pool = StagePool::default();
        let mut sink = Sink::new(&ctx, &mut pool);
        sink.begin(1);
        let cap = sink.cap;
        for p in 0..(cap as u32 + 3) {
            sink.stage(p, &vec![p as u8; PAGE as usize]);
        }
        sink.stage(1, &vec![0xee; PAGE as usize]);
        assert!(!sink.failed);
        assert_eq!(sink.slots.len(), cap + 3);
        let buf = sink.buf.clone().unwrap();
        let mut b = [0u8; 4];
        buf.read(u64::from(sink.index[&2]) * PAGE, &mut b).unwrap();
        assert_eq!(b, [2; 4]);
        buf.read(u64::from(sink.index[&1]) * PAGE, &mut b).unwrap();
        assert_eq!(b, [0xee; 4], "a page that is staged again keeps the later bytes");
    }

    #[test]
    fn the_extent_of_a_stream_follows_the_draw() {
        let s = |stride, divisor, size| StreamDetail {
            index: 0,
            address: 0x1000,
            size,
            stride,
            divisor,
        };
        let arrays = DrawArgs::Arrays {
            first: 2,
            count: 10,
            instances: 4,
            base_instance: 0,
        };
        assert_eq!(stream_extent(&s(32, 0, 1 << 30), &arrays), 32 * 12);
        assert_eq!(stream_extent(&s(32, 2, 1 << 30), &arrays), 32 * 2);
        assert_eq!(stream_extent(&s(32, 0, 100), &arrays), 100, "never past the buffer");
        let indexed = DrawArgs::Indexed {
            index_type: 1,
            address: 0,
            count: 10,
            instances: 1,
            base_vertex: 0,
            base_instance: 0,
        };
        assert_eq!(stream_extent(&s(32, 0, 1 << 30), &indexed), UNKNOWN_BYTES);
    }

    #[test]
    fn page_runs_merge_and_answer_membership() {
        let mut r = PageRuns::default();
        r.add(&[(5, 8), (1, 3), (3, 4), (7, 10)]);
        assert_eq!(r.0, vec![(1, 4), (5, 10)]);
        assert!(r.contains(1) && r.contains(3) && r.contains(9));
        assert!(!r.contains(0) && !r.contains(4) && !r.contains(10));
    }

    // ----- step 6: pass signals -----

    #[test]
    fn the_cuts_follow_the_last_pass_that_writes_a_page_and_the_earliest_ones_are_kept() {
        // Passes 0..5; readers are pages 10, 20, 30, 40, 50, 60.
        let runs = |v: &[u32]| runs_of_pages(v);
        let pass_runs = vec![
            vec![(10, 11)],
            vec![(10, 11), (20, 21)],
            vec![(30, 31)],
            vec![(30, 31), (40, 41)],
            vec![(50, 51)],
            vec![(60, 61)],
        ];
        let readers = [10, 20, 30, 40, 50, 60];
        let (cuts, unserved) = plan_cuts(&readers, &pass_runs, &[], 8);
        assert_eq!(unserved, 1, "page 60 is written by the last pass");
        let at: Vec<usize> = cuts.iter().map(|c| c.after_pass).collect();
        assert_eq!(at, vec![1, 3, 4], "page 10 is written again by pass 1");
        assert_eq!(cuts[0].pages, runs(&[10, 20]));
        assert_eq!(cuts[1].pages, runs(&[30, 40]));
        // With two cuts at most the earliest stays and the last takes the rest.
        let (cuts, _) = plan_cuts(&readers, &pass_runs, &[], 2);
        assert_eq!(cuts.len(), 2);
        assert_eq!((cuts[0].after_pass, cuts[0].pages.clone()), (1, runs(&[10, 20])));
        assert_eq!((cuts[1].after_pass, cuts[1].pages.clone()), (4, runs(&[30, 40, 50])));
        // A page that some pass writes through a pool of raw stores has no cut.
        let (cuts, unserved) = plan_cuts(&readers, &pass_runs, &[(20, 21), (30, 31)], 8);
        assert_eq!(unserved, 3);
        assert_eq!(cuts.iter().map(|c| c.after_pass).collect::<Vec<_>>(), vec![1, 3, 4]);
        assert_eq!(cuts[0].pages, runs(&[10]));
        assert_eq!(cuts[1].pages, runs(&[40]));
        // A page that no pass writes has none either.
        let (cuts, unserved) = plan_cuts(&[99], &pass_runs, &[], 8);
        assert!(cuts.is_empty());
        assert_eq!(unserved, 1);
    }

    /// A frame of three passes (a dispatch, a buffer fill, a dispatch): the first dispatch writes `a`, the fill `z`, the last
    /// dispatch `y`.
    fn three_passes(a: u64, z: u64, y: u64) -> (nxkit_nvn::frame::Frame, nxkit_nvn::passes::PassPlan, FrameDetail) {
        use crate::detail::CopyDetail;
        use nxkit_nvn::frame::{Cmd, Frame, Span};
        let ubo = |address| UboDetail {
            stage: Stage::Compute,
            slot: 0,
            address,
            size: 64,
            snapshot: None,
        };
        let disp = |at| DispatchDetail {
            program: "cs".into(),
            push: None,
            groups: [1, 1, 1],
            indirect: None,
            ubos: Vec::new(),
            sbufs: vec![ubo(at)],
            textures: Vec::new(),
            images: Vec::new(),
        };
        let d = Cmd::Dispatch {
            reads: Span::default(),
            writes: Span::default(),
        };
        let frame = Frame {
            cmds: vec![d, Cmd::BufferCopy, d],
            ..Frame::default()
        };
        let mut plan = nxkit_nvn::passes::PassPlan::default();
        nxkit_nvn::analyse::Analyser::new()
            .analyse(&frame, &nxkit_nvn::resource::ResourceTable::new(), &mut plan)
            .unwrap();
        let detail = FrameDetail {
            dispatches: vec![disp(a), disp(y)],
            copies: vec![CopyDetail {
                kind: CopyKind::BufferFill,
                dst_addr: z,
                bytes: 32,
                ..Default::default()
            }],
            ..Default::default()
        };
        (frame, plan, detail)
    }

    /// Reads one byte of `page` on a thread of its own.
    fn read_on_thread(g: &Guest, page: u64) -> std::thread::JoinHandle<u8> {
        let mem = g.mem.clone();
        std::thread::spawn(move || {
            let mut b = [0u8; 1];
            mem.read(page * PAGE, &mut b).unwrap();
            b[0]
        })
    }

    #[test]
    fn the_planner_cuts_a_real_frame_after_the_passes_that_write_the_pages_a_thread_waited_for() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        let (page_a, page_z, page_y) = (80u64, 81u64, 82u64);
        let (frame, plan, detail) = three_passes(VA + page_a * PAGE, VA + page_z * PAGE, VA + page_y * PAGE);
        assert_eq!(plan.passes.len(), 3, "dispatch, fill, dispatch");
        let _ = Pages::global().readers(0);
        // Frame 1: three threads wait for the writers of the three pages.
        let _o = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
        let readers: Vec<_> = [page_a, page_z, page_y].iter().map(|&p| read_on_thread(&g, p)).collect();
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(readers.iter().all(|r| !r.is_finished()), "the writers run");
        encoded(1, None);
        readers.into_iter().for_each(|r| {
            r.join().unwrap();
        });
        // Frame 2 has the same passes: a cut after the dispatch and one after the fill; the last dispatch's page has none.
        let o2 = at_flush(&detail, 2, &g.map, 0, false, &|| {}).expect("overlay");
        assert_eq!(o2.frame(), 2);
        let pg = |p: u64| {
            let h = g.mem.host_base() + p * PAGE;
            Pages::global().page_range(h, 1).unwrap()
        };
        let plan2 = early_cuts(&frame, &plan, &detail, 2, &g.map, &[]);
        assert_eq!(plan2.readers, 3);
        assert_eq!(plan2.blocked, 1, "the last pass writes page y");
        assert_eq!(plan2.cuts.len(), 2);
        assert_eq!((plan2.cuts[0].after_pass, plan2.cuts[0].pages.clone()), (0, vec![pg(page_a)]));
        assert_eq!((plan2.cuts[1].after_pass, plan2.cuts[1].pages.clone()), (1, vec![pg(page_z)]));
        // The same frame with a pool of raw stores over page a: no cut serves it.
        let learned = [(VA + page_a * PAGE, VA + (page_a + 1) * PAGE)];
        let plan3 = early_cuts(&frame, &plan, &detail, 2, &g.map, &learned);
        assert_eq!(plan3.cuts.len(), 1);
        assert_eq!(plan3.cuts[0].after_pass, 1);
        // The pages of a texture that is copied back are left out.
        let mut cuts = plan2.cuts.clone();
        early_exclude(&mut cuts, &[(VA + page_z * PAGE, VA + (page_z + 1) * PAGE)], &g.map);
        assert_eq!(cuts.len(), 1);
        assert_eq!(cuts[0].after_pass, 0);
        encoded(2, None);
        FORCE.with(|f| f.set(None));
    }

    const WRITE_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
kernel void nxkit_test_write(device uint *p [[buffer(0)]]) { p[0] = 0x600df00du; }
kernel void nxkit_test_spin(device uint *p [[buffer(0)]], device const uint *n [[buffer(1)]], uint t [[thread_position_in_grid]]) {
    uint x = t + 1u;
    for (uint i = 0u; i < n[0]; i++) { x = x * 1664525u + 1013904223u; }
    p[t] = x;
}
"#;

    /// One compute pass of `cb` that runs `kernel` over one thread with the buffers `bufs` (index, GPU address).
    fn kernel_pass(
        pipelines: &Pipelines,
        table: &ArgumentTable,
        cb: &mut CommandBuffer,
        kernel: &str,
        bufs: &[(u32, u64)],
    ) {
        let pipeline = pipelines.host_compute("nxkit-private-test", WRITE_MSL, kernel).unwrap();
        let mut cp = cb.begin_compute_pass(table).unwrap();
        cp.encode(&ComputeCommand::SetPipeline(pipeline)).unwrap();
        for &(index, gpu_addr) in bufs {
            cp.encode(&ComputeCommand::SetBuffer { index, gpu_addr }).unwrap();
        }
        cp.encode(&ComputeCommand::DispatchThreads {
            grid: [1, 1, 1],
            threads: [1, 1, 1],
        })
        .unwrap();
        cp.end();
    }

    #[test]
    fn a_reader_of_a_page_written_by_pass_0_goes_on_at_its_signal_while_a_long_pass_1_runs_and_before_the_rest_is_encoded() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        let Ok(pipelines) = Pipelines::new(&g.ctx, &nxkit_metal::ArchiveConfig::default()) else {
            return;
        };
        let (page_a, page_y) = (90u64, 91u64);
        let (frame, plan, detail) = three_passes(VA + page_a * PAGE, VA + 95 * PAGE, VA + page_y * PAGE);
        let _ = Pages::global().readers(0);
        let table = ArgumentTable::new(&g.ctx, 31, 0).unwrap();
        let mut ring = nxkit_metal::BatchRing::new(&g.ctx, 3).unwrap();
        // Calibration of the long pass: one thread spins for about `n` rounds.
        let spin = |ring: &mut nxkit_metal::BatchRing, n: u32| -> std::time::Duration {
            let mut batch = ring.begin(1).unwrap();
            let scratch = batch.scratch();
            let out = scratch.push(&[0u8; 64]).unwrap();
            let count = scratch.push(&n.to_le_bytes()).unwrap();
            kernel_pass(&pipelines, &table, &mut batch.buffers()[0], "nxkit_test_spin", &[(0, out), (1, count)]);
            g.ctx.commit_residency();
            let t0 = Instant::now();
            batch.submit().unwrap().wait().unwrap();
            t0.elapsed()
        };
        let _ = spin(&mut ring, 1000);
        let probe = spin(&mut ring, 20_000_000);
        let rounds = (20_000_000f64 * 0.5 / probe.as_secs_f64().max(0.001)).clamp(1e6, 4e9) as u32;
        // Frame 1 teaches the planner: threads wait for the writers of both pages.
        let _o = at_flush(&detail, 1, &g.map, 0, false, &|| {}).expect("overlay");
        let readers: Vec<_> = [page_a, page_y].iter().map(|&p| read_on_thread(&g, p)).collect();
        std::thread::sleep(std::time::Duration::from_millis(40));
        encoded(1, None);
        readers.into_iter().for_each(|r| {
            r.join().unwrap();
        });
        // Frame 2: flush, plan, then encode in the order of `encode_frame_inner`.
        let o2 = at_flush(&detail, 2, &g.map, 0, false, &|| {}).expect("overlay");
        let pools = pools_of(&g, o2.clone());
        let loc = pools.resolve(VA + page_a * PAGE, 64).unwrap();
        let mut ep = early_cuts(&frame, &plan, &detail, 2, &g.map, &[]);
        assert_eq!(ep.cuts.len(), 1, "a cut after pass 0: {:?}", ep.cuts);
        assert_eq!(ep.cuts[0].after_pass, 0);
        // Pass 0 is in the command buffer 1 (the command buffer 0 is the first one), the other passes in the command buffer 2.
        let reader = read_on_thread(&g, page_a);
        let other = read_on_thread(&g, page_y);
        let s0 = Pages::global().stats();
        let done_at = std::sync::Arc::new(Mutex::new(None::<Instant>));
        let mut batch = ring.begin(3).unwrap();
        assert!(batch.on_prefix_complete(2, early_release(2, ep.cuts.remove(0).pages)));
        let ahead = batch.commit_ahead().expect("a prefix");
        assert_eq!(ahead.count(), 2);
        let scratch = batch.scratch();
        let out = scratch.push(&[0u8; 64]).unwrap();
        let count = scratch.push(&rounds.to_le_bytes()).unwrap();
        {
            let bufs = batch.buffers();
            ahead.finish(0, &mut bufs[0]);
            // Pass 0 writes the page in the mirror.
            kernel_pass(&pipelines, &table, &mut bufs[1], "nxkit_test_write", &[(0, loc.real)]);
            assert!(ahead.finish(1, &mut bufs[1]), "the second command buffer completes the group");
        }
        // The group is on the GPU. The rest of the frame is not encoded yet: the reader must not wait for that.
        let t_group = Instant::now();
        let first_byte = reader.join().unwrap();
        let waited = t_group.elapsed();
        assert_eq!(first_byte, 0x0d, "the bytes of pass 0 (low byte of 0x600df00d)");
        assert!(waited < std::time::Duration::from_millis(1500), "the reader went on before the rest was encoded: {waited:?}");
        assert!(!other.is_finished(), "the page of the later pass still waits for the frame");
        // Now the long pass is encoded and the rest is committed.
        {
            let bufs = batch.buffers();
            kernel_pass(&pipelines, &table, &mut bufs[2], "nxkit_test_spin", &[(0, out), (1, count)]);
        }
        g.ctx.commit_residency();
        let submitted = batch.submit().unwrap();
        let t_done = done_at.clone();
        submitted.on_complete(move || *locked(&t_done) = Some(Instant::now()));
        encoded(2, Some(&submitted));
        let t_reader_done = t_group + waited;
        assert!(!other.is_finished() || locked(&done_at).is_some());
        let stats = submitted.wait().unwrap();
        let _ = other.join().unwrap();
        let finished = locked(&done_at).expect("the batch completed");
        assert!(
            t_reader_done + std::time::Duration::from_millis(100) < finished,
            "the reader was released at least 100 ms before the batch completed (long pass {:?})",
            std::time::Duration::from_nanos(stats.gpu_ns)
        );
        eprintln!(
            "pass signal test: reader released {waited:?} after the group was committed, {:?} before the batch completed (long pass {:?})",
            finished.duration_since(t_reader_done),
            std::time::Duration::from_nanos(stats.gpu_ns)
        );
        let s1 = Pages::global().stats();
        assert_eq!(s1.wait_ends[0] - s0.wait_ends[0], 1, "one wait ended at the signal of a pass");
        assert!(s1.early_pages > s0.early_pages);
        FORCE.with(|f| f.set(None));
    }

    #[test]
    fn a_batch_with_a_prefix_is_committed_in_two_groups_and_the_prefix_reports_before_the_rest_is_done() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        let Ok(pipelines) = Pipelines::new(&g.ctx, &nxkit_metal::ArchiveConfig::default()) else {
            return;
        };
        let table = ArgumentTable::new(&g.ctx, 31, 0).unwrap();
        let mut ring = nxkit_metal::BatchRing::new(&g.ctx, 3).unwrap();
        for with_prefix in [false, true] {
            let mut batch = ring.begin(3).unwrap();
            let scratch = batch.scratch();
            let out = scratch.push(&[0u8; 64]).unwrap();
            let count = scratch.push(&20_000_000u32.to_le_bytes()).unwrap();
            let at = std::sync::Arc::new(Mutex::new(None::<Instant>));
            if with_prefix {
                let a = at.clone();
                assert!(batch.on_prefix_complete(2, move || *locked(&a) = Some(Instant::now())));
                assert!(!batch.on_prefix_complete(3, || {}), "the whole batch is no prefix");
                assert!(!batch.on_prefix_complete(0, || {}));
            }
            {
                let bufs = batch.buffers();
                kernel_pass(&pipelines, &table, &mut bufs[0], "nxkit_test_write", &[(0, out)]);
                kernel_pass(&pipelines, &table, &mut bufs[1], "nxkit_test_write", &[(0, out)]);
                kernel_pass(&pipelines, &table, &mut bufs[2], "nxkit_test_spin", &[(0, out), (1, count)]);
            }
            g.ctx.commit_residency();
            let submitted = batch.submit().unwrap();
            let end = std::sync::Arc::new(Mutex::new(None::<Instant>));
            let e = end.clone();
            submitted.on_complete(move || *locked(&e) = Some(Instant::now()));
            let stats = submitted.wait().unwrap();
            assert!(stats.gpu_ns > 0);
            let end = locked(&end).expect("the batch completed");
            if with_prefix {
                let prefix = locked(&at).expect("the prefix reported");
                assert!(prefix <= end, "the prefix reports before the batch completes");
            } else {
                assert!(locked(&at).is_none());
            }
        }
        FORCE.with(|f| f.set(None));
    }
}
