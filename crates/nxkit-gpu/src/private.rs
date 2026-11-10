//! Private GPU memory, step 1: tracking (`NXKIT_PRIVATE=track`, see `docs/nxkit-gpu.md`).
//!
//! The question of this step is what a private copy of guest memory for the GPU would cost, before any copy exists. At
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
//! nothing is redirected. Without the variable nothing here runs.
//!
//! The counters print every [`REPORT_EVERY`] frames (`NXKIT_DIAG=private`, also with `NXKIT_PRIVATE=track`).

use std::hash::BuildHasher;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use nxkit_metal::{AddressMap, FaultKind, FaultOrigin, PageStats, Pages, SubmittedBatch};

use crate::detail::{CopyKind, FrameDetail, Stage};
use crate::draws::FastMap;
use crate::programs::{BindKind, Programs};

/// Frames between two reports.
pub(crate) const REPORT_EVERY: u64 = 300;
/// The largest single range that counts (a binding that is larger is a placeholder for "the whole buffer").
const RANGE_MAX_BYTES: u64 = 256 << 20;
/// The most bytes of member pages that one flush may add to the pages it protects against all access.
const FRAME_MAX_BYTES: u64 = 1 << 30;
/// The part of a buffer of unknown size that a copy touches.
const UNKNOWN_BYTES: u64 = 1 << 20;
/// Program counters listed for each kind.
const TOP: usize = 10;

/// Whether `NXKIT_PRIVATE=track` is set. Read once. Other values warn once and leave the tracking off: the other modes
/// of the variable are not built.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| match std::env::var("NXKIT_PRIVATE") {
        Ok(v) if v == "track" => true,
        Ok(v) if !v.is_empty() && v != "0" => {
            eprintln!("nxkit-gpu: NXKIT_PRIVATE={v} is not a mode that exists yet, only `track` does; it stays off");
            false
        }
        _ => false,
    })
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
    base: PageStats,
}

struct Private {
    rw: Mutex<Rw>,
    /// The pools shaders wrote through raw pointers, as the encoder last saw them.
    learned: Mutex<Vec<(u64, u64)>>,
    window: Mutex<Window>,
    frames: std::sync::atomic::AtomicU64,
    peak_members: std::sync::atomic::AtomicU64,
}

fn state() -> &'static Private {
    static S: OnceLock<Private> = OnceLock::new();
    S.get_or_init(|| Private {
        rw: Mutex::new(Rw::default()),
        learned: Mutex::new(Vec::new()),
        window: Mutex::new(Window::default()),
        frames: std::sync::atomic::AtomicU64::new(0),
        peak_members: std::sync::atomic::AtomicU64::new(0),
    })
}

/// Names the guest memory to the page tracker, when the tracking is on. Called once, when the backend is made.
pub(crate) fn attach(host_base: u64, len: u64) {
    if !on() {
        if crate::diag("private") {
            eprintln!("DIAG private: NXKIT_PRIVATE=track is not set, nothing is tracked");
        }
        return;
    }
    Pages::global().attach(host_base, len);
    eprintln!(
        "nxkit-gpu: NXKIT_PRIVATE=track, tracking {} MiB of guest memory in pages of {} KiB",
        len >> 20,
        Pages::global().page_size() >> 10
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

/// The flush of a frame, on the CPU thread: protects the member pages and the pages the frame may write, and counts. The
/// frame is complete when [`encoded`] reports its batch done.
pub(crate) fn at_flush(detail: &FrameDetail, fence: u64, map: &AddressMap) {
    if !on() || fence == 0 {
        return;
    }
    let st = state();
    let t_plan = Instant::now();
    let learned = locked(&st.learned).clone();
    let (ranges, clipped) = {
        let rw = locked(&st.rw);
        let mut gpu = written_ranges(detail, &learned, &rw);
        drop(rw);
        gpu.sort_unstable();
        gpu.dedup();
        // To host addresses, with the limits of one range and of the frame.
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
}

/// The encoder submitted the frame of `fence`: its pages are released when the batch is complete on the GPU. A frame
/// without a batch is complete at once.
pub(crate) fn encoded(fence: u64, batch: Option<&SubmittedBatch>) {
    if !on() {
        return;
    }
    match batch {
        Some(b) => b.on_complete(move || Pages::global().complete(fence)),
        None => Pages::global().complete(fence),
    }
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
         read in flight {:.1} ({}), other {:.1}; in the handler {:.3} ms per frame",
        d(FaultKind::CleanWrite) as f64 / n,
        by_origin(FaultKind::CleanWrite),
        d(FaultKind::WriteInFlight) as f64 / n,
        by_origin(FaultKind::WriteInFlight),
        d(FaultKind::ReadInFlight) as f64 / n,
        by_origin(FaultKind::ReadInFlight),
        d(FaultKind::Other) as f64 / n,
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
        ..Window::default()
    };
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
