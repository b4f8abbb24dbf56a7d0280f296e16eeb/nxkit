//! The flush of a frame, its completion, the readers, and the flush with results one frame late.

use super::*;

impl Pages {
    /// The flush of frame `frame`. First the pages that the CPU dirtied since the last flush go back to clean and read
    /// only, except the ones the new frame may write; then the pages in `written` (host address and length) are
    /// protected against all access and tagged with the frame, becoming tracked if they were not. The frame's runs
    /// are released by [`complete`](Self::complete).
    pub fn flush(&self, frame: u64, written: &[(u64, u64)]) -> FlushStats {
        let mut st = FlushStats::default();
        if !self.attached() {
            return st;
        }
        let t0 = now_ns();
        let calls0 = SHARED.mprotects.load(Ordering::Relaxed);
        let mut c = ctrl();
        let runs = span_runs(written);
        // The pages that went dirty.
        let n = SHARED.log_n.swap(0, Ordering::AcqRel);
        let mut dirty: Vec<u32> = Vec::new();
        if n > LOG_CAP {
            st.scanned = true;
            let total = SHARED.npages.load(Ordering::Acquire);
            for p in 0..total {
                if word(p).is_some_and(|w| w.load(Ordering::Relaxed) == DIRTY) {
                    dirty.push(p as u32);
                }
            }
        } else {
            for slot in &SHARED.log[..n] {
                // A handler that took its index may not have stored yet; it does so within a few instructions.
                let mut spins = 0;
                let v = loop {
                    let v = slot.swap(0, Ordering::AcqRel);
                    if v != 0 || spins > 10_000 {
                        break v;
                    }
                    spins += 1;
                    std::hint::spin_loop();
                };
                if v != 0 {
                    dirty.push(v - 1);
                }
            }
            dirty.sort_unstable();
            dirty.dedup();
        }
        let mut back: Vec<u32> = Vec::with_capacity(dirty.len());
        for p in dirty {
            if in_runs(&runs, p) {
                continue;
            }
            // The state goes first: a store after this CAS and before the protection is in guest memory already, and a
            // store after the protection faults and is logged for the next flush.
            if word(p as usize).is_some_and(|w| {
                w.compare_exchange(DIRTY, CLEAN, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            }) {
                back.push(p);
            }
        }
        st.dirtied = back.len() as u32;
        for (s, e) in runs_of(&back) {
            protect_pages(s, e, PROT_READ, true);
        }
        // The pages of the new frame.
        let mut added = 0u64;
        for &(s, e) in &runs {
            for p in s..e {
                if let Some(w) = word(p as usize)
                    && w.swap(pack(GPU, frame), Ordering::AcqRel) & STATE_MASK == FREE
                {
                    added += 1;
                    member_bit(p as usize, true);
                }
            }
            protect_pages(s, e, PROT_NONE, true);
            st.written += e - s;
        }
        SHARED.members.fetch_add(added, Ordering::AcqRel);
        st.new_members = added as u32;
        st.written_runs = runs.len() as u32;
        if !runs.is_empty() {
            c.frames.push_back((frame, runs));
        }
        while c.frames.len() > MAX_FRAMES {
            if let Some((f, r)) = c.frames.pop_front() {
                release_runs(f, &r);
            }
        }
        st.mprotects = (SHARED.mprotects.load(Ordering::Relaxed) - calls0) as u32;
        st.ns = now_ns().saturating_sub(t0);
        st
    }

    /// The pages that game threads waited for as the writer of a frame lately (within `LEARN_FRAMES` frames of `now`), sorted:
    /// CPU-read GPU results. The encoder of a frame cuts the frame after the last pass that writes such a page and calls
    /// [`release_early`](Self::release_early) when that part is done (step 6).
    pub fn readers(&self, now: u64) -> Vec<u32> {
        let mut l = LEARNED.lock().unwrap_or_else(PoisonError::into_inner);
        let n = SHARED.readers_n.swap(0, Ordering::AcqRel).min(READERS_CAP);
        let mut changed = 0u64;
        for slot in &SHARED.readers_log[..n] {
            let v = slot.swap(0, Ordering::AcqRel);
            if v != 0 && l.pages.insert(v - 1) {
                changed += 1;
            }
        }
        let now = now & FRAME_MASK;
        let old: Vec<u32> = l
            .pages
            .iter()
            .copied()
            .filter(|&pg| {
                info(pg as usize).is_none_or(|i| {
                    let w = u64::from(i.waited.load(Ordering::Relaxed));
                    w == 0 || now.saturating_sub(w) > LEARN_FRAMES
                })
            })
            .collect();
        for pg in &old {
            l.pages.remove(pg);
        }
        changed += old.len() as u64;
        l.changes += changed;
        l.pages.iter().copied().collect()
    }

    /// The last pass of `frame` that writes the pages of `runs` (page numbers, first and end) is done on the GPU: a thread that
    /// waits for the writer of one of them goes on, before the rest of the frame completes. Only pages that no later pass
    /// of the frame writes may be named (step 6).
    pub fn release_early(&self, frame: u64, runs: &[(u32, u32)]) {
        let frame = (frame & FRAME_MASK) as u32;
        let mut n = 0u64;
        for &(first, end) in runs {
            for pg in first..end {
                if let Some(i) = info(pg as usize) {
                    i.ready.fetch_max(frame, Ordering::AcqRel);
                    n += 1;
                }
            }
        }
        SHARED.early_calls.fetch_add(1, Ordering::Relaxed);
        SHARED.early_pages.fetch_add(n, Ordering::Relaxed);
        wake_all();
    }

    /// The GPU finished frame `frame` (and, by the order of the queue, every frame before it). Its pages that are still
    /// protected for it go to clean and read only. It runs on the thread of the Metal completion handler.
    pub fn complete(&self, frame: u64) {
        SHARED.completed.fetch_max(frame, Ordering::AcqRel);
        if SHARED.mirror.load(Ordering::Acquire) != 0 {
            // With a mirror the pages stay tagged: the guest bytes are stale until a CPU access copies them. Pages that
            // a host thread opened for reading while the frame ran lose their access again.
            let (due, mut due_masks, mut due_copies): (
                Vec<FrameRuns>,
                Vec<FrameMasks>,
                Vec<LateCopy>,
            ) = {
                let mut c = ctrl();
                let mut due = Vec::new();
                while c.frames.front().is_some_and(|(f, _)| *f <= frame) {
                    if let Some(x) = c.frames.pop_front() {
                        due.push(x);
                    }
                }
                let mut masks = Vec::new();
                while c.masks.front().is_some_and(|(f, _)| *f <= frame) {
                    if let Some(x) = c.masks.pop_front() {
                        masks.push(x);
                    }
                }
                let mut copies = Vec::new();
                while c.late_copies.front().is_some_and(|l| l.frame <= frame) {
                    if let Some(x) = c.late_copies.pop_front() {
                        copies.push(x);
                    }
                }
                (due, masks, copies)
            };
            let eager = SHARED.eager.load(Ordering::Acquire);
            let mirror = SHARED.mirror.load(Ordering::Acquire);
            let copy_back = SHARED.eager_on.load(Ordering::Acquire)
                && SHARED.alias.load(Ordering::Acquire) != 0;
            let mut later: Vec<FrameRuns> = Vec::new();
            let alias = SHARED.alias.load(Ordering::Acquire);
            if SHARED.late.load(Ordering::Acquire) && alias != 0 {
                // Under the control mutex, so that a flush that tags a page again is either before it (the page is
                // copied here) or after it (the flush copies a page of a complete frame itself).
                let mut c = ctrl();
                for (f, runs) in &due {
                    let m = due_masks
                        .iter()
                        .position(|(g, _)| g == f)
                        .map(|pos| due_masks.swap_remove(pos).1);
                    let copies = due_copies
                        .iter()
                        .position(|l| l.frame == *f)
                        .map(|pos| due_copies.swap_remove(pos));
                    late_copy_back(
                        *f,
                        runs,
                        m.as_deref(),
                        copies.as_ref(),
                        &mut c.carry,
                        mirror,
                        alias,
                    );
                }
            }
            for (f, runs) in due {
                close_opened(f, &runs);
                if copy_back {
                    later.push((f, runs.clone()));
                }
                if eager {
                    for &(first, end) in &runs {
                        for pg in first..end {
                            let Some(w) = word(pg as usize) else { continue };
                            let cur = w.load(Ordering::Acquire);
                            if cur & STATE_MASK == GPU
                                && u64::from(cur >> FRAME_SHIFT) == f & FRAME_MASK
                            {
                                serve_mirrored(pg as usize, w, false, true, mirror, 0);
                            }
                        }
                    }
                }
            }
            wake_all();
            SHARED.settled.fetch_max(frame, Ordering::AcqRel);
            if !later.is_empty() {
                eager_submit(later);
            }
            return;
        }
        let due: Vec<(u64, Vec<(u32, u32)>)> = {
            let mut c = ctrl();
            let mut due = Vec::new();
            while c.frames.front().is_some_and(|(f, _)| *f <= frame) {
                if let Some(x) = c.frames.pop_front() {
                    due.push(x);
                }
            }
            due
        };
        for (f, runs) in due {
            release_runs(f, &runs);
        }
        SHARED.settled.fetch_max(frame, Ordering::AcqRel);
    }

    /// The late staging of `NXKIT_PRIVATE=1` with the knob `privlatestage`: the pages that the CPU wrote since the flush of
    /// the frame go back to read only and `stager` takes their bytes again. Returns the pages.
    pub fn flush_late(&self, stager: &mut dyn Stager) -> Vec<u32> {
        if !self.attached() || SHARED.mirror.load(Ordering::Acquire) == 0 {
            return Vec::new();
        }
        let mut c = ctrl();
        let carry = &mut c.carry;
        let p = page();
        let base = SHARED.base.load(Ordering::Acquire);
        let n = SHARED.log_n.swap(0, Ordering::AcqRel).min(LOG_CAP);
        let mut dirty: Vec<u32> = Vec::new();
        for slot in &SHARED.log[..n] {
            let mut spins = 0;
            let v = loop {
                let v = slot.swap(0, Ordering::AcqRel);
                if v != 0 || spins > 10_000 {
                    break v;
                }
                spins += 1;
                std::hint::spin_loop();
            };
            if v != 0 {
                dirty.push(v - 1);
            }
        }
        dirty.sort_unstable();
        dirty.dedup();
        let emu: Vec<u32> = dirty
            .iter()
            .copied()
            .filter(|&pg| info(pg as usize).is_some_and(|i| i.emu.load(Ordering::Acquire) & 1 != 0))
            .collect();
        let mut back: Vec<u32> = Vec::new();
        for pg in dirty {
            if word(pg as usize).is_some_and(|w| {
                w.compare_exchange(DIRTY, CLEAN, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            }) {
                back.push(pg);
            }
        }
        stager.begin(back.len() + emu.len());
        for (s, e) in runs_of(&back) {
            protect_pages(s, e, PROT_READ, true);
            for pg in s..e {
                // SAFETY: the page lies in guest memory and is readable since the protection above.
                let bytes = unsafe {
                    std::slice::from_raw_parts((base + u64::from(pg) * p) as *const u8, p as usize)
                };
                stage_carry(stager, carry, pg, bytes, stage_skip(pg as usize));
            }
        }
        if self.stage_emulated(&emu, &back, stager, carry) > 0 {
            let extra: Vec<u32> = emu
                .iter()
                .copied()
                .filter(|pg| back.binary_search(pg).is_err())
                .collect();
            back.extend(extra);
            back.sort_unstable();
        }
        back
    }

    /// Stages the pages of `emu` (page numbers, sorted) that the handler stored to, except the ones in `done` (sorted),
    /// which were staged whole. The bytes come through the alias, because a page of a frame in flight has no access; the
    /// stale blocks go along with a mask and are not copied into the mirror. The flag is taken first: a store after it
    /// sets it again and logs the page for the next flush. Returns the pages staged.
    pub(super) fn stage_emulated(
        &self,
        emu: &[u32],
        done: &[u32],
        stager: &mut dyn Stager,
        carry: &mut BTreeMap<u32, Carry>,
    ) -> u32 {
        let mut n = 0;
        for &pg in emu {
            let Some(i) = info(pg as usize) else { continue };
            if i.emu.swap(0, Ordering::AcqRel) & 1 == 0 || done.binary_search(&pg).is_ok() {
                continue;
            }
            if word(pg as usize).is_none_or(|w| w.load(Ordering::Acquire) & STATE_MASK == FREE) {
                continue;
            }
            let Some(bytes) = self.alias_page(pg) else {
                continue;
            };
            stage_carry(stager, carry, pg, bytes, i.take_skip());
            n += 1;
        }
        SHARED.emu_staged.fetch_add(u64::from(n), Ordering::Relaxed);
        n
    }
}
