//! Protection of page runs, the dirty log, internal threads, the program counter tables and the registers of a fault.

use super::*;

fn set_prot_raw(start: u64, end: u64, prot: c_int) -> bool {
    SHARED.mprotects.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the run lies in guest memory that `attach` named; the run is whole host pages. `mprotect` only changes
    // the access to them.
    let ok = unsafe { mprotect(start as *mut c_void, (end - start) as usize, prot) == 0 };
    if !ok {
        SHARED.mprotect_failed.fetch_add(1, Ordering::Relaxed);
    }
    ok
}

/// Protects the pages `[first, end)` (page numbers). Outside the handler this also adds the time to the counters.
pub(super) fn protect_pages(first: u32, end: u32, prot: c_int, timed: bool) -> bool {
    let base = SHARED.base.load(Ordering::Acquire);
    let p = page();
    let t0 = timed.then(now_ns);
    let ok = set_prot_raw(base + u64::from(first) * p, base + u64::from(end) * p, prot);
    if let Some(t0) = t0 {
        SHARED
            .mprotect_ns
            .fetch_add(now_ns().saturating_sub(t0), Ordering::Relaxed);
    }
    ok
}

/// Page numbers (first, end) of the host range `[addr, addr + len)`, clipped to the table. None when empty.
pub(super) fn page_span(addr: u64, len: u64) -> Option<(u32, u32)> {
    let base = SHARED.base.load(Ordering::Acquire);
    let total = SHARED.len.load(Ordering::Acquire);
    let p = page();
    let lo = addr.max(base);
    let hi = addr
        .saturating_add(len.max(1))
        .min(base.saturating_add(total));
    if lo >= hi {
        return None;
    }
    let first = (lo - base) / p;
    let end = (hi - base).div_ceil(p);
    let n = SHARED.npages.load(Ordering::Acquire) as u64;
    (first < n).then(|| (first as u32, end.min(n) as u32))
}

/// Pushes a page that went dirty into the log.
pub(super) fn log_dirty(page: usize) {
    let i = SHARED.log_n.fetch_add(1, Ordering::AcqRel);
    if i < LOG_CAP {
        SHARED.log[i].store(page as u32 + 1, Ordering::Release);
    } else {
        SHARED.log_overflows.fetch_add(1, Ordering::Relaxed);
    }
}

pub(super) fn is_internal() -> bool {
    // SAFETY: `pthread_self` has no preconditions.
    let me = unsafe { pthread_self() };
    SHARED
        .internal
        .iter()
        .any(|s| s.load(Ordering::Acquire) == me)
}

/// Marks the calling thread as part of the host's own way to the completion of a frame (the encoder thread, the code
/// that writes the counter reports). Its accesses to tracked pages are counted apart from the game's. The mark ends
/// when the guard drops.
pub fn mark_internal_thread() -> InternalThread {
    // SAFETY: `pthread_self` has no preconditions.
    let me = unsafe { pthread_self() };
    for (i, s) in SHARED.internal.iter().enumerate() {
        if s.compare_exchange(0, me, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            return InternalThread(Some(i));
        }
    }
    InternalThread(None)
}

/// The mark of [`mark_internal_thread`].
#[derive(Debug)]
pub struct InternalThread(Option<usize>);

impl Drop for InternalThread {
    fn drop(&mut self) {
        if let Some(i) = self.0 {
            SHARED.internal[i].store(0, Ordering::Release);
        }
    }
}

pub(super) fn count_pc(kind: FaultKind, pc: u64) {
    let k = kind as usize;
    if k > 2 {
        return;
    }
    count_pc_in(k, pc);
}

/// Counts `pc` in table `k` (0 to 2 by fault kind, 3 for writes between a flush and the commit of its frame).
pub(super) fn count_pc_in(k: usize, pc: u64) {
    if pc == 0 {
        return;
    }
    let table = &SHARED.pcs[k];
    for i in 0..PC_PROBE {
        let (key, n) = &table[((pc >> 2) as usize).wrapping_add(i) % PC_SLOTS];
        match key.compare_exchange(0, pc, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                n.fetch_add(1, Ordering::Relaxed);
                return;
            }
            Err(v) if v == pc => {
                n.fetch_add(1, Ordering::Relaxed);
                return;
            }
            Err(_) => {}
        }
    }
    SHARED.pc_overflow.fetch_add(1, Ordering::Relaxed);
}

/// The program counter and the write bit of the exception syndrome of the faulting instruction.
pub(super) fn fault_regs(uctx: *mut c_void) -> (u64, bool) {
    if uctx.is_null() {
        return (0, false);
    }
    // SAFETY: the kernel passes a valid `ucontext_t` to an SA_SIGINFO handler. Its `uc_mcontext` pointer is at byte 48;
    // the `__darwin_mcontext64` it points at starts with the exception state (`far` at 0, `esr` at 8) and holds the
    // program counter at byte 272. The fields are read, not written.
    unsafe {
        let mc = *uctx.cast::<u8>().add(48).cast::<*const u8>();
        if mc.is_null() {
            (0, false)
        } else {
            let esr = *mc.add(8).cast::<u32>();
            (*mc.add(272).cast::<u64>(), esr & (1 << 6) != 0)
        }
    }
}

/// The registers of the faulting thread in the saved state of the signal, and the address of its program counter.
///
/// # Safety
/// `uctx` must be the context that the kernel passed to an `SA_SIGINFO` handler of this thread on arm64 macOS.
pub(super) unsafe fn thread_regs(uctx: *mut c_void) -> Option<(a64::Regs, *mut u64)> {
    if uctx.is_null() {
        return None;
    }
    // SAFETY: the `ucontext_t` is valid; its `uc_mcontext` pointer is at byte 48. The `__darwin_mcontext64` it points
    // at holds the exception state (16 bytes), then x0 to x30 (31 words in a row, `fp` and `lr` are x29 and x30), the
    // stack pointer at byte 264, the program counter at 272, `cpsr` and a pad word, and the SIMD registers v0 to v31 at
    // byte 288. The pointers stay valid until the handler returns.
    unsafe {
        let mc = *uctx.cast::<u8>().add(48).cast::<*mut u8>();
        if mc.is_null() {
            return None;
        }
        Some((
            a64::Regs {
                x: mc.add(16).cast(),
                sp: mc.add(264).cast(),
                v: mc.add(288).cast(),
            },
            mc.add(272).cast(),
        ))
    }
}

pub(super) fn count_fallback(why: Reason, pc: u64) {
    SHARED.fallback[why as usize].fetch_add(1, Ordering::Relaxed);
    count_pc_in(PC_FALLBACK, pc);
}
