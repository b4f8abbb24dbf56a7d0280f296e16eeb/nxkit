//! The flush of a frame in mirrored mode.

use super::*;

impl Pages {
    /// The flush of frame `frame` when there is a mirror ([`set_mirror`](Self::set_mirror)); the mirror of every page
    /// in `members` must be readable and writable.
    ///
    /// 1. The pages of `members` (host address and length) that are not tracked yet become tracked and read only, and
    ///    their guest bytes are copied into the mirror (the protection first, the copy second: a store after the
    ///    protection faults and is staged below or at the next flush).
    /// 2. The pages that the CPU dirtied since the last flush go back to clean and read only, and `stager` takes their
    ///    bytes (protection first, copy second).
    /// 3. The pages of `written` are tagged with the frame and lose all access. A page that was staged is tagged fresh.
    ///    With `poison` their guest bytes are first overwritten with the pattern, so that a read of guest memory that
    ///    should have gone to the mirror shows.
    ///
    /// The completion of the frame ([`complete`](Self::complete)) leaves the pages tagged: guest memory is stale until a
    /// CPU access copies the page from the mirror.
    pub fn flush_mirrored(
        &self,
        frame: u64,
        members: &[(u64, u64)],
        written: &[(u64, u64)],
        poison: Option<u32>,
        stager: &mut dyn Stager,
    ) -> FlushStats {
        let mut st = FlushStats::default();
        let mirror = SHARED.mirror.load(Ordering::Acquire);
        if !self.attached() || mirror == 0 {
            return st;
        }
        let t0 = now_ns();
        let mut lap = t0;
        let mut tick = || {
            let now = now_ns();
            let d = now.saturating_sub(lap);
            lap = now;
            d
        };
        let calls0 = SHARED.mprotects.load(Ordering::Relaxed);
        let prot0 = SHARED.mprotect_ns.load(Ordering::Relaxed);
        let mut c = ctrl();
        let cc = &mut *c;
        SHARED
            .epoch
            .store((frame / EPOCH_FRAMES) as u32, Ordering::Relaxed);
        let p = page();
        let base = SHARED.base.load(Ordering::Acquire);
        // From here until the commit of the frame, first writes of the CPU are counted and logged for it.
        SHARED.p1_n[frame as usize % P1_RING].store(0, Ordering::Release);
        SHARED.flushed.store(frame, Ordering::Release);
        let runs_w = span_runs(written);
        let mut all: Vec<(u64, u64)> = members.to_vec();
        all.extend_from_slice(written);
        let runs_m = span_runs(&all);
        // 1. New members.
        let mut newly: Vec<u32> = Vec::new();
        for &(s, e) in &runs_m {
            for pg in s..e {
                if let Some(w) = word(pg as usize)
                    && w.load(Ordering::Relaxed) & STATE_MASK == FREE
                    && w.compare_exchange(FREE, CLEAN, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    newly.push(pg);
                    member_bit(pg as usize, true);
                }
            }
        }
        for (s, e) in runs_of(&newly) {
            protect_pages(s, e, PROT_READ, true);
            // SAFETY: both ranges lie inside guest memory and its mirror (page numbers are clipped to the table); the
            // guest pages are readable (just protected read only), the mirror pages are writable (the caller made them
            // usable) and the two are different mappings.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (base + u64::from(s) * p) as *const u8,
                    (mirror + u64::from(s) * p) as *mut u8,
                    ((e - s) as u64 * p) as usize,
                );
            }
        }
        SHARED
            .members
            .fetch_add(newly.len() as u64, Ordering::AcqRel);
        st.new_members = newly.len() as u32;
        st.new_ns = tick();
        // 2. The dirty pages.
        let n = SHARED.log_n.swap(0, Ordering::AcqRel);
        let mut dirty: Vec<u32> = Vec::new();
        if n > LOG_CAP {
            st.scanned = true;
            let total = SHARED.npages.load(Ordering::Acquire);
            for pg in 0..total {
                if word(pg).is_some_and(|w| w.load(Ordering::Relaxed) == DIRTY) {
                    dirty.push(pg as u32);
                }
            }
        } else {
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
        }
        // The pages the handler stored to without opening them.
        let mut emu: Vec<u32> = dirty
            .iter()
            .copied()
            .filter(|&pg| info(pg as usize).is_some_and(|i| i.emu.load(Ordering::Acquire) & 1 != 0))
            .collect();
        if st.scanned {
            for pg in 0..SHARED.npages.load(Ordering::Acquire) {
                if word(pg).is_some_and(|w| w.load(Ordering::Relaxed) & STATE_MASK != FREE)
                    && info(pg).is_some_and(|i| i.emu.load(Ordering::Acquire) & 1 != 0)
                {
                    emu.push(pg as u32);
                }
            }
            emu.sort_unstable();
            emu.dedup();
        }
        st.log_ns = tick();
        stager.begin(dirty.len() + emu.len() + cc.hot.len() + 64);
        // The pages the CPU dirtied in several flushes in a row stay writable (step 5).
        let hot_on = SHARED.hot_on.load(Ordering::Acquire);
        let mut to_hot: Vec<u32> = Vec::new();
        if hot_on {
            for &pg in &cc.prev_dirty {
                if dirty.binary_search(&pg).is_err()
                    && let Some(x) = cc.streak.get_mut(pg as usize)
                {
                    *x = 0;
                }
            }
            for &pg in &dirty {
                if let Some(x) = cc.streak.get_mut(pg as usize) {
                    *x = x.saturating_add(1);
                    if *x >= HOT_AFTER && !in_runs(&runs_w, pg) && !cc.hot.contains_key(&pg) {
                        to_hot.push(pg);
                    }
                }
            }
            cc.prev_dirty.clone_from(&dirty);
        }
        let mut back: Vec<u32> = Vec::with_capacity(dirty.len());
        for pg in dirty {
            if cc.hot.contains_key(&pg) || to_hot.binary_search(&pg).is_ok() {
                continue;
            }
            if word(pg as usize).is_some_and(|w| {
                w.compare_exchange(DIRTY, CLEAN, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            }) {
                back.push(pg);
            }
        }
        st.dirtied = back.len() as u32;
        let stage_run = |stager: &mut dyn Stager,
                         carry: &mut BTreeMap<u32, Carry>,
                         s: u32,
                         e: u32| {
            for pg in s..e {
                // SAFETY: the page lies in guest memory and is readable (read only or open) since the protection above.
                let bytes = unsafe {
                    std::slice::from_raw_parts((base + u64::from(pg) * p) as *const u8, p as usize)
                };
                stage_carry(stager, carry, pg, bytes, stage_skip(pg as usize));
            }
        };
        for (s, e) in runs_of(&back) {
            protect_pages(s, e, PROT_READ, true);
            stage_run(stager, &mut cc.carry, s, e);
        }
        st.staged = back.len() as u32;
        st.emu_staged = self.stage_emulated(&emu, &back, stager, &mut cc.carry);
        st.staged += st.emu_staged;
        st.stage_ns = tick();
        // The writable pages: each is compared with the bytes staged for it last, and staged when it differs. A page that
        // did not change for [`HOT_QUIET`] flushes is protected again; the compare after the protection catches a store
        // that came in between.
        st.promoted = to_hot.len() as u32;
        for pg in to_hot {
            cc.hot.insert(
                pg,
                Hot {
                    shadow: Vec::new(),
                    quiet: 0,
                },
            );
        }
        if !cc.hot.is_empty() {
            let mut gone: Vec<u32> = Vec::new();
            let mut quiet: Vec<u32> = Vec::new();
            for (&pg, h) in cc.hot.iter_mut() {
                let still = word(pg as usize)
                    .is_some_and(|w| w.load(Ordering::Acquire) & STATE_MASK == DIRTY);
                if !still || in_runs(&runs_w, pg) {
                    // A frame writes it (step 3 stages it and tags it) or it is not tracked any more.
                    gone.push(pg);
                    continue;
                }
                // SAFETY: the page lies in guest memory and is readable and writable (it is dirty); the CPU may store
                // to it at the same time, which the staging accepts: the shadow is what is staged, so a later store is
                // found at the next flush.
                let live = unsafe {
                    std::slice::from_raw_parts((base + u64::from(pg) * p) as *const u8, p as usize)
                };
                if h.shadow.len() == p as usize
                    && h.shadow[..] == *live
                    && !cc.carry.contains_key(&pg)
                {
                    st.hot_same += 1;
                    h.quiet += 1;
                    if h.quiet >= HOT_QUIET {
                        quiet.push(pg);
                    }
                    continue;
                }
                h.quiet = 0;
                h.shadow.resize(p as usize, 0);
                h.shadow.copy_from_slice(live);
                stage_carry(
                    stager,
                    &mut cc.carry,
                    pg,
                    &h.shadow,
                    stage_skip(pg as usize),
                );
                st.hot_staged += 1;
            }
            for pg in &gone {
                cc.hot.remove(pg);
                if let Some(x) = cc.streak.get_mut(*pg as usize) {
                    *x = 0;
                }
            }
            let mut demote: Vec<(u32, Vec<u8>)> = Vec::new();
            for pg in quiet {
                if let Some(h) = cc.hot.remove(&pg)
                    && word(pg as usize).is_some_and(|w| {
                        w.compare_exchange(DIRTY, CLEAN, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                    })
                {
                    demote.push((pg, h.shadow));
                }
                if let Some(x) = cc.streak.get_mut(pg as usize) {
                    *x = 0;
                }
            }
            let pgs: Vec<u32> = demote.iter().map(|d| d.0).collect();
            for (s, e) in runs_of(&pgs) {
                protect_pages(s, e, PROT_READ, true);
            }
            for (pg, shadow) in &demote {
                // SAFETY: as above; the page is read only now, so nothing changes under the compare.
                let live = unsafe {
                    std::slice::from_raw_parts((base + u64::from(*pg) * p) as *const u8, p as usize)
                };
                if shadow[..] != *live || cc.carry.contains_key(pg) {
                    stage_carry(stager, &mut cc.carry, *pg, live, stage_skip(*pg as usize));
                    st.hot_staged += 1;
                }
            }
            st.demoted = demote.len() as u32;
            st.staged += st.hot_staged;
        }
        st.hot = cc.hot.len() as u32;
        st.hot_ns = tick();
        // 3. The pages of the frame.
        let masks = range_masks(&runs_w, written);
        let late_on = SHARED.late.load(Ordering::Acquire);
        let alias = SHARED.alias.load(Ordering::Acquire);
        let mut late_pages: Vec<u32> = Vec::new();
        for (ri, &(s, e)) in runs_w.iter().enumerate() {
            for pg in s..e {
                let Some(w) = word(pg as usize) else { continue };
                if late_on && alias != 0 {
                    // A page that a complete frame still tags and that game threads read late: its guest bytes are the
                    // last completed frame's only after the copy, which the copy-back thread may not have done yet.
                    let cur = w.load(Ordering::Acquire);
                    if cur & STATE_MASK == GPU
                        && !in_flight(cur >> FRAME_SHIFT)
                        && let Some(i) = info(pg as usize)
                        && i.snap.load(Ordering::Acquire) != 0
                    {
                        if late_active(i) {
                            let f = cur >> FRAME_SHIFT;
                            if !late_current(i, f) {
                                copy_stale(pg as usize, mirror, alias, false, f);
                                SHARED.late_flush_copies.fetch_add(1, Ordering::Relaxed);
                            }
                            for a in &i.stale {
                                a.store(0, Ordering::Release);
                            }
                            i.snap.store(f.max(1), Ordering::Release);
                        } else {
                            i.snap_reset(SnapWhy::FlushIdle);
                        }
                    }
                }
                let mut fresh = back.binary_search(&pg).is_ok();
                let mut tries = 0;
                loop {
                    let cur = w.load(Ordering::Acquire);
                    match cur & STATE_MASK {
                        DIRTY => {
                            // Written after step 2: take it back to clean and stage it again.
                            if w.compare_exchange(DIRTY, CLEAN, Ordering::AcqRel, Ordering::Acquire)
                                .is_ok()
                            {
                                protect_pages(pg, pg + 1, PROT_READ, true);
                                stage_run(stager, &mut cc.carry, pg, pg + 1);
                                fresh = true;
                                st.raced += 1;
                                st.staged += 1;
                            }
                        }
                        SYNC if tries < 1000 => {
                            // A thread copies the page right now; let it finish.
                            tries += 1;
                            std::thread::yield_now();
                        }
                        _ => {
                            let tag = pack(GPU, frame) | if fresh { FRESH } else { 0 };
                            if w.compare_exchange(cur, tag, Ordering::AcqRel, Ordering::Acquire)
                                .is_ok()
                            {
                                break;
                            }
                        }
                    }
                }
                // The blocks the frame may write are stale from here on, in guest memory, until a copy from the
                // mirror (the tag came first: a thread that copies the page waits for the frame, which is not done).
                if let Some(i) = info(pg as usize) {
                    i.or_stale(&masks[ri][(pg - s) as usize]);
                    if late_on
                        && alias != 0
                        && i.snap.load(Ordering::Acquire) != 0
                        && late_active(i)
                    {
                        late_pages.push(pg);
                    }
                }
            }
            // The pages lose all access first: from here a thread that touches a page faults, finds it tagged and
            // waits for the frame and copies the mirror. Only then is the pattern written, one page at a time, so a page
            // is open and holds the pattern for a few microseconds, not for the time of the whole run.
            //
            // Results one frame late: a page whose guest bytes are a completed frame's (`snap`) and that the completions
            // keep current stays readable. A read of it needs no trap: it gets the last completed frame's bytes, which is
            // what the handler served after the trap. Only stores fault (the handler marks the blocks as the CPU's).
            if late_on
                && alias != 0
                && poison.is_none()
                && SHARED.read_open_on.load(Ordering::Acquire)
            {
                let readable = |pg: u32| {
                    info(pg as usize).is_some_and(|i| i.snap.load(Ordering::Acquire) != 0)
                };
                let mut run_start = s;
                while run_start < e {
                    let open = readable(run_start);
                    let mut run_end = run_start + 1;
                    while run_end < e && readable(run_end) == open {
                        run_end += 1;
                    }
                    protect_pages(
                        run_start,
                        run_end,
                        if open { PROT_READ } else { PROT_NONE },
                        true,
                    );
                    if open {
                        SHARED
                            .read_open
                            .fetch_add(u64::from(run_end - run_start), Ordering::Relaxed);
                    }
                    run_start = run_end;
                }
            } else {
                protect_pages(s, e, PROT_NONE, true);
            }
            if let Some(pattern) = poison {
                for pg in s..e {
                    // The pattern replaces the bytes of the last completed frame: late readers must wait here.
                    if let Some(i) = info(pg as usize) {
                        i.snap_reset(SnapWhy::Poison);
                    }
                    let stale = info(pg as usize).map_or([u64::MAX; 4], PageInfo::stale);
                    protect_pages(pg, pg + 1, PROT_RW, true);
                    for b in 0..(p / BLOCK) {
                        if stale[(b / 64) as usize] >> (b % 64) & 1 == 0 {
                            continue;
                        }
                        // SAFETY: the page is writable now and lies in guest memory; the block lies in the page.
                        unsafe {
                            std::slice::from_raw_parts_mut(
                                (base + u64::from(pg) * p + b * BLOCK) as *mut u32,
                                (BLOCK / 4) as usize,
                            )
                            .fill(pattern);
                        }
                    }
                    protect_pages(pg, pg + 1, PROT_NONE, true);
                }
            }
            st.written += e - s;
        }
        st.written_runs = runs_w.len() as u32;
        if !runs_w.is_empty() {
            if late_on {
                cc.masks.push_back((frame, masks));
            }
            cc.frames.push_back((frame, runs_w));
        }
        if !late_pages.is_empty() {
            // `runs_w` is sorted and its runs are disjoint, so the pages are sorted.
            cc.late_copies.push_back(LateCopy {
                frame,
                pages: late_pages,
                start: 0,
                end: 0,
            });
        }
        // Blocks kept aside for pages no path above staged (released, or kept writable without a change).
        let lost: u32 = cc
            .carry
            .values()
            .map(|c| c.mask.iter().map(|x| x.count_ones()).sum::<u32>())
            .sum();
        SHARED
            .late_carry_lost
            .fetch_add(u64::from(lost), Ordering::Relaxed);
        cc.carry.clear();
        while cc.late_copies.len() > MAX_FRAMES {
            cc.late_copies.pop_front();
        }
        while cc.frames.len() > MAX_FRAMES {
            cc.frames.pop_front();
        }
        while cc.masks.len() > MAX_FRAMES {
            cc.masks.pop_front();
        }
        st.tag_ns = tick();
        st.mprotect_ns = SHARED
            .mprotect_ns
            .load(Ordering::Relaxed)
            .saturating_sub(prot0);
        st.mprotects = (SHARED.mprotects.load(Ordering::Relaxed) - calls0) as u32;
        st.ns = now_ns().saturating_sub(t0);
        st
    }
}
