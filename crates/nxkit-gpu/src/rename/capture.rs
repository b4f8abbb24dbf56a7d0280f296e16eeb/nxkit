//! The renamer at the flush, at the encode of a batch slot and after a wait: capture, binding and write-back.

use super::*;

impl Renamer {
    /// The flush of a frame, on the CPU thread. Takes the `pre` bytes of the ranges that are judged and the `start`
    /// bytes of those that this frame renames. Returns the capture of the renamed ranges, when there are any.
    pub(crate) fn at_flush(
        &self,
        detail: &FrameDetail,
        fence: u64,
    ) -> (Option<Capture>, Option<Vec<Recheck>>) {
        self.timed(0, || self.at_flush_inner(detail, fence))
    }

    fn at_flush_inner(
        &self,
        detail: &FrameDetail,
        fence: u64,
    ) -> (Option<Capture>, Option<Vec<Recheck>>) {
        let mut st = lock(&self.state);
        let mut force = 0u8;
        for (i, n) in FORCE_KNOBS.iter().enumerate() {
            if crate::knob(n) {
                force |= 1 << i;
            }
        }
        if st.cands.is_empty() && st.keys.is_empty() {
            let all = force >> 6 & 1 != 0;
            self.honest
                .store(if all { fence } else { 0 }, Ordering::Release);
            self.honest_why.store(4, Ordering::Relaxed);
            self.forced_mask
                .store(if all { 1 << 6 } else { 0 }, Ordering::Relaxed);
            return (None, None);
        }
        let t_bind = crate::diag("rename").then(std::time::Instant::now);
        let (bindings, bound) = st.bindings(detail);
        let check = st.adopt(&bindings);
        if let Some(t) = t_bind {
            self.cost.add(8, t.elapsed().as_nanos() as u64);
        }
        st.watch = crate::diag("rename");
        st.conflict_wait = crate::knob("conflictwait");
        st.force = force & 0b11111;
        let read = self.reader();
        let f = st.flush(
            fence,
            &bound,
            &|ranges| blocked_flags(ranges, others(detail)),
            &|| copy_writes(detail),
            &read,
        );
        if st.watch {
            self.cost.add(1, f.judge_ns);
            if f.capture_ns != 0 {
                self.cost.add(2, f.capture_ns);
            }
        }
        let mut forced = f.forced;
        if force >> 5 & 1 != 0 && self.other_changed(detail, &read) {
            forced |= 1 << 5;
        }
        if force >> 6 & 1 != 0 {
            forced |= 1 << 6;
        }
        self.watching
            .store(if f.watching { fence } else { 0 }, Ordering::Release);
        self.honest.store(
            if f.sampling || f.conflict || forced != 0 {
                fence
            } else {
                0
            },
            Ordering::Release,
        );
        self.honest_why.store(
            if f.checking {
                2
            } else if f.sampling {
                1
            } else if f.conflict {
                3
            } else {
                4
            },
            Ordering::Relaxed,
        );
        self.forced_mask.store(u64::from(forced), Ordering::Relaxed);
        if f.news.iter().any(|n| n.2 == Class::Rename) {
            self.young_until.store(fence + 20, Ordering::Relaxed);
        }
        if st.watch && st.frame.is_multiple_of(120) {
            let mut v: Vec<(&u64, &Inflight)> =
                st.inflight.iter().filter(|(_, e)| e.hit > 0).collect();
            v.sort_by_key(|(a, e)| (std::cmp::Reverse(e.hit), **a));
            for (addr, e) in v.iter().take(40) {
                eprintln!(
                    "DIAG rename cpu-in-flight addr {addr:#x} size {} class {}{} words {} in {} of {} judged frames",
                    e.size,
                    e.class,
                    if e.ro { " (read-only)" } else { "" },
                    e.words,
                    e.hit,
                    e.frames
                );
            }
        }
        if st.watch && st.frame.is_multiple_of(120) {
            let mut conflicts: Vec<(&u64, &Inflight)> = st
                .inflight
                .iter()
                .filter(|(_, e)| !e.ro && e.hit >= 2)
                .collect();
            conflicts.sort_by_key(|(a, e)| (std::cmp::Reverse(e.words), **a));
            for (addr, e) in conflicts.iter().take(40) {
                eprintln!(
                    "DIAG rename conflict addr {addr:#x} size {} words {} frames {} of {} judged",
                    e.size, e.words, e.hit, e.frames
                );
            }
            eprintln!(
                "DIAG rename cost ms per call (mean/max (calls)): {}; period {}",
                self.cost.take(),
                st.period()
            );
            let [l, r, n, i, wa, sn, mg, cf] = st.classes();
            let w: Vec<u64> = self
                .waits
                .iter()
                .map(|w| w.swap(0, Ordering::Relaxed))
                .collect();
            let forced: Vec<String> = FORCE_KNOBS
                .iter()
                .zip(&self.forced)
                .filter_map(|(n, c)| {
                    let v = c.swap(0, Ordering::Relaxed);
                    (v != 0).then(|| format!("{n} {v}"))
                })
                .collect();
            if !forced.is_empty() {
                eprintln!("DIAG rename waits forced by knobs: {}", forced.join(", "));
            }
            eprintln!(
                "DIAG rename waits honest {} (learning range {}, re-check {}, conflict {}, other {}) overlapped {}; learning {l} candidates {} (rename {r} never {n} idle {i} watch {wa} snapshot {sn} merge {mg} conflict {cf})",
                w[0] + w[1] + w[2] + w[4],
                w[0],
                w[1],
                w[4],
                w[2],
                w[3],
                st.cands.len()
            );
        }
        if f.sampling {
            self.active.store(true, Ordering::Release);
        }
        for (addr, size, class, frames) in &f.news {
            if !matches!(class, Class::Learning | Class::Idle | Class::Watch) {
                eprintln!(
                    "DIAG overlap rename {addr:#x} {size} {} after {frames} frames",
                    class.name()
                );
            }
        }
        if f.excluded != 0 {
            note_n(Reason::OverlapRenameExcluded, f.excluded as u64);
        }
        if f.over_budget != 0 {
            note_n(Reason::OverlapSnapshotOverBudget, f.over_budget as u64);
        }
        if f.snapshot_bytes != 0 {
            note_n(Reason::OverlapSnapshotBytes, f.snapshot_bytes);
        }
        if f.renamed.is_empty() {
            return (None, Some(check));
        }
        let sampled = crate::diag("rename")
            && (fence.is_multiple_of(60) || fence < self.young_until.load(Ordering::Relaxed));
        let capture = Capture {
            fence,
            sampled,
            epoch: self.wait_epoch.load(Ordering::Acquire),
            ranges: f
                .renamed
                .into_iter()
                .map(|x| CapRange {
                    addr: x.addr,
                    len: x.len,
                    start: x.start,
                    kind: x.kind,
                    cmask: x.cmask,
                    gmask: x.gmask,
                    rmask: x.rmask,
                    cvals: x.cvals,
                    cprefix: x.cprefix,
                })
                .collect(),
        };
        (Some(capture), Some(check))
    }

    /// The encode of a frame in batch slot `region`, after the batch ring waited for the slot. Writes back the frame that
    /// the slot held before, puts the bytes of the capture into the slot's buffer and returns where the bindings read
    /// them, and the buffer, which the batch must keep alive. A range that lies in a pool that raw global writes
    /// touched (`learned`) is not renamed.
    pub(crate) fn begin_slot(
        &self,
        region: usize,
        capture: Option<&Arc<Capture>>,
        learned: &[(u64, u64)],
    ) -> Slotted {
        self.timed(4, || self.begin_slot_inner(region, capture, learned))
    }

    fn begin_slot_inner(
        &self,
        region: usize,
        capture: Option<&Arc<Capture>>,
        learned: &[(u64, u64)],
    ) -> Slotted {
        let none = || Slotted {
            map: RenameMap::new(),
            buf: None,
            merges: Vec::new(),
        };
        let off = crate::knob("renameoff");
        let mut slots = lock(&self.slots);
        let Some(slot) = slots.get_mut(region) else {
            return none();
        };
        if let Some(p) = slot.pending.take() {
            self.write_back(&p, format_args!("begin_slot {region}"));
            self.report_probes(&p);
        }
        let Some(cap) = capture.filter(|_| !off) else {
            return none();
        };
        let mut left = 0;
        let mut offsets = Vec::new();
        let mut total = 0u64;
        for (i, r) in cap.ranges.iter().enumerate() {
            let end = r.addr + u64::from(r.len);
            if learned.iter().any(|&l| overlaps((r.addr, end), l)) {
                // A snapshot or merge range that raw global stores can reach is state.
                if r.kind != Kind::Rename {
                    lock(&self.state).demote(r.addr);
                }
                left += 1;
                continue;
            }
            // A snapshot or merge range that was found written since the flush (a shader of this frame writes it, or the
            // masks overlap now) is bound as before.
            if matches!(r.kind, Kind::Snapshot | Kind::Merge)
                && !lock(&self.state).has_kind(r.addr, r.kind)
            {
                left += 1;
                continue;
            }
            offsets.push((i, total));
            let size = u64::from(r.len).next_multiple_of(ALIGN);
            // A probe has a second copy, which keeps the state at the start of the frame.
            total += if r.kind == Kind::Probe {
                2 * size
            } else {
                size
            };
        }
        if left != 0 {
            note_n(Reason::OverlapRenameExcluded, left);
        }
        if offsets.is_empty() {
            return none();
        }
        if slot.buf.as_ref().is_none_or(|b| b.len() < total) {
            match DataBuffer::new(&self.ctx, total.next_power_of_two().max(4096)) {
                Ok(b) => slot.buf = Some(Arc::new(b)),
                Err(_) => return none(),
            }
        }
        let Some(buf) = slot.buf.clone() else {
            return none();
        };
        let mut entries = Vec::with_capacity(offsets.len());
        let mut merges = Vec::new();
        let mut probes = Vec::new();
        for &(i, off) in &offsets {
            let r = &cap.ranges[i];
            // Merge ranges and probes are filled on the GPU; the others take their bytes now.
            if !r.start.is_empty() && buf.write(off, &r.start).is_err() {
                return none();
            }
            let real = buf.gpu_address() + off;
            let size = r.len;
            entries.push((r.addr, size, real));
            match r.kind {
                Kind::Merge => merges.push((i, r.addr, size, real, 0)),
                Kind::Probe => {
                    let second = off + u64::from(size).next_multiple_of(ALIGN);
                    probes.push((i, off, second));
                    merges.push((i, r.addr, size, real, buf.gpu_address() + second));
                }
                _ => {}
            }
        }
        // The ranges whose words the host writes back in the wait: a renamed range, and the words that the CPU and the GPU
        // both write of a merge range.
        let offsets: Vec<(usize, u64)> = offsets
            .into_iter()
            .filter(|&(i, _)| {
                let r = &cap.ranges[i];
                r.kind == Kind::Rename || (r.kind == Kind::Merge && !is_empty_mask(&r.rmask))
            })
            .collect();
        let merge_count = merges.iter().filter(|m| m.4 == 0).count();
        note_n(Reason::OverlapRenamed, offsets.len() as u64);
        if merge_count != 0 {
            note_n(Reason::OverlapMergeRanges, merge_count as u64);
        }
        if cap.sampled {
            for &(i, _) in &offsets {
                let r = &cap.ranges[i];
                eprintln!(
                    "DIAG rename capture fence {} addr {:#x} size {} slot {region} {:?} words {}",
                    cap.fence,
                    r.addr,
                    r.len,
                    r.kind,
                    if r.kind == Kind::Rename {
                        nonzero_words(&r.start)
                    } else {
                        format!("{} of the CPU", r.cvals.len())
                    }
                );
            }
        }
        if !offsets.is_empty() || !probes.is_empty() {
            slot.pending = Some(Pending {
                fence: cap.fence,
                buf: buf.clone(),
                cap: cap.clone(),
                offsets,
                probes,
            });
            self.active.store(true, Ordering::Release);
        }
        Slotted {
            map: RenameMap::from_entries(entries),
            buf: Some(buf),
            merges: merges
                .into_iter()
                .map(|(idx, addr, size, slot_real, copy2)| {
                    (cap.clone(), idx, addr, size, slot_real, copy2)
                })
                .collect(),
        }
    }

    /// A CPU wait for fence `waited` returned. The GPU is done with the frames up to `done`, and `newest` is the newest
    /// flushed fence. Writes back the renamed ranges of the frames up to `waited` and reads the `post` bytes of the ranges
    /// that are judged in the frame `newest`.
    pub(crate) fn after_wait(
        &self,
        waited: u64,
        done: u64,
        newest: u64,
        who: std::fmt::Arguments<'_>,
    ) {
        self.timed(6, || self.after_wait_inner(waited, done, newest, who));
    }

    fn after_wait_inner(&self, waited: u64, done: u64, newest: u64, who: std::fmt::Arguments<'_>) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        {
            // In the order of the frames: a sum that no CPU wait separates carries from one frame to the next.
            let mut slots = lock(&self.slots);
            let mut due: Vec<Pending> = slots
                .iter_mut()
                .filter(|s| s.pending.as_ref().is_some_and(|p| p.fence <= waited))
                .filter_map(|s| s.pending.take())
                .collect();
            due.sort_unstable_by_key(|p| p.fence);
            for p in &due {
                self.write_back(p, format_args!("{who}"));
                self.report_probes(p);
            }
        }
        let mut st = lock(&self.state);
        st.waited(done, newest, &self.reader());
        let waiting = st.waiting();
        drop(st);
        let pending = lock(&self.slots).iter().any(|s| s.pending.is_some());
        self.active.store(waiting || pending, Ordering::Release);
    }

    /// The GPU is done with a frame: tells the learner what the probes of the frame saw.
    fn report_probes(&self, p: &Pending) {
        let mut seen = Vec::new();
        for &(i, off, second) in &p.probes {
            let r = &p.cap.ranges[i];
            let n = r.len as usize;
            let (mut end, mut start) = (vec![0u8; n], vec![0u8; n]);
            if p.buf.read(off, &mut end).is_ok() && p.buf.read(second, &mut start).is_ok() {
                seen.push((r.addr, end, start));
            }
        }
        if !seen.is_empty() {
            let mut st = lock(&self.state);
            for (addr, end, start) in seen {
                st.probed(p.cap.fence, addr, &end, &start);
            }
        }
    }

    /// Writes the changed words of the ranges of a frame into guest memory, each only if the guest word still holds the
    /// word that the frame started with.
    fn write_back(&self, p: &Pending, who: std::fmt::Arguments<'_>) {
        if crate::knob("renamenowb") {
            return;
        }
        let base = self.guest.address_map().gpu_base();
        let (mut written, mut skipped) = (0, 0);
        for &(i, off) in &p.offsets {
            let r = &p.cap.ranges[i];
            let mut end = vec![0u8; r.len as usize];
            if p.buf.read(off, &mut end).is_err() || r.addr < base {
                continue;
            }
            let at = r.addr - base;
            let mut shown = 0;
            let words = r.len as usize / 4;
            let mut chains = lock(&self.chains);
            let prev = chains.remove(&r.addr);
            let mut wrote: Vec<Option<u32>> = vec![None; words];
            let (mut w, mut sk) = (0, 0);
            for (i, cur, new) in written_words(r, &end) {
                let word = at + 4 * i as u64;
                let guest = self.guest.load_u32(word).unwrap_or(0);
                let before = prev
                    .as_ref()
                    .and_then(|c| c.wrote.get(i).copied().flatten());
                let same = prev.as_ref().is_some_and(|c| c.epoch == p.cap.epoch);
                let swap = decide_word(guest, cur, new, before, same);
                let done = swap.is_some_and(|v| {
                    self.guest
                        .compare_exchange_u32(word, guest, v)
                        .unwrap_or(false)
                });
                if done {
                    wrote[i] = swap;
                    w += 1;
                } else {
                    sk += 1;
                }
                if p.cap.sampled && shown < 24 {
                    shown += 1;
                    let what = if done {
                        if guest == cur {
                            "swapped"
                        } else if same {
                            "accumulated"
                        } else {
                            "replaced"
                        }
                    } else if guest == new {
                        "unchanged"
                    } else {
                        "skipped"
                    };
                    eprintln!(
                        "DIAG rename wb fence {} addr {:#x} word {i} start {cur:#x} end {new:#x} guest {guest:#x} -> {what} ({}) by {who} on {}",
                        p.cap.fence,
                        r.addr,
                        swap.map_or(String::from("-"), |v| format!("{v:#x}")),
                        std::thread::current().name().unwrap_or("?")
                    );
                }
            }
            chains.insert(
                r.addr,
                Chain {
                    epoch: p.cap.epoch,
                    wrote,
                },
            );
            written += w;
            skipped += sk;
        }
        note_n(Reason::OverlapWritebackWords, written);
        note_n(Reason::OverlapWritebackSkipped, skipped);
    }
}
