//! The handler does an access outside the stale blocks in place of the thread, through the alias.

use super::*;

/// Does the access that faulted on the page `pg` in place of the thread, when it lies outside the blocks that a frame
/// in flight may write: the access runs through the alias, the registers are filled or read, and the program counter
/// moves on. Returns false when the access must take the ordinary path (wait for the writer, copy, open). Atomics and
/// the access itself; no lock, no allocation, no system call.
pub(super) fn try_emulate(pg: usize, w: &AtomicU32, pc: u64, far: u64, uctx: *mut c_void) -> bool {
    let alias = SHARED.alias.load(Ordering::Acquire);
    if alias == 0 || pc == 0 || !SHARED.emulate.load(Ordering::Relaxed) {
        return false;
    }
    let cur = w.load(Ordering::Acquire);
    if !matches!(cur & STATE_MASK, GPU | OPEN) || !page_flying(pg, cur >> FRAME_SHIFT) {
        return false;
    }
    let Some(info) = info(pg) else { return false };
    let t0 = now_ns();
    // SAFETY: a fault at `pc` means the instruction there was fetched, so its word is readable.
    let insn = unsafe { (pc as *const u32).read() };
    let op = match a64::decode_op(insn) {
        Ok(op) => op,
        Err(why) => {
            count_fallback(why, pc);
            return false;
        }
    };
    // SAFETY: the context comes from the handler of this thread.
    let Some((regs, pc_at)) = (unsafe { thread_regs(uctx) }) else {
        return false;
    };
    // SAFETY: `regs` points into the saved state of this thread.
    let plan = unsafe { a64::plan_op(&op, &regs) };
    let (base, p) = (SHARED.base.load(Ordering::Relaxed), page());
    let (first, last) = (plan.ea, plan.ea.wrapping_add(plan.len - 1));
    // The access may cross into the next page (or come from the one before): both pages are looked at, the faulting one
    // `pg` (in flight, known) and its neighbour, whose protection did not matter for the fault.
    let page_of = |a: u64| (a >= base).then(|| ((a - base) / p) as usize);
    let (Some(pa), Some(pb)) = (page_of(first), page_of(last)) else {
        count_fallback(Reason::Mismatch, pc);
        return false;
    };
    if far < first || far > last || pg < pa || pg > pb || pb > pa + 1 {
        count_fallback(Reason::Mismatch, pc);
        return false;
    }
    // Results one frame late: guest memory holds the bytes of the last completed frame in the stale blocks, and the access
    // goes on through the alias without a wait. A store marks the blocks as the CPU's before it is done, so the copy of
    // the frame's bytes that may run at the same time leaves them alone, and the next flush stages them.
    let mut late = false;
    for q in pa..=pb {
        let (qlo, qhi) = (
            if q == pa { first } else { base + q as u64 * p },
            if q == pb {
                last
            } else {
                base + (q as u64 + 1) * p - 1
            },
        );
        let (info_q, cur_q) = if q == pg {
            (info, cur)
        } else {
            let Some(wq) = word(q) else {
                count_fallback(Reason::Mismatch, pc);
                return false;
            };
            let cur_q = wq.load(Ordering::Acquire);
            match cur_q & STATE_MASK {
                // Not tracked: nothing to look at.
                FREE => continue,
                CLEAN | DIRTY | GPU | OPEN => {}
                _ => {
                    count_fallback(Reason::Mismatch, pc);
                    return false;
                }
            }
            let Some(iq) = self::info(q) else {
                count_fallback(Reason::Mismatch, pc);
                return false;
            };
            (iq, cur_q)
        };
        let stale = info_q.stale();
        let (lo, hi) = (((qlo - base) % p) / BLOCK, ((qhi - base) % p) / BLOCK);
        let is_stale = |b: u64| stale[(b / 64) as usize] >> (b % 64) & 1 != 0;
        if !(lo..=hi).any(is_stale) {
            continue;
        }
        let snap = info_q.snap.load(Ordering::Acquire);
        let flying = q == pg
            || (matches!(cur_q & STATE_MASK, GPU | OPEN) && page_flying(q, cur_q >> FRAME_SHIFT));
        if snap == 0 || !SHARED.late.load(Ordering::Relaxed) || !flying {
            if snap == 0 && flying {
                let why = info_q.snap_why.load(Ordering::Relaxed) as usize;
                SHARED.snap_misses[why.min(SnapWhy::COUNT - 1)].fetch_add(1, Ordering::Relaxed);
            }
            count_fallback(Reason::Overlap, pc);
            return false;
        }
        late = true;
        if op.writes() {
            let mut owned = 0u64;
            for b in (lo..=hi).filter(|&b| is_stale(b)) {
                info_q.own[(b / 64) as usize].fetch_or(1 << (b % 64), Ordering::AcqRel);
                owned += 1;
            }
            info_q.own_frame.fetch_max(
                (SHARED.flushed.load(Ordering::Acquire) & FRAME_MASK) as u32,
                Ordering::AcqRel,
            );
            SHARED.late_owned.fetch_add(owned, Ordering::Relaxed);
        }
        let age = (cur_q >> FRAME_SHIFT).saturating_sub(snap).max(1);
        SHARED
            .late_age_sum
            .fetch_add(u64::from(age), Ordering::Relaxed);
        SHARED.late_age[(age as usize - 1).min(3)].fetch_add(1, Ordering::Relaxed);
        SHARED
            .late_age_max
            .fetch_max(u64::from(age), Ordering::Relaxed);
        // The page is a CPU-read GPU result: the completion of the frames keeps copying it.
        info_q
            .eager
            .store(SHARED.epoch.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    }
    // SAFETY: the alias maps the same pages as guest memory with read and write access, and `plan` lies inside the
    // pages `pa` and `pb`, which lie inside guest memory. The registers are the thread's own.
    unsafe {
        if !a64::perform_op(&op, &regs, &plan, (alias + (first - base)) as *mut u8) {
            // An atomic on an address that is not aligned: the hardware would fault; the ordinary path decides.
            count_fallback(Reason::Mismatch, pc);
            return false;
        }
        pc_at.write(pc + 4);
    }
    if op.writes() {
        for q in pa..=pb {
            let at = if q == pa { first } else { base + q as u64 * p };
            let Some(info_q) = (if q == pg { Some(info) } else { self::info(q) }) else {
                continue;
            };
            // A dirty page is staged by the flush anyway; a page that is not tracked is not staged at all.
            let state = if q == pg {
                cur & STATE_MASK
            } else {
                word(q).map_or(FREE, |w| w.load(Ordering::Acquire) & STATE_MASK)
            };
            if matches!(state, FREE | DIRTY) {
                continue;
            }
            if info_q.emu.fetch_or(1, Ordering::AcqRel) & 1 == 0 {
                // The first store since the flush took the page: the next flush stages it, and it counts as a first write
                // by the phase of the newest frame, as a write that dirties a page does.
                log_dirty(q);
                let (ph, newest) = phase_now();
                SHARED.phase_writes[ph].fetch_add(1, Ordering::Relaxed);
                if ph == 1 {
                    let r = newest as usize % P1_RING;
                    let n = SHARED.p1_n[r].fetch_add(1, Ordering::AcqRel) as usize;
                    if n < P1_CAP {
                        SHARED.p1_log[r][n].store(at.wrapping_sub(base) + 1, Ordering::Release);
                    }
                    count_pc_in(3, pc);
                }
            }
        }
    }
    if late {
        let c = if op.writes() {
            &SHARED.late_writes
        } else {
            &SHARED.late_reads
        };
        c.fetch_add(1, Ordering::Relaxed);
    }
    SHARED.emu[op.form() as usize].fetch_add(1, Ordering::Relaxed);
    SHARED
        .emu_ns
        .fetch_add(now_ns().saturating_sub(t0), Ordering::Relaxed);
    true
}
