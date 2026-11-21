//! Copies from the mirror into guest memory, and the handler's path for a page of a frame in flight.

use super::*;

/// Copies the stale blocks of the mirror page `pg` into guest memory: the blocks that a frame may have written. The
/// other blocks of the guest page hold the CPU's bytes (the handler stores to them without waiting) and stay, and so do
/// the blocks that the CPU wrote while a frame wrote them (results one frame late). The guest page must be writable.
/// `frame` is the frame whose bytes these are.
fn copy_from_mirror(pg: usize, mirror: u64, frame: u32) {
    copy_stale(pg, mirror, SHARED.base.load(Ordering::Acquire), true, frame);
}

/// [`copy_from_mirror`] into the page at `dst_base + pg * page` (the guest range itself, or its alias), counted as a copy
/// of the handler when `count` says so.
pub(super) fn copy_stale(pg: usize, mirror: u64, dst_base: u64, count: bool, frame: u32) {
    let (stale, keep) = info(pg).map_or(([u64::MAX; 4], [0; 4]), |i| (i.stale(), i.keep(frame)));
    copy_mask(
        pg,
        mirror,
        dst_base,
        &std::array::from_fn(|i| stale[i] & !keep[i]),
    );
    let kept: u32 = stale
        .iter()
        .zip(&keep)
        .map(|(s, k)| (s & k).count_ones())
        .sum();
    if kept > 0 {
        SHARED
            .late_kept
            .fetch_add(u64::from(kept), Ordering::Relaxed);
    }
    if count {
        SHARED.synced.fetch_add(1, Ordering::Relaxed);
    }
}

/// Copies the blocks of `mask` of the mirror page `pg` into the page at `dst_base + pg * page`.
fn copy_mask(pg: usize, mirror: u64, dst_base: u64, mask: &Mask) {
    let off = pg as u64 * page();
    copy_blocks(mirror + off, dst_base + off, mask);
}

/// True when block `b` is set in `m`.
pub(super) fn has(m: &Mask, b: usize) -> bool {
    m[b / 64] >> (b % 64) & 1 != 0
}

/// Copies the blocks of `mask` of the page at `src` into the page at `dst` (both host addresses of a whole page).
pub(super) fn copy_blocks(src: u64, dst: u64, mask: &Mask) {
    let p = page();
    let blocks = (p / BLOCK) as usize;
    let mut b = 0;
    while b < blocks {
        if mask[b / 64] >> (b % 64) & 1 == 0 {
            b += 1;
            continue;
        }
        let first = b;
        while b < blocks && mask[b / 64] >> (b % 64) & 1 != 0 {
            b += 1;
        }
        let off = first as u64 * BLOCK;
        // SAFETY: the callers pass whole pages: a guest page inside the range that `attach` named, made writable (the
        // page itself, or its alias, which is always writable), and the page of the mirror (readable while the page is
        // a member) or of a copy the GPU made of it. The two are different mappings.
        unsafe {
            let n = (b - first) * BLOCK as usize;
            std::ptr::copy_nonoverlapping((src + off) as *const u8, (dst + off) as *mut u8, n);
        }
    }
}

/// Serves a fault on the tracked page `pg` when there is a mirror. A page tagged with a frame holds stale bytes: the
/// thread waits for the frame to complete (an internal thread does not wait), copies the page from the mirror and opens
/// it. Atomics, `mprotect`, `memcpy` and the wake word; no lock, no allocation.
pub(super) fn serve_mirrored(
    pg: usize,
    w: &AtomicU32,
    write: bool,
    internal: bool,
    mirror: u64,
    at: u64,
) -> FaultKind {
    let p = pg as u32;
    let mut kind: Option<FaultKind> = None;
    let mut gave_up = false;
    loop {
        let cur = w.load(Ordering::Acquire);
        let state = cur & STATE_MASK;
        let frame = cur >> FRAME_SHIFT;
        match state {
            CLEAN => {
                if write {
                    if w.compare_exchange(cur, DIRTY, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                    {
                        continue;
                    }
                    log_dirty(pg);
                    protect_pages(p, p + 1, PROT_RW, false);
                    return kind.unwrap_or(FaultKind::CleanWrite);
                }
                protect_pages(p, p + 1, PROT_READ, false);
                return kind.unwrap_or(FaultKind::Other);
            }
            GPU | OPEN => {
                let flying = page_flying(pg, frame) && !gave_up;
                if kind.is_none() && flying && !internal {
                    learn_reader(pg, frame);
                    let n = SHARED.flight_n.fetch_add(1, Ordering::AcqRel) as usize;
                    if n < FLIGHT_CAP {
                        let off =
                            at.wrapping_sub(SHARED.base.load(Ordering::Relaxed)) & ((1 << 35) - 2);
                        let v = u64::from(frame) << 36 | u64::from(write) << 35 | (off + 1);
                        SHARED.flight_log[n].store(v, Ordering::Release);
                    }
                }
                let k = *kind.get_or_insert(if flying {
                    if write {
                        FaultKind::WriteInFlight
                    } else {
                        FaultKind::ReadInFlight
                    }
                } else {
                    FaultKind::Sync
                });
                if state == OPEN && !write {
                    // Open for reading already: a race with the thread that opened it.
                    protect_pages(p, p + 1, PROT_READ, false);
                    return k;
                }
                if flying && !internal {
                    gave_up = wait_complete(pg, frame, write);
                    continue;
                }
                // The claim: one thread copies, the others sleep on the wake word and run their access again.
                if w.compare_exchange(
                    cur,
                    pack(SYNC, u64::from(frame)),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
                {
                    continue;
                }
                let fresh = cur & FRESH != 0;
                protect_pages(p, p + 1, PROT_RW, false);
                if !(flying && fresh) {
                    if flying || !info(pg).is_some_and(|i| late_current(i, frame)) {
                        copy_from_mirror(pg, mirror, frame);
                    }
                    if !flying
                        && !internal
                        && let Some(i) = info(pg)
                    {
                        // A game thread waited for this copy: the completion of the next frame that writes the page copies
                        // it on its own (step 5).
                        i.eager
                            .store(SHARED.epoch.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
                    }
                    if !flying && let Some(i) = info(pg) {
                        // Guest memory is current here again. (A page read in flight stays stale: the frame may
                        // still write it.)
                        for a in &i.stale {
                            a.store(0, Ordering::Release);
                        }
                        // Guest memory holds the frame's bytes: the next frame's readers may go on without a wait.
                        i.snap.store(frame.max(1), Ordering::Release);
                    }
                }
                if flying {
                    SHARED.internal_opens.fetch_add(1, Ordering::Relaxed);
                } else if !internal && in_flight(frame) {
                    // The frame runs on, but the last pass that writes this page is done (step 6).
                    SHARED.early_syncs.fetch_add(1, Ordering::Relaxed);
                }
                if write {
                    w.store(DIRTY, Ordering::Release);
                    log_dirty(pg);
                } else if flying {
                    let open = pack(OPEN, u64::from(frame)) | (cur & FRESH);
                    w.store(open, Ordering::Release);
                    protect_pages(p, p + 1, PROT_READ, false);
                } else {
                    w.store(CLEAN, Ordering::Release);
                    protect_pages(p, p + 1, PROT_READ, false);
                }
                wake_all();
                return k;
            }
            SYNC => {
                // Another thread copies the page. Sleep on the wake word and run the access again.
                sleep_on_wake(SHARED.wake.load(Ordering::Acquire));
                return kind.unwrap_or(FaultKind::Other);
            }
            _ => {
                // Dirty (a race with a flush), or free (a race with a release): open it.
                protect_pages(p, p + 1, PROT_RW, false);
                return kind.unwrap_or(FaultKind::Other);
            }
        }
    }
}
