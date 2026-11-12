//! System calls of the host on guest memory: settling, opening and forgetting pages.

use super::*;

impl Pages {
    /// The host reads or writes the range through a system call, and there is a mirror: the pages in the range that
    /// are tagged with a frame are brought up to date (the caller waits for the writer, an internal thread does not),
    /// so that the system call finds current bytes. A write leaves them dirty.
    pub(super) fn settle(&self, addr: u64, len: u64, write: bool) {
        let Some((first, end)) = page_span(addr, len) else {
            return;
        };
        let mirror = SHARED.mirror.load(Ordering::Acquire);
        let internal = is_internal();
        for p in first..end {
            let Some(w) = word(p as usize) else { break };
            for _ in 0..64 {
                let state = w.load(Ordering::Acquire) & STATE_MASK;
                match state {
                    GPU => {
                        if !write && !internal && late_host_read(p as usize, w, addr, len) {
                            // A read outside the bytes the frame writes, of a page whose guest bytes are a completed
                            // frame's: the read needs no wait (the caller opens the page for reading next).
                            SHARED.settle_late.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                        let k = serve_mirrored(p as usize, w, write, internal, mirror, addr);
                        if matches!(k, FaultKind::ReadInFlight | FaultKind::WriteInFlight) {
                            SHARED.settle_waits.fetch_add(1, Ordering::Relaxed);
                        }
                        SHARED.faults[k as usize][FaultOrigin::Host as usize]
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    OPEN if write => {
                        let k = serve_mirrored(p as usize, w, write, internal, mirror, addr);
                        SHARED.faults[k as usize][FaultOrigin::Host as usize]
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    SYNC => sleep_on_wake(SHARED.wake.load(Ordering::Acquire)),
                    _ => break,
                }
            }
        }
    }

    /// The host reads or writes the range through a system call. A tracked page that a read needs and that is protected
    /// against all access opens for reading; a write dirties the tracked pages and opens them for writing.
    pub(super) fn open(&self, addr: u64, len: u64, write: bool) {
        let Some((first, end)) = page_span(addr, len) else {
            return;
        };
        let prot = if write { PROT_RW } else { PROT_READ };
        let mut start: Option<u32> = None;
        for p in first..end {
            let Some(w) = word(p as usize) else { break };
            let mut changed = false;
            loop {
                let cur = w.load(Ordering::Acquire);
                let state = cur & STATE_MASK;
                if write && (state == CLEAN || state == GPU || state == OPEN) {
                    if state == GPU && in_flight(cur >> FRAME_SHIFT) {
                        SHARED.host_access[1].fetch_add(1, Ordering::Relaxed);
                    }
                    if w.compare_exchange(cur, DIRTY, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                    {
                        continue;
                    }
                    log_dirty(p as usize);
                    SHARED.hook_dirtied.fetch_add(1, Ordering::Relaxed);
                    changed = true;
                } else if !write && state == GPU {
                    if in_flight(cur >> FRAME_SHIFT) {
                        SHARED.host_access[0].fetch_add(1, Ordering::Relaxed);
                    }
                    changed = true;
                }
                break;
            }
            match (changed, start) {
                (true, None) => start = Some(p),
                (false, Some(s)) => {
                    protect_pages(s, p, prot, true);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            protect_pages(s, end, prot, true);
        }
    }

    /// The pages in the range stop being tracked and become readable and writable: the host changes their permission or
    /// gives them back.
    pub(super) fn forget(&self, addr: u64, len: u64) {
        let Some((first, end)) = page_span(addr, len) else {
            return;
        };
        let mut start: Option<u32> = None;
        for p in first..end {
            let member =
                word(p as usize).is_some_and(|w| w.load(Ordering::Acquire) & STATE_MASK != FREE);
            match (member, start) {
                (true, None) => start = Some(p),
                (false, Some(s)) => {
                    protect_pages(s, p, PROT_RW, true);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            protect_pages(s, end, PROT_RW, true);
        }
        // Open first, free after: a thread that faulted meanwhile finds a tracked page and opens it.
        let mut freed = 0u64;
        for p in first..end {
            if let Some(w) = word(p as usize)
                && w.swap(FREE, Ordering::AcqRel) & STATE_MASK != FREE
            {
                freed += 1;
                member_bit(p as usize, false);
                if let Some(i) = info(p as usize) {
                    i.clear();
                }
            }
        }
        let members = SHARED.members.load(Ordering::Acquire);
        SHARED
            .members
            .fetch_sub(freed.min(members), Ordering::AcqRel);
    }
}
