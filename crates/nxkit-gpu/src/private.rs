//! Private GPU memory: step 1, tracking (`NXKIT_PRIVATE=track`), and step 2, the mirror (`NXKIT_PRIVATE=1`; see
//! `docs/nxkit-gpu.md`).
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
//! The fence logic is the same as without it: the overlap and the renamer are off. Without the variable nothing here runs.
//!
//! The counters print every [`REPORT_EVERY`] frames (`NXKIT_DIAG=private`, also with the variable set).

use std::hash::BuildHasher;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use nxkit_metal::{
    AddressMap, ArgumentTable, CommandBuffer, ComputeCommand, Context, DataBuffer, Dependency, FaultKind,
    FaultOrigin, GuestMemory, Mirror, PageStats, Pages, Scope, Scratch, Stager, Stages, SubmittedBatch,
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
    /// `1`: step 2, the mirror.
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
    /// The partial resolves (`overlay::partial`) at the start of the window.
    partial_base: u64,
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
    })
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
    let mut out: Vec<(u64, u64)> = Vec::new();
    // Draws after draws bind the same buffers: a direct-mapped table of the bindings seen lately drops the repeats.
    let mut seen = vec![(0u64, 0u32, 0u32, 0u64); 4096];
    let mut add = |s: &crate::detail::UboDetail, prog: Option<u64>| {
        if s.address == 0 || s.size == 0 {
            return;
        }
        let slot_stage = s.slot | (s.stage as u32) << 24;
        if !rw.written(prog, slot_stage) {
            return;
        }
        let p = prog.unwrap_or(0);
        let at = ((s.address >> 4) ^ (s.address >> 16) ^ p ^ u64::from(slot_stage)) as usize & 4095;
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
    for d in &detail.draws {
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
    for d in &detail.dispatches {
        let h = hash_str(&d.program);
        for s in &d.sbufs {
            if s.stage == Stage::Compute {
                add(s, Some(h));
            } else {
                add(s, None);
            }
        }
    }
    for c in &detail.copies {
        let (a, len) = match c.kind {
            CopyKind::BufferFill | CopyKind::BufferToBuffer => (c.dst_addr, c.bytes),
            CopyKind::TextureToBuffer => (c.buffer, 0),
            _ => (0, 0),
        };
        if a != 0 {
            out.push((a, if len != 0 { len } else { UNKNOWN_BYTES }));
        }
    }
    out.extend(learned.iter().map(|&(lo, hi)| (lo, hi.saturating_sub(lo))));
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
    drain: &dyn Fn(),
) -> Option<Arc<GuestOverlay>> {
    if !on() || fence == 0 {
        return None;
    }
    if mirrored() {
        return flush_mirror(detail, fence, map, drain);
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
fn read_ranges(detail: &FrameDetail) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut seen = vec![(0u64, 0u64); 4096];
    let mut add = |a: u64, len: u64| {
        if a == 0 || len == 0 {
            return;
        }
        let at = ((a >> 4) ^ (a >> 17) ^ len) as usize & 4095;
        if seen[at] == (a, len) {
            return;
        }
        seen[at] = (a, len);
        out.push((a, len));
    };
    let mut prev = 0usize;
    for d in &detail.draws {
        let ptr = Arc::as_ptr(d) as usize;
        if ptr == prev {
            continue;
        }
        prev = ptr;
        for u in d.ubos.iter().chain(&d.sbufs) {
            add(u.address, u64::from(u.size));
        }
        for s in &d.streams {
            add(s.address, stream_extent(s, &d.args));
        }
        match d.args {
            DrawArgs::Indexed {
                index_type,
                address,
                count,
                ..
            } => add(address, u64::from(count) << index_type.min(2)),
            DrawArgs::ArraysIndirect { indirect } => add(indirect, 16),
            DrawArgs::IndexedIndirect { address, indirect, .. } => {
                add(address, UNKNOWN_BYTES);
                add(indirect, 20);
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
                add(indirect, (u64::from(max) * stride.max(20)).min(RANGE_MAX_BYTES));
                add(count, 4);
            }
            DrawArgs::MeshTasksIndirect { indirect } => add(indirect, 8),
            _ => {}
        }
    }
    for d in &detail.dispatches {
        for u in d.ubos.iter().chain(&d.sbufs) {
            add(u.address, u64::from(u.size));
        }
        if let Some(a) = d.indirect {
            add(a, 12);
        }
    }
    for c in &detail.copies {
        match c.kind {
            CopyKind::BufferToBuffer => add(c.src_addr, c.bytes),
            CopyKind::BufferToTexture => add(c.buffer, UNKNOWN_BYTES),
            _ => {}
        }
    }
    out
}

/// The pages that become members because a binding of the frame touches a member (the written ranges included): up to two
/// rounds, since a range that is made whole can touch another binding.
fn closure(
    pages: &Pages,
    reads: &[(u64, u64)],
    map: &AddressMap,
    members: &mut Members<'_>,
) -> u64 {
    let mut added_pages = 0u64;
    for _ in 0..2 {
        let mut new: Vec<(u32, u32)> = Vec::new();
        for &(a, len) in reads {
            let len = len.min(RANGE_MAX_BYTES);
            let Ok(h) = map.host_address(a, len) else {
                continue;
            };
            let Some((first, end)) = pages.page_range(h, len) else {
                continue;
            };
            if members.any(first, end) && !members.all(first, end) {
                new.push((first, end));
            }
        }
        if new.is_empty() {
            break;
        }
        let page = pages.page_size();
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
            failed: false,
        }
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
        if self.failed {
            return;
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
    drain: &dyn Fn(),
) -> Option<Arc<GuestOverlay>> {
    let st = state();
    let pages = Pages::global();
    let t_plan = Instant::now();
    let learned = locked(&st.learned).clone();
    let (written, clipped) = {
        let rw = locked(&st.rw);
        plan_written(detail, &learned, &rw, map)
    };
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
    let reads = read_ranges(detail);
    let closure_pages = closure(pages, &reads, map, &mut members);
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
    let (buf, slots, failed) = (sink.buf.take(), std::mem::take(&mut sink.slots), sink.failed);
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
            Some(Staged { buf: b, slots })
        }
        Some(b) => {
            ms.pool.free.push(b);
            None
        }
        None => None,
    };
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
    let overlay = Overlay::new(ms.va_base, bits, runs, Some(ms.mirror.view()), staged, generation).with_off(off);
    drop(guard);
    pages.set_phase(1);
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
        n.is_multiple_of(REPORT_EVERY)
    };
    if report {
        print_report(n);
    }
    Some(Arc::new(overlay))
}

/// The kernel that copies staged pages into the mirror: one page per 256 threads, 16 bytes each, four times.
const COPY_IN_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct page_job {
    device uint4 *dst;
    device const uint4 *src;
};
kernel void nxkit_private_in(device const page_job *jobs [[buffer(0)]], uint t [[thread_position_in_grid]]) {
    page_job j = jobs[t >> 8];
    uint lane = t & 255u;
    for (uint i = lane; i < 1024u; i += 256u) j.dst[i] = j.src[i];
}
"#;

/// The `(mirror address, staging address)` of every page that the frame's first command buffer copies, and the number of
/// staged pages whose page is no longer a member of the view (they are dropped).
pub(crate) fn copy_in_jobs(o: &GuestOverlay) -> (Vec<(u64, u64)>, u32) {
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
                Some((_, real, _)) => Some((real, base + u64::from(slot) * PAGE)),
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
    jobs: &[(u64, u64)],
) -> Result<u32, Skip> {
    if jobs.is_empty() {
        return Ok(0);
    }
    let bytes: Vec<u8> = jobs
        .iter()
        .flat_map(|&(dst, src)| dst.to_le_bytes().into_iter().chain(src.to_le_bytes()))
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

/// The encoder submitted the frame of `fence`: its pages are released when the batch is complete on the GPU. A frame
/// without a batch is complete at once.
pub(crate) fn encoded(fence: u64, batch: Option<&SubmittedBatch>) {
    if !on() {
        return;
    }
    // The frame is committed and runs; the completion says it is done.
    Pages::global().set_phase(2);
    let done = move || {
        Pages::global().complete(fence);
        Pages::global().set_phase(0);
    };
    match batch {
        Some(b) => b.on_complete(done),
        None => done(),
    }
}

/// The knob `privlatestage`: the pages that the CPU wrote since the flush are staged a second time, right before the frame's
/// first command buffer is committed. Returns the `(mirror address, staging address)` of each page for a second copy-in pass.
pub(crate) fn late_stage(o: &GuestOverlay) -> Vec<(u64, u64)> {
    if !mirrored() || !crate::knob("privlatestage") {
        return Vec::new();
    }
    let mut guard = locked(&state().mirror);
    let Some(ms) = guard.as_mut() else {
        return Vec::new();
    };
    let fence = ms.last_fence;
    let mut sink = Sink::new(&ms.ctx, &mut ms.pool);
    Pages::global().flush_late(&mut sink);
    let (buf, slots, failed) = (sink.buf.take(), std::mem::take(&mut sink.slots), sink.failed);
    drop(sink);
    let Some(b) = buf else {
        return Vec::new();
    };
    if slots.is_empty() || failed {
        ms.pool.free.push(b);
        return Vec::new();
    }
    ms.pool.busy.push((fence, b.clone()));
    drop(guard);
    locked(&state().window).late_staged += slots.len() as u64;
    let base = b.gpu_address();
    slots
        .iter()
        .filter_map(|&(page, slot)| {
            o.locate(o.va_base() + u64::from(page) * PAGE)
                .map(|(_, real, _)| (real, base + u64::from(slot) * PAGE))
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
        let partial = crate::overlay::partial();
        let sync = |o: FaultOrigin| now.faults[FaultKind::Sync as usize][o as usize]
            - base.faults[FaultKind::Sync as usize][o as usize];
        eprintln!(
            "DIAG private mirror frame {frame}: mirror {:.1} MiB usable in {runs} runs ({retired} buffers retired), \
             members {} pages; per frame: staged {:.1} pages ({:.2} MiB, {:.2} written again), copy-in {:.1} pages \
             (lost {}), pages copied mirror to guest {:.1} (by a fault of guest code {:.1}), closure added {:.1} pages, \
             waits for a writer {:.2} ({:.3} ms; {} timeouts), internal opens {:.2}, enter waits {}, \
             partial resolves {}, poison {}",
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
            partial - w.partial_base,
            if poison_on() { "on" } else { "off" },
        );
    }
    if mirrored() {
        let ph = |i: usize| (now.phase_writes[i] - base.phase_writes[i]) as f64 / n;
        eprintln!(
            "DIAG private writes to member pages per frame (first write of a page): after the GPU was done or before the \
             next flush {:.1}, between the flush and the commit {:.1}, after the commit before the GPU was done {:.1}; \
             staged a second time (privlatestage) {:.1}",
            ph(0),
            ph(1),
            ph(2),
            w.late_staged as f64 / n,
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
        partial_base: crate::overlay::partial(),
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

    static TURN: Mutex<()> = Mutex::new(());
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
        let o = at_flush(&detail, 1, &g.map, &|| {}).expect("overlay");
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
        let o = at_flush(&detail, 1, &g.map, &|| {}).expect("overlay");
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
        let o1 = at_flush(&detail, 1, &g.map, &|| {}).expect("overlay");
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
        let o2 = at_flush(&detail, 2, &g.map, &|| {}).expect("overlay");
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
    fn the_poison_knob_fills_the_stale_guest_bytes_of_a_page_a_host_thread_reads_in_flight() {
        let _t = locked(&TURN);
        let Some(g) = guest(64 << 20) else { return };
        FORCE_POISON.with(|f| f.set(true));
        let page = PAGE;
        let addr = VA + 40 * page;
        // The page is a member; the CPU writes it, and the frame that writes it starts: it is fresh, guest memory holds
        // its bytes, and the poison covers them.
        let o = at_flush(&dispatch(addr, 0x100), 1, &g.map, &|| {}).expect("overlay");
        encoded(1, None);
        g.mem.write(40 * page + 4, &[3; 4]).unwrap();
        let o2 = at_flush(&dispatch(addr, 0x100), 2, &g.map, &|| {}).expect("overlay");
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
        let o = at_flush(&dispatch(addr, 0x100), 1, &g.map, &|| {}).expect("overlay");
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
        let _o = at_flush(&detail, 1, &g.map, &|| {}).expect("overlay");
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
}
