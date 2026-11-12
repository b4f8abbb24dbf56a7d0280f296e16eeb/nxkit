//! Step 1 and the CPU side: attaching guest memory, the bindings a frame may write and the learned pools.

use super::*;

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
                            if !std::env::var("NXKIT_PRIVATE_READOPEN")
                                .is_ok_and(|v| v.trim() == "0")
                            {
                                pages.set_read_open(true);
                                eprintln!(
                                    "nxkit-gpu: NXKIT_PRIVATE=1: a page whose guest bytes are a completed frame's stays readable while a \
                                     frame writes it, so a read takes no trap (NXKIT_PRIVATE_READOPEN=0 turns it off)"
                                );
                            }
                            eprintln!(
                                "nxkit-gpu: NXKIT_PRIVATE=1: the CPU sees the GPU's results one frame late: a game thread's access to a \
                                 page that a frame in flight writes is served from guest memory, which holds the last completed frame's \
                                 bytes, without a wait (NXKIT_PRIVATE_LATE=0 turns it off)"
                            );
                        }
                        eprintln!(
                            "nxkit-gpu: NXKIT_PRIVATE=1: accesses of the CPU to a page that a frame writes, outside the bytes it \
                             writes, are done by the fault handler through a second mapping of guest memory{}",
                            if off {
                                " (off: the knob privnoemu)"
                            } else {
                                ""
                            }
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
                eprintln!(
                    "nxkit-gpu: NXKIT_PRIVATE=1: no mirror ({e}); the run goes on without it"
                );
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
                learn(
                    rw,
                    h,
                    &d.program,
                    false,
                    s.slot,
                    s.slot | (s.stage as u32) << 24,
                );
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
pub(super) fn written_ranges(
    detail: &FrameDetail,
    learned: &[(u64, u64)],
    rw: &Rw,
) -> Vec<(u64, u64)> {
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
pub(super) fn written_in(
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
pub(super) fn plan_written(
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
