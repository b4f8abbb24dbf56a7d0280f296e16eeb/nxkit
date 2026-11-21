//! The methods of `PageInfo`: the stale, own and held blocks of a page and its snapshot.

use super::*;

impl PageInfo {
    /// Drops the snapshot of the page for `why` and counts the reason. A page that had none is left alone.
    pub(super) fn snap_reset(&self, why: SnapWhy) {
        if self.snap.swap(0, Ordering::AcqRel) != 0 {
            self.snap_why.store(why as u32, Ordering::Relaxed);
            SHARED.snap_resets[why as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn stale(&self) -> Mask {
        [
            self.stale[0].load(Ordering::Acquire),
            self.stale[1].load(Ordering::Acquire),
            self.stale[2].load(Ordering::Acquire),
            self.stale[3].load(Ordering::Acquire),
        ]
    }

    pub(super) fn or_stale(&self, m: &Mask) {
        for (a, b) in self.stale.iter().zip(m) {
            // A page that frames write over and over has the bits already: no read-modify-write then.
            if *b != 0 && a.load(Ordering::Relaxed) & *b != *b {
                a.fetch_or(*b, Ordering::AcqRel);
            }
        }
    }

    pub(super) fn clear(&self) {
        for a in &self.stale {
            a.store(0, Ordering::Release);
        }
        self.emu.store(0, Ordering::Release);
        self.eager.store(0, Ordering::Release);
        self.ready.store(0, Ordering::Release);
        self.waited.store(0, Ordering::Release);
        self.snap.store(0, Ordering::Release);
        self.snap_why
            .store(SnapWhy::Released as u32, Ordering::Relaxed);
        self.hold_frame.store(0, Ordering::Release);
        self.own_frame.store(0, Ordering::Release);
        for a in self.own.iter().chain(&self.hold) {
            a.store(0, Ordering::Release);
        }
    }

    pub(super) fn load_mask(a: &[AtomicU64; 4]) -> Mask {
        [
            a[0].load(Ordering::Acquire),
            a[1].load(Ordering::Acquire),
            a[2].load(Ordering::Acquire),
            a[3].load(Ordering::Acquire),
        ]
    }

    /// The blocks that a copy from the mirror for frame `frame` must leave alone: the ones the CPU wrote and no flush
    /// staged yet, and the ones a flush staged for a write after a frame that is not older than `frame`.
    pub(super) fn keep(&self, frame: u32) -> Mask {
        let own = Self::load_mask(&self.own);
        if frame > self.hold_frame.load(Ordering::Acquire) {
            return own;
        }
        let hold = Self::load_mask(&self.hold);
        std::array::from_fn(|i| own[i] | hold[i])
    }

    /// The mask for [`Stager::stage_masked`]: the stale blocks that the CPU did not write. The blocks the CPU wrote move to
    /// `hold` (they go into the mirror with this staging) and protect the copy-back of the frames flushed before the write. A
    /// `hold` that no frame in flight needs any more is dropped first.
    pub(super) fn take_skip(&self) -> Mask {
        let stale = self.stale();
        if self.own.iter().all(|a| a.load(Ordering::Relaxed) == 0) {
            return stale;
        }
        let hf = self.hold_frame.load(Ordering::Acquire);
        if u64::from(hf) <= SHARED.completed.load(Ordering::Acquire) & FRAME_MASK {
            for a in &self.hold {
                a.store(0, Ordering::Release);
            }
        }
        let mut skip = stale;
        for (i, a) in self.own.iter().enumerate() {
            let taken = a.swap(0, Ordering::AcqRel);
            self.hold[i].fetch_or(taken, Ordering::AcqRel);
            skip[i] &= !taken;
        }
        self.hold_frame
            .fetch_max(self.own_frame.load(Ordering::Acquire), Ordering::AcqRel);
        skip
    }
}

/// The mask for the staging of page `pg`.
pub(super) fn stage_skip(pg: usize) -> Mask {
    info(pg).map_or([0; 4], PageInfo::take_skip)
}
