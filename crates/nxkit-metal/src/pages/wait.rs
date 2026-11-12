//! Frames in flight, the member bitmap, and the waits of game threads for a writer.

use super::*;

/// True when `frame` (28 bits) is a frame whose completion has not been reported.
pub(super) fn in_flight(frame: u32) -> bool {
    u64::from(frame) > SHARED.completed.load(Ordering::Acquire) & FRAME_MASK
}

/// True when the writer of page `pg` in `frame` still runs: the frame is not complete and the last pass that writes the page
/// has not reported (step 6).
pub(super) fn page_flying(pg: usize, frame: u32) -> bool {
    in_flight(frame) && info(pg).is_none_or(|i| i.ready.load(Ordering::Acquire) < frame)
}

/// A game thread is about to wait for the writer of `pg` in `frame`: the page becomes a CPU-read GPU result for
/// [`Pages::readers`] (atomics only).
pub(super) fn learn_reader(pg: usize, frame: u32) {
    let Some(i) = info(pg) else { return };
    let prev = i.waited.swap(frame, Ordering::Relaxed);
    if prev == 0 || u64::from(frame.saturating_sub(prev)) >= LEARN_FRAMES / 4 {
        let n = SHARED.readers_n.fetch_add(1, Ordering::AcqRel);
        if n < READERS_CAP {
            SHARED.readers_log[n].store(pg as u32 + 1, Ordering::Release);
        }
    }
}

/// Sets or clears the member bit of `page`. Not for the signal handler.
pub(super) fn member_bit(page: usize, set: bool) {
    let p = SHARED.member_bits.load(Ordering::Acquire);
    if p.is_null() || page >= SHARED.npages.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: the bitmap has `npages.div_ceil(64)` words that are never freed while the process runs (a new `attach`
    // leaks the old one), and `page` is inside the table.
    let w = unsafe { &*p.add(page / 64) };
    let bit = 1u64 << (page % 64);
    if set {
        w.fetch_or(bit, Ordering::AcqRel);
    } else {
        w.fetch_and(!bit, Ordering::AcqRel);
    }
    SHARED.member_gen.fetch_add(1, Ordering::AcqRel);
}

/// Sleeps until the wake word changes from `seen` or the slice ends.
pub(super) fn sleep_on_wake(seen: u32) {
    // SAFETY: the address of a static atomic of 4 bytes; the kernel reads 4 bytes. Safe in a signal handler.
    unsafe {
        os_sync_wait_on_address_with_timeout(
            (&raw const SHARED.wake).cast_mut().cast(),
            u64::from(seen),
            4,
            0,
            OS_CLOCK_MACH_ABSOLUTE_TIME,
            SLICE_NS,
        );
    }
}

pub(super) fn wake_all() {
    SHARED.wake.fetch_add(1, Ordering::AcqRel);
    // SAFETY: the address of a static atomic of 4 bytes.
    unsafe {
        os_sync_wake_by_address_all((&raw const SHARED.wake).cast_mut().cast(), 4, 0);
    }
}

/// Where the newest frame is, and its number: 1 flushed and not committed, 2 committed and running, 0 done.
pub(super) fn phase_now() -> (usize, u64) {
    let f = SHARED.flushed.load(Ordering::Acquire);
    let c = SHARED.committed.load(Ordering::Acquire);
    if f > c {
        (1, f)
    } else if c > SHARED.completed.load(Ordering::Acquire) {
        (2, c)
    } else {
        (0, c)
    }
}

/// Waits until the writer of page `pg` in `frame` is done: the completion of the frame is reported or the last pass that
/// writes the page signalled (step 6), for an access that `write` says is a write. Returns true when the wait ended at the
/// timeout.
pub(super) fn wait_complete(pg: usize, frame: u32, write: bool) -> bool {
    let t0 = now_ns();
    let uncommitted = u64::from(frame) > SHARED.committed.load(Ordering::Acquire) & FRAME_MASK;
    let mut waited = false;
    let mut timed_out = false;
    loop {
        let seen = SHARED.wake.load(Ordering::Acquire);
        if !page_flying(pg, frame) {
            break;
        }
        waited = true;
        if now_ns().saturating_sub(t0) > TIMEOUT_NS {
            timed_out = true;
            break;
        }
        sleep_on_wake(seen);
    }
    if waited {
        let ns = now_ns().saturating_sub(t0);
        let k = usize::from(write);
        SHARED.writer_waits.fetch_add(1, Ordering::Relaxed);
        SHARED.writer_wait_ns.fetch_add(ns, Ordering::Relaxed);
        SHARED.wait_n[k].fetch_add(1, Ordering::Relaxed);
        SHARED.wait_ns[k].fetch_add(ns, Ordering::Relaxed);
        SHARED.wait_max_ns[k].fetch_max(ns, Ordering::Relaxed);
        if uncommitted {
            SHARED.wait_uncommitted[k].fetch_add(1, Ordering::Relaxed);
        }
        // The frame still runs when the pass signal ended the wait.
        let by_pass = usize::from(!(in_flight(frame) && !timed_out));
        SHARED.wait_end[by_pass].fetch_add(1, Ordering::Relaxed);
        SHARED.wait_end_ns[by_pass].fetch_add(ns, Ordering::Relaxed);
    }
    if timed_out {
        SHARED.wait_timeouts.fetch_add(1, Ordering::Relaxed);
    }
    timed_out
}
