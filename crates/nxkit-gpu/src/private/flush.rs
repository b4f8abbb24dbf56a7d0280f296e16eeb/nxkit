//! The flush of a frame: the stage pool, the sink of the staged pages and the mirror's side of the flush.

use super::*;

/// The flush of a frame, on the CPU thread: protects the member pages and the pages the frame may write, and counts. The
/// frame is complete when [`encoded`] reports its batch done.
///
/// With the mirror (`NXKIT_PRIVATE=1`) it also makes the pages that the frame's bindings touch members, copies the pages
/// that the CPU wrote into a staging buffer and returns the overlay of the frame: its pool snapshot resolves member
/// ranges to the mirror, and the first command buffer copies the staged pages into the mirror. `drain` waits for every
/// frame in flight; it is called when a page becomes a member that such a frame may still write in guest memory.
pub(crate) fn at_flush(
    detail: &FrameDetail,
    fence: u64,
    map: &AddressMap,
    forced: u32,
    late: bool,
    drain: &dyn Fn(),
) -> Option<Arc<GuestOverlay>> {
    if !on() || fence == 0 {
        return None;
    }
    if mirrored() {
        return flush_mirror(detail, fence, map, forced, late, drain);
    }
    let st = state();
    let t_plan = Instant::now();
    let learned = locked(&st.learned).clone();
    let (ranges, clipped) = {
        let rw = locked(&st.rw);
        plan_written(detail, &learned, &rw, map)
    };
    let plan_ns = t_plan.elapsed().as_nanos() as u64;
    let f = Pages::global().flush(fence, &ranges);
    let n = st.frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let report = {
        let mut w = locked(&st.window);
        w.flushes += 1;
        w.written_pages += u64::from(f.written);
        w.written_runs += u64::from(f.written_runs);
        w.new_members += u64::from(f.new_members);
        w.dirtied += u64::from(f.dirtied);
        w.flush_ns += f.ns;
        w.plan_ns += plan_ns;
        w.mprotects += u64::from(f.mprotects);
        w.clipped += clipped;
        w.scanned += u64::from(f.scanned);
        n.is_multiple_of(REPORT_EVERY)
    };
    if report {
        print_report(n);
    }
    None
}

// ---------------------------------------------------------------------------------------------------------------------
// Step 2: the mirror.

/// The most staging buffers that wait for their frames.
const STAGE_BUSY_MAX: usize = 16;

impl StagePool {
    pub(super) fn reclaim(&mut self, completed: u64) {
        let mut i = 0;
        while i < self.busy.len() {
            if self.busy[i].0 <= completed {
                let (_, b) = self.busy.swap_remove(i);
                self.free.push(b);
            } else {
                i += 1;
            }
        }
        while self.busy.len() > STAGE_BUSY_MAX {
            self.busy.remove(0);
        }
        self.free.truncate(4);
    }

    /// A buffer for `pages` pages.
    pub(super) fn take(
        &mut self,
        ctx: &Context,
        pages: usize,
    ) -> Result<Arc<DataBuffer>, nxkit_metal::MetalError> {
        let need = pages.max(1) as u64 * PAGE;
        if let Some(i) = self.free.iter().position(|b| b.len() >= need) {
            return Ok(self.free.swap_remove(i));
        }
        // None fits: the smaller ones go, a larger one is made.
        self.free.clear();
        let pages = (pages.max(256) as u64).next_power_of_two();
        DataBuffer::new_unfilled(ctx, pages * PAGE).map(Arc::new)
    }
}

impl<'a> Sink<'a> {
    pub(super) fn new(ctx: &'a Context, pool: &'a mut StagePool) -> Self {
        Self {
            ctx,
            pool,
            buf: None,
            cap: 0,
            slots: Vec::new(),
            index: FastMap::default(),
            skips: FastMap::default(),
            failed: false,
        }
    }

    /// What the sink took: the buffer, the slots, the masks sorted by page, and whether it failed.
    #[allow(clippy::type_complexity)]
    pub(super) fn finish(
        &mut self,
    ) -> (
        Option<Arc<DataBuffer>>,
        Vec<(u32, u32)>,
        Vec<(u32, [u64; 4])>,
        bool,
    ) {
        let mut skips: Vec<(u32, [u64; 4])> = self.skips.drain().collect();
        skips.sort_unstable_by_key(|s| s.0);
        (
            self.buf.take(),
            std::mem::take(&mut self.slots),
            skips,
            self.failed,
        )
    }

    fn grow(&mut self) {
        let Some(old) = self.buf.take() else {
            self.failed = true;
            return;
        };
        let used = self.slots.len() as u64 * PAGE;
        let bigger = DataBuffer::new_unfilled(self.ctx, old.len() * 2).map(Arc::new);
        let moved = bigger.and_then(|b| {
            let mut tmp = vec![0u8; used as usize];
            old.read(0, &mut tmp)?;
            b.write(0, &tmp)?;
            Ok(b)
        });
        match moved {
            Ok(b) => {
                self.cap = (b.len() / PAGE) as usize;
                self.buf = Some(b);
                self.pool.free.push(old);
            }
            Err(_) => {
                self.buf = Some(old);
                self.failed = true;
            }
        }
    }
}

impl Stager for Sink<'_> {
    fn begin(&mut self, max_pages: usize) {
        match self.pool.take(self.ctx, max_pages) {
            Ok(b) => {
                self.cap = (b.len() / PAGE) as usize;
                self.buf = Some(b);
            }
            Err(_) => self.failed = true,
        }
    }

    fn stage(&mut self, page: u32, bytes: &[u8]) {
        self.stage_masked(page, bytes, [0; 4]);
    }

    fn stage_masked(&mut self, page: u32, bytes: &[u8], skip: [u64; 4]) {
        if self.failed {
            return;
        }
        if skip.iter().any(|w| *w != 0) {
            self.skips.insert(page, skip);
        } else {
            self.skips.remove(&page);
        }
        let slot = match self.index.get(&page) {
            Some(&s) => s,
            None => {
                if self.slots.len() >= self.cap {
                    self.grow();
                    if self.failed {
                        return;
                    }
                }
                let s = self.slots.len() as u32;
                self.slots.push((page, s));
                self.index.insert(page, s);
                s
            }
        };
        if let Some(b) = &self.buf
            && b.write(u64::from(slot) * PAGE, bytes).is_err()
        {
            self.failed = true;
        }
    }
}

fn flush_mirror(
    detail: &FrameDetail,
    fence: u64,
    map: &AddressMap,
    forced: u32,
    late: bool,
    drain: &dyn Fn(),
) -> Option<Arc<GuestOverlay>> {
    let st = state();
    let pages = Pages::global();
    let t_all = Instant::now();
    let t_plan = Instant::now();
    let learned = locked(&st.learned).clone();
    let (written, clipped) = {
        let rw = locked(&st.rw);
        plan_written(detail, &learned, &rw, map)
    };
    let t_written_ns = t_plan.elapsed().as_nanos() as u64;
    // The members after this flush: the tracked pages, the written ranges and what the bindings touch.
    let (bits, _) = pages.member_snapshot();
    let mut members = Members {
        bits: &bits,
        added: PageRuns::default(),
    };
    let written_runs: Vec<(u32, u32)> = written
        .iter()
        .filter_map(|&(h, l)| pages.page_range(h, l))
        .collect();
    members.added.add(&written_runs);
    let t_r = Instant::now();
    let mut reads = read_ranges(detail);
    // Ranges that the encoder found split between members and non-members in an earlier frame and that no binding of
    // this one announces (texture upload sources, ranges of recordings): they become whole too.
    let learned_partial = crate::overlay::partial_ranges();
    reads.extend(learned_partial.iter().map(|&(a, l)| (a, l, BIND_COPY)));
    let t_reads_ns = t_r.elapsed().as_nanos() as u64;
    let t_c = Instant::now();
    let closure_pages = closure(pages, &reads, map, &mut members);
    let t_closure_ns = t_c.elapsed().as_nanos() as u64;
    let psize = pages.page_size();
    let host_of = |p: u32| pages.host_base() + u64::from(p) * psize;
    let member_ranges: Vec<(u64, u64)> = members
        .added
        .0
        .iter()
        .map(|&(s, e)| (host_of(s), u64::from(e - s) * psize))
        .collect();
    let plan_ns = t_plan.elapsed().as_nanos() as u64;
    let mut guard = locked(&st.mirror);
    let ms = guard.as_mut()?;
    let t_e = Instant::now();
    // The segments of the mirror that hold them.
    let host0 = map.host_base();
    let offsets: Vec<(u64, u64)> = member_ranges
        .iter()
        .map(|&(h, l)| (h - host0, h - host0 + l))
        .collect();
    if let Err(e) = ms.mirror.ensure(&offsets) {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| eprintln!("nxkit-gpu: NXKIT_PRIVATE=1: the mirror cannot grow: {e}"));
        // From here on guest memory is the truth again: the page tracker stops copying from the mirror.
        pages.set_mirror(0);
        FAILED.store(true, Ordering::Relaxed);
        return None;
    }
    let t_ensure_ns = t_e.elapsed().as_nanos() as u64;
    // A page that becomes a member in a pool that raw stores wrote may still be written in guest memory by a frame in
    // flight: wait for those frames once, before the page is copied.
    let enters_learned = members.added.0.iter().any(|&(s, e)| {
        (s..e).any(|p| {
            let wasn_t = members
                .bits
                .get(p as usize / 64)
                .is_none_or(|w| w >> (p % 64) & 1 == 0);
            wasn_t
                && learned.iter().any(|&(lo, hi)| {
                    map.host_address(lo, hi - lo)
                        .ok()
                        .and_then(|h| pages.page_range(h, hi - lo))
                        .is_some_and(|(a, b)| a <= p && p < b)
                })
        })
    });
    let mut enter_wait = false;
    if enters_learned && pages.completed() + 1 < fence {
        enter_wait = true;
        // The mirror lock is not held across the wait: the encoder takes it to read the runs.
        drop(guard);
        drain();
        guard = locked(&st.mirror);
    }
    let ms = guard.as_mut()?;
    // A buffer is free again once the completion of its frame has finished its copies (it reads the late copies).
    ms.pool.reclaim(pages.settled());
    let poison = poison_on().then_some(POISON);
    let mut sink = Sink::new(&ms.ctx, &mut ms.pool);
    let f = pages.flush_mirrored(fence, &member_ranges, &written, poison, &mut sink);
    let (buf, slots, skips, failed) = sink.finish();
    drop(sink);
    if failed {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| eprintln!("nxkit-gpu: NXKIT_PRIVATE=1: the staging buffer failed; pages the CPU wrote are lost to the GPU"));
    }
    let staged = match buf {
        Some(b) if !slots.is_empty() && !failed => {
            ms.pool.busy.push((fence, b.clone()));
            let mut slots = slots;
            slots.sort_unstable();
            Some(Staged {
                buf: b,
                slots,
                skips,
            })
        }
        Some(b) => {
            ms.pool.free.push(b);
            None
        }
        None => None,
    };
    // Results one frame late: a buffer for the GPU's copies of the late pages at the start and the end of the frame.
    let late_pages = if pages.late() {
        pages.late_copy_pages(fence)
    } else {
        Vec::new()
    };
    let late_copies = if late_pages.is_empty() {
        None
    } else {
        match ms.pool.take(&ms.ctx, 2 * late_pages.len()) {
            Ok(b) => {
                ms.pool.busy.push((fence, b.clone()));
                Some(LateCopies {
                    buf: b,
                    pages: late_pages,
                })
            }
            Err(_) => None,
        }
    };
    let mut diag_sets: Option<DiagSets> = None;
    if crate::diag("private") {
        // What the frame binds, to sort the writes of the CPU between its flush and its commit.
        let r: Vec<(u64, u64, u8)> = reads
            .iter()
            .filter_map(|&(a, len, kind)| {
                let len = len.min(RANGE_MAX_BYTES);
                let h = map.host_address(a, len).ok()?;
                Some((h, h + len, kind))
            })
            .collect();
        let mut sources: Vec<(u64, u64, u8)> = {
            let rw = locked(&st.rw);
            written_ranges(detail, &[], &rw)
                .iter()
                .filter_map(|&(a, len)| {
                    let h = map.host_address(a, len.max(1)).ok()?;
                    Some((h, h + len.max(1), 0))
                })
                .collect()
        };
        sources.extend(learned.iter().filter_map(|&(lo, hi)| {
            let h = map.host_address(lo, hi - lo).ok()?;
            Some((h, h + (hi - lo), 1))
        }));
        diag_sets = Some((
            r,
            written.iter().map(|&(h, l)| (h, h + l)).collect(),
            sources,
        ));
    }
    let (bits, member_gen) = pages.member_snapshot();
    let runs: Vec<Run<Arc<GuestMemory>>> = ms
        .mirror
        .runs()
        .iter()
        .map(|r| Run {
            va_start: ms.va_base + r.start,
            va_end: ms.va_base + r.end,
            mem: r.mem.clone(),
        })
        .collect();
    let generation = member_gen << 24 | (ms.mirror.generation() & 0xff_ffff);
    ms.last_fence = fence;
    // The knobs that bind a class of resolves to guest memory, to find which class a defect comes from.
    let off = [
        ("privnoubo", crate::overlay::UBO),
        ("privnosbuf", crate::overlay::SBUF),
        ("privnostream", crate::overlay::STREAM),
        ("privnoupload", crate::overlay::UPLOAD),
        ("privnogmem", crate::overlay::GMEM),
        ("privnocopy", crate::overlay::COPY),
    ]
    .iter()
    .filter(|(k, _)| crate::knob(k))
    .fold(0u8, |a, (_, c)| a | c);
    // With a class bound to guest memory, guest memory must be current: every page a frame wrote is copied back when the
    // frame completes.
    pages.set_eager_sync(off != 0);
    let overlay = Overlay::new(
        ms.va_base,
        bits,
        runs,
        Some(ms.mirror.view()),
        staged,
        generation,
    )
    .with_off(off)
    .with_frame(fence, late)
    .with_late(late_copies);
    drop(guard);
    let n = st.frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let report = {
        let mut w = locked(&st.window);
        w.flushes += 1;
        w.written_pages += u64::from(f.written);
        w.written_runs += u64::from(f.written_runs);
        w.new_members += u64::from(f.new_members);
        w.dirtied += u64::from(f.dirtied);
        w.flush_ns += f.ns;
        w.plan_ns += plan_ns;
        w.mprotects += u64::from(f.mprotects);
        w.clipped += clipped;
        w.scanned += u64::from(f.scanned);
        w.staged += u64::from(f.staged);
        w.raced += u64::from(f.raced);
        w.closure_pages += closure_pages;
        w.enter_waits += u64::from(enter_wait);
        w.t_plan_written += t_written_ns;
        w.t_reads += t_reads_ns;
        w.t_closure += t_closure_ns;
        w.t_ensure += t_ensure_ns;
        w.t_pages += f.ns;
        let total = t_all.elapsed().as_nanos() as u64;
        w.t_total += total;
        w.t_rest +=
            total.saturating_sub(t_written_ns + t_reads_ns + t_closure_ns + t_ensure_ns + f.ns);
        w.t_new += f.new_ns;
        w.t_log += f.log_ns;
        w.t_stage += f.stage_ns;
        w.t_hot += f.hot_ns;
        w.t_tag += f.tag_ns;
        w.t_prot += f.mprotect_ns;
        w.hot_pages += u64::from(f.hot);
        w.hot_staged += u64::from(f.hot_staged);
        w.hot_same += u64::from(f.hot_same);
        w.promoted += u64::from(f.promoted);
        w.demoted += u64::from(f.demoted);
        n.is_multiple_of(REPORT_EVERY)
    };
    if report {
        print_report(n);
    }
    let overlay = Arc::new(overlay);
    if let Some((reads, written, sources)) = diag_sets {
        let mut q = locked(&st.sets);
        q.push_back(Arc::new(FrameSets {
            fence,
            reads,
            written,
            sources,
            forced,
            view: overlay.clone(),
        }));
        while q.len() > 12 {
            q.pop_front();
        }
    }
    Some(overlay)
}
