//! `NXKIT_DIAG=overlap`: what the CPU rewrites of the buffers that a frame reads (see `docs/plans/fence-overlap.md`).
//!
//! A game that waits for its newest fence could run a frame ahead if the GPU read its own copy of the memory that the
//! CPU rewrites. This counts, without changing anything:
//!
//! - the `FlushMappedRange` calls of the CPU and their bytes, and the 16 KiB pages that they named in the last
//!   [`DYNAMIC_FRAMES`] frames (dynamic pages);
//! - the bytes of the buffer bindings of each frame (uniform, storage, vertex, index) that touch a dynamic page: what
//!   shadow copies would copy;
//! - the flushes that touch a page that the frame still on the GPU reads, by kind of binding: the writes that race with
//!   the GPU when `nvnSyncWait` returns early (the `nowait` knob).

use std::sync::{Mutex, OnceLock};

/// Whether the CPU may run one frame ahead of the GPU (`NXKIT_OVERLAP=1`): `nvnSyncWait` for the newest frame waits for
/// the frame before it, and every bound uniform buffer reads a copy that the walk takes, so that the CPU's writes for
/// the next frame do not reach the frame on the GPU. See `docs/plans/fence-overlap.md`.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NXKIT_OVERLAP").is_ok_and(|v| v == "1")) || crate::knob("overlap")
}

use crate::detail::{DrawArgs, FrameDetail, UboDetail};
use crate::draws::FastMap;

const PAGE: u64 = 16 * 1024;
/// A page stays dynamic for this many frames after a flush named it.
const DYNAMIC_FRAMES: u64 = 8;
/// The part of a vertex stream that counts, from its address: the stream size is the rest of the buffer.
const STREAM_BYTES: u64 = 1 << 20;
/// Frames between two lines of the report.
const REPORT_EVERY: u64 = 120;

const KINDS: [&str; 4] = ["uniform", "storage", "vertex", "index"];

#[derive(Default)]
struct State {
    frame: u64,
    /// The last frame in which a flush named the page.
    flushed: FastMap<u64, u64>,
    /// The fence of the frame on the GPU and the kinds of binding of each page that it reads.
    inflight: Option<(u64, FastMap<u64, u8>)>,
    calls: u64,
    bytes: u64,
    racing_calls: u64,
    racing: [u64; 4],
    copy: [u64; 4],
    read_pages: u64,
    frames: u64,
    /// The hash of the bytes of each binding in the last frame that had it.
    hashes: FastMap<(u64, u64, u8), u64>,
    /// Bytes of the distinct bindings, and of those whose bytes changed since the last frame that had them.
    bound: [u64; 4],
    changed: [u64; 4],
    changed_n: [u64; 4],
    bound_n: [u64; 4],
}

/// The counters of the diagnostic.
#[derive(Default)]
pub(crate) struct Overlap {
    state: Mutex<State>,
}

fn pages(addr: u64, len: u64) -> std::ops::Range<u64> {
    addr / PAGE..(addr + len.max(1)).div_ceil(PAGE)
}

impl Overlap {
    /// The CPU flushed `len` bytes at the GPU address `addr`. `done` is the newest fence that the GPU finished.
    pub(crate) fn flushed(&self, addr: u64, len: u64, done: u64) {
        let Ok(mut s) = self.state.lock() else { return };
        let frame = s.frame;
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if frame >= 2000 && SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 40 {
            eprintln!("DIAG overlap flush gpu {addr:#x} len {len}");
        }
        s.calls += 1;
        s.bytes += len;
        let mut kinds = 0u8;
        for p in pages(addr, len) {
            s.flushed.insert(p, frame);
            if let Some((fence, read)) = &s.inflight
                && *fence > done
            {
                kinds |= read.get(&p).copied().unwrap_or(0);
            }
        }
        if kinds != 0 {
            s.racing_calls += 1;
            for (k, n) in s.racing.iter_mut().enumerate() {
                if kinds & 1 << k != 0 {
                    *n += 1;
                }
            }
        }
    }

    /// The frame with fence `fence` goes to the GPU with the bindings of `detail`.
    pub(crate) fn frame(&self, detail: &FrameDetail, fence: u64, mem: &nxkit_metal::GuestMemory) {
        let Ok(mut s) = self.state.lock() else { return };
        let frame = s.frame;
        let mut read: FastMap<u64, u8> = FastMap::default();
        let mut seen: FastMap<(u64, u64, u8), ()> = FastMap::default();
        let base = mem.address_map().gpu_base();
        let mut buf: Vec<u8> = Vec::new();
        let mut add = |s: &mut State, kind: usize, addr: u64, len: u64| {
            if addr == 0 || len == 0 || seen.insert((addr, len, kind as u8), ()).is_some() {
                return;
            }
            buf.resize(len as usize, 0);
            if addr >= base && mem.read(addr - base, &mut buf).is_ok() {
                let h = buf.chunks(8).fold(0xcbf2_9ce4_8422_2325u64, |h, c| {
                    let mut w = [0u8; 8];
                    w[..c.len()].copy_from_slice(c);
                    (h ^ u64::from_le_bytes(w)).wrapping_mul(0x100_0000_01b3).rotate_left(23)
                });
                s.bound[kind] += len;
                s.bound_n[kind] += 1;
                if s.hashes.insert((addr, len, kind as u8), h).is_some_and(|old| old != h) {
                    s.changed[kind] += len;
                    s.changed_n[kind] += 1;
                }
            }
            let mut dynamic = false;
            for p in pages(addr, len) {
                *read.entry(p).or_default() |= 1 << kind;
                dynamic |= s.flushed.get(&p).is_some_and(|f| frame.saturating_sub(*f) < DYNAMIC_FRAMES);
            }
            if dynamic {
                s.copy[kind] += len;
            }
        };
        let ubos = |list: &Vec<UboDetail>| list.iter().filter(|u| u.snapshot.is_none()).map(|u| (u.address, u64::from(u.size))).collect::<Vec<_>>();
        for d in &detail.draws {
            for (a, l) in ubos(&d.ubos) {
                add(&mut s, 0, a, l);
            }
            for (a, l) in ubos(&d.sbufs) {
                add(&mut s, 1, a, l);
            }
            let verts = match d.args {
                DrawArgs::Arrays { first, count, .. } => Some(u64::from(first) + u64::from(count)),
                DrawArgs::Indexed { .. } => None,
            };
            for st in &d.streams {
                let used = verts.map_or(STREAM_BYTES, |v| v * u64::from(st.stride.max(1)));
                add(&mut s, 2, st.address, st.size.min(used).min(STREAM_BYTES));
            }
            if let DrawArgs::Indexed { index_type, address, count, .. } = d.args {
                add(&mut s, 3, address, u64::from(count) << index_type.min(2));
            }
        }
        for d in &detail.dispatches {
            for (a, l) in ubos(&d.ubos) {
                add(&mut s, 0, a, l);
            }
            for (a, l) in ubos(&d.sbufs) {
                add(&mut s, 1, a, l);
            }
        }
        if frame % REPORT_EVERY == 60 {
            let mut sample: Vec<u64> = read.keys().map(|p| p * PAGE).collect();
            sample.sort_unstable();
            let n = sample.len();
            let picks: Vec<String> = sample.iter().step_by((n / 12).max(1)).map(|a| format!("{a:#x}")).collect();
            eprintln!("DIAG overlap read pages {n}: {}", picks.join(" "));
        }
        if s.hashes.len() > 200_000 {
            s.hashes.clear();
        }
        s.read_pages += read.len() as u64;
        s.inflight = Some((fence, read));
        s.frames += 1;
        s.frame += 1;
        let keep = s.frame;
        s.flushed.retain(|_, f| keep.saturating_sub(*f) < DYNAMIC_FRAMES);
        if s.frames >= REPORT_EVERY {
            let n = s.frames as f64;
            let per = |v: u64| v as f64 / n;
            let kb = |v: u64| v as f64 / n / 1024.0;
            let copy: Vec<String> = KINDS.iter().zip(s.copy).map(|(k, v)| format!("{k} {:.0} KiB", kb(v))).collect();
            let race: Vec<String> = KINDS.iter().zip(s.racing).map(|(k, v)| format!("{k} {:.1}", per(v))).collect();
            let content: Vec<String> = (0..4)
                .map(|k| {
                    format!(
                        "{} {:.0} bindings {:.0} KiB, changed {:.0} bindings {:.0} KiB",
                        KINDS[k],
                        per(s.bound_n[k]),
                        kb(s.bound[k]),
                        per(s.changed_n[k]),
                        kb(s.changed[k])
                    )
                })
                .collect();
            eprintln!("DIAG overlap content per frame: {}", content.join(" | "));
            eprintln!(
                "DIAG overlap per frame: flushes {:.0} ({:.0} KiB), dynamic pages {}, pages read {:.0}, \
                 copy on dynamic pages [{}], flushes racing the GPU {:.1} [{}]",
                per(s.calls),
                kb(s.bytes),
                s.flushed.len(),
                per(s.read_pages),
                copy.join(", "),
                per(s.racing_calls),
                race.join(", "),
            );
            let f = s.frame;
            *s = State {
                frame: f,
                flushed: std::mem::take(&mut s.flushed),
                inflight: s.inflight.take(),
                hashes: std::mem::take(&mut s.hashes),
                ..State::default()
            };
        }
    }
}
