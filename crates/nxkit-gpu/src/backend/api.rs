//! The `Backend` trait of [`GpuBackend`]: what the front end calls for queues, fences, memory and presents.

use super::poolops::PoolOp;
use super::*;

impl Backend for GpuBackend {
    fn gpu_base(&self) -> u64 {
        self.guest.address_map().gpu_base()
    }

    fn host_range(&self) -> Option<(u64, u64)> {
        Some((self.guest.host_base(), self.guest.len()))
    }

    fn register_pool(&self, pool: PoolInfo) -> BackendResult<()> {
        let map = self.guest.address_map();
        let gpu_addr = map.gpu_address(pool.host, pool.len).map_err(fail)?;
        // The check of `Gpu::register_pool`, made here so the error comes back now and the queued op cannot fail.
        map.host_address(gpu_addr, pool.len).map_err(fail)?;
        // A pool of no bytes holds no address. The encoder still hears of it, as before.
        if pool.len > 0 {
            self.pools
                .register(gpu_addr, pool.len, pool.flags)
                .map_err(|e| fail(format_args!("bad pool range {:#x}+{:#x}", e.start, e.len)))?;
        }
        self.pool_ops.run(
            &self.gpu,
            PoolOp::Register {
                addr: gpu_addr,
                len: pool.len,
                flags: pool.flags,
            },
        );
        Ok(())
    }

    fn unregister_pool(&self, host: u64) {
        if let Ok(gpu_addr) = self.guest.address_map().gpu_address(host, 1) {
            self.pools.unregister(gpu_addr);
            self.pool_ops
                .run(&self.gpu, PoolOp::Unregister { addr: gpu_addr });
        }
    }

    fn map_pool_window(&self, window: PoolInfo) {
        if let Ok(gpu_addr) = self
            .guest
            .address_map()
            .gpu_address(window.host, window.len)
        {
            self.pool_ops.run(
                &self.gpu,
                PoolOp::Map {
                    addr: gpu_addr,
                    len: window.len,
                    flags: window.flags,
                },
            );
        }
    }

    fn unmap_pool_window(&self, host: u64, len: u64) {
        if let Ok(gpu_addr) = self.guest.address_map().gpu_address(host, len) {
            self.pool_ops.run(
                &self.gpu,
                PoolOp::Unmap {
                    addr: gpu_addr,
                    len,
                },
            );
        }
    }

    fn device_modes(&self, origin_lower_left: bool, _depth_zero_to_one: bool) {
        locked(&self.walker).device_origin(origin_lower_left);
    }

    fn submit<'a>(
        &'a self,
        queue: u64,
        handles: &'a [CommandHandle],
        origin_lower_left: bool,
    ) -> Option<Deferred<'a>> {
        Some(Box::new(move || {
            let mut walker = locked(&self.walker);
            walker.queue_origin(queue, origin_lower_left);
            let mut mem = GuestView(self.guest.clone());
            walker.submit(&mut mem, queue, handles);
        }))
    }

    fn flush_frame(
        &self,
        frame: &Frame,
        plan: &PassPlan,
        _reason: FlushReason,
        fence: Fence,
        cx: FlushCx<'_>,
    ) -> BackendResult<()> {
        self.flushed.fetch_max(fence.0, Ordering::AcqRel);
        let mut detail = {
            let mut walker = locked(&self.walker);
            let counters = walker.take_counters();
            if stats_diag() && self.reports.load(Ordering::Relaxed).is_multiple_of(30) {
                eprintln!("nxkit-gpu: walk: {counters:?}");
            }
            walker.take()
        };
        if overlap_diag() {
            self.overlap.frame(&detail, fence.0, &self.guest);
        }
        if !matches(frame, &detail) {
            eprintln!(
                "nxkit-gpu: the walked detail does not match the frame, its draws are skipped"
            );
            // The event signals stay: a game that polls a counter would wait for ever without them.
            let reports = std::mem::take(&mut detail.reports);
            detail = FrameDetail {
                reports: reports.into_iter().filter(|r| r.is_event()).collect(),
                ..FrameDetail::default()
            };
        }
        // The overlap: the ranges that this frame reads from a copy of its own take their bytes now, before the CPU
        // writes the next frame.
        if crate::overlap::on() {
            let (capture, check) = self.rename.at_flush(&detail, fence.0);
            detail.rename = capture.map(Arc::new);
            detail.rename_check = check;
        }
        // `NXKIT_PRIVATE`: protects the pages the frame may write and the pages the CPU wrote, and counts. With the mirror
        // it also stages the pages the CPU wrote and gives the view that resolves member ranges to the mirror.
        // Submits that the flush ran before the CPU signals they wait for read what the CPU writes until the signals: the
        // capture of this frame stays open for them (`private::late_stage`).
        let waits = std::mem::take(&mut *locked(&self.forced));
        let pending = if crate::private::late_enabled() {
            self.unsatisfied(&waits)
        } else {
            Vec::new()
        };
        if !pending.is_empty() {
            locked(&self.forced_pending).extend(pending.iter().map(|w| (fence.0, *w)));
            crate::private::late_expect(fence.0, pending.len() as u32);
        }
        let overlay = crate::private::at_flush(
            &detail,
            fence.0,
            &self.guest.address_map(),
            waits.len() as u32,
            !pending.is_empty(),
            &|| self.wait_idle(),
        );
        self.captured.fetch_max(fence.0, Ordering::AcqRel);
        // The snapshot of the flush: a pool the game finalizes after this point still serves the frame.
        let pools = match overlay {
            Some(o) => Arc::new(self.pools.current().with_overlay(o)),
            None => self.pools.current(),
        };
        if let Some(enc) = &self.enc {
            let settled = enc.settle();
            if let Some(update) = &settled.update {
                update.apply(cx.table);
            }
            if !enc.is_dead() {
                let snapshot = Snapshot::capture(
                    frame,
                    plan,
                    &detail,
                    cx.table,
                    cx.descriptors,
                    &self.pool_hint.ids(),
                    self.pool_hint.sampler_epoch(),
                );
                let job = Box::new(Job {
                    frame: frame.clone(),
                    plan: plan.clone(),
                    detail,
                    snapshot,
                    pools,
                    fence,
                });
                if !enc.submit(job) {
                    return Err(BackendError("the encoder thread is gone".into()));
                }
                return settled.error.map_or(Ok(()), |e| Err(BackendError(e)));
            }
        }
        let r = self.flush_sync(frame, plan, &pools, fence, &detail, cx);
        drop_later(detail);
        r
    }

    fn forced_waits(&self, _fence: Fence, waits: &[nxkit_nvn::backend::ForcedWait]) {
        if crate::private::mirrored() {
            locked(&self.forced).extend_from_slice(waits);
        }
    }

    fn cpu_event_signalled(&self, word: u64) {
        self.events.cpu_signalled(word);
        let mut pending = locked(&self.forced_pending);
        if pending.is_empty() {
            return;
        }
        let value = self.counter_of(word);
        let mut closed: Vec<u64> = Vec::new();
        pending.retain(|(fence, w)| {
            let done = w.word == word
                && value.is_some_and(|v| nxkit_nvn::marks::event_wait_done(w.mode, v, w.value));
            if done {
                closed.push(*fence);
            }
            !done
        });
        drop(pending);
        for fence in closed {
            crate::private::late_signal(fence);
        }
    }

    fn settle(&self, table: &mut ResourceTable) {
        if let Some(update) = self.enc.as_ref().and_then(|enc| enc.settle().update) {
            update.apply(table);
        }
    }

    fn present(&self, present: Present) -> BackendResult<()> {
        encpace::asked();
        self.asked.fetch_add(1, Ordering::AcqRel);
        let origin_lower_left = locked(&self.walker).current_origin();
        match &self.enc {
            Some(enc)
                if enc.send(Msg::Present {
                    present,
                    origin_lower_left,
                }) =>
            {
                Ok(())
            }
            _ => self.present_now(present, origin_lower_left),
        }
    }

    fn wait_fence(&self, fence: Fence, _timeout_ns: u64) -> WaitResult {
        let r = self.wait_fence_now(fence);
        // A wait may return before the GPU finished the frame (`NXKIT_PRIVATE=1`, the overlap): a CPU signal after it
        // is newer than the GPU's signals of the frame (`events`).
        if matches!(r, WaitResult::Done | WaitResult::AlreadyDone) {
            self.events.waited(fence.0);
        }
        r
    }

    fn wait_idle(&self) {
        crate::private::note_full(crate::private::FullWait::Idle);
        if let Some(enc) = &self.enc {
            enc.wait_idle();
        }
        self.drain(Some(self.encoded.load(Ordering::Acquire)));
    }

    fn program_set(&self, program: u64, stages: &[StageKey]) {
        locked(&self.walker).program_set(program, stages);
    }

    fn program_released(&self, program: u64) {
        locked(&self.walker).program_released(program);
    }

    fn recording_invalidated(&self, start: u64, end: u64) {
        self.invalidations.push(start, end);
        if overlap_diag()
            && let Ok(gpu) = self
                .guest
                .address_map()
                .gpu_address(start, end.saturating_sub(start).max(1))
        {
            self.overlap.flushed(
                gpu,
                end.saturating_sub(start),
                self.done.load(Ordering::Acquire),
            );
        }
    }

    fn acquire(&self, _window: u64, _index: u32) {
        let mut pending = locked(&self.pending);
        if self.enc.is_some() {
            // With the encoder thread a frame reaches the queue only after its encode, so the queue does not see the
            // frames that wait for the encoder. Counted from the game's presents, at most MAX_PENDING frames are on
            // their way to the screen; otherwise the game ran frames ahead, each showed 80 ms after it, and the
            // textures it wrote for later frames reached the encodes of earlier ones.
            let in_flight = |_: &Vec<Pending>| {
                self.asked
                    .load(Ordering::Acquire)
                    .saturating_sub(self.shown_upto.load(Ordering::Acquire))
            };
            while in_flight(&pending) >= MAX_PENDING as u64 {
                let (guard, timeout) = self
                    .freed
                    .wait_timeout(pending, ACQUIRE_WAIT)
                    .unwrap_or_else(PoisonError::into_inner);
                pending = guard;
                if timeout.timed_out() {
                    break;
                }
            }
            return;
        }
        while pending.len() >= MAX_PENDING {
            let (guard, timeout) = self
                .freed
                .wait_timeout(pending, ACQUIRE_WAIT)
                .unwrap_or_else(PoisonError::into_inner);
            pending = guard;
            if timeout.timed_out() {
                pending.remove(0);
            }
        }
    }

    fn texture_created(&self, record: &TextureRecord) {
        self.retire(record.id);
    }

    fn texture_finalized(&self, id: TexId) {
        self.retire(id);
    }

    fn sampler_registered(&self, _id: u32, _sampler: &Tsc) {}

    fn sampler_released(&self, _id: u32) {}
}

impl GpuBackend {
    /// The wait of [`Backend::wait_fence`].
    fn wait_fence_now(&self, fence: Fence) -> WaitResult {
        if fence.0 != 0 && crate::overlap::on() {
            self.rename.note_wait();
            self.rename.before_wait(fence.0);
        }
        if fence.0 == 0 || fence.0 <= self.done.load(Ordering::Acquire) {
            if fence.0 != 0 {
                self.renamed_wait(
                    fence.0,
                    format_args!("wait done already for fence {}", fence.0),
                );
            }
            return WaitResult::AlreadyDone;
        }
        if crate::knob("nowait") {
            return WaitResult::AlreadyDone;
        }
        // `NXKIT_PRIVATE=1`: the frame's inputs are captured and its outputs are guarded, so the CPU goes on.
        if let Some(r) = self.wait_early(fence) {
            return r;
        }
        // One frame of overlap: a wait for the newest frame waits for the one before it. A frame that copied back to
        // CPU memory waited in full when it was encoded, so `done` covers it already.
        if crate::overlap::on()
            && self.enc.is_none()
            && fence.0 == self.encoded.load(Ordering::Acquire)
            && fence.0 > 1
            && !self.rename.honest(fence.0)
        {
            let older = fence.0 - 1;
            self.rename.count_wait(fence.0, true);
            if older > self.done.load(Ordering::Acquire) {
                let result = self.lock_gpu().wait_older();
                match result {
                    Ok(Some(stats)) => {
                        nxkit_nvn::screen::set_gpu_ms((stats.gpu_end_s - stats.gpu_start_s) * 1e3);
                    }
                    Ok(None) => {}
                    Err(_) => return WaitResult::Failed,
                }
                let (finished, ok) = self.finish_batches(older);
                self.write_counters(older, &finished);
                self.done.fetch_max(older, Ordering::AcqRel);
                if ok {
                    self.renamed_wait(
                        older,
                        format_args!("wait older={older} for fence {}", fence.0),
                    );
                }
            }
            return WaitResult::Done;
        }
        // The same with the encoder thread: the frame before the newest must be encoded and finished on the GPU. The
        // wait goes to the batch itself, not through the lock that the encoder holds while it encodes the newest frame.
        if crate::overlap::on()
            && let Some(enc) = &self.enc
            && fence.0 > 2
            && fence.0 == self.flushed.load(Ordering::Acquire)
            && !self.copied_recent.load(Ordering::Acquire)
            && !self.rename.honest(fence.0)
        {
            // The `overlap2` knob waits for the frame two back: the encode on its own thread makes the way from a
            // present to the GPU's end longer than one frame.
            let older = fence.0 - if crate::knob("overlap2") { 2 } else { 1 };
            self.rename.count_wait(fence.0, true);
            if older > self.done.load(Ordering::Acquire) {
                let reached = || self.encoded.load(Ordering::Acquire) >= older;
                if !enc.wait_for(reached) || enc.has_error() {
                    return WaitResult::Failed;
                }
                let (finished, ok) = self.finish_batches(older);
                if !ok {
                    return WaitResult::Failed;
                }
                if let Some((_, stats)) = finished.last() {
                    nxkit_nvn::screen::set_gpu_ms((stats.gpu_end_s - stats.gpu_start_s) * 1e3);
                }
                self.write_counters(older, &finished);
                self.done.fetch_max(older, Ordering::AcqRel);
                self.renamed_wait(
                    older,
                    format_args!("wait older={older} for fence {}", fence.0),
                );
            }
            return WaitResult::Done;
        }
        if crate::overlap::on() {
            self.rename.count_wait(fence.0, false);
        }
        if let Some(enc) = &self.enc {
            let reached = || self.encoded.load(Ordering::Acquire) >= fence.0;
            if !enc.wait_for(reached) || enc.has_error() {
                return WaitResult::Failed;
            }
        }
        if self.drain(Some(fence.0)) {
            WaitResult::Done
        } else {
            WaitResult::Failed
        }
    }

    /// The wait of `NXKIT_PRIVATE=1` that returns when the flush of `fence` has run: its inputs are in the staging buffer
    /// and its outputs are guarded, so the GPU's work on the frame has no effect on what the CPU does next. Returns None
    /// when the wait must be a full one: the knob `privhonest`, a frame that copies back to CPU memory (the copy-back is
    /// not guarded yet), or a flush that did not finish. The frames in flight are bounded: the wait for `fence` also waits
    /// until the frame [`BATCHES_IN_FLIGHT`] before it is done.
    fn wait_early(&self, fence: Fence) -> Option<WaitResult> {
        use crate::private::{FullWait, note_bounded, note_early, note_full};
        if !crate::private::early() {
            return None;
        }
        if crate::knob("privhonest") {
            note_full(FullWait::Knob);
            return None;
        }
        // The copy-back of a frame writes guest memory when the GPU is done, and the CPU reads it: the frames after a
        // copy-back keep the full wait.
        let copied = self.copied_fence.load(Ordering::Acquire);
        if self.copied_recent.load(Ordering::Acquire)
            || (copied != 0 && fence.0 <= copied + COPYBACK_SPAN)
        {
            note_full(FullWait::CopyBack);
            return None;
        }
        if fence.0 > self.captured.load(Ordering::Acquire) {
            note_full(FullWait::NotCaptured);
            return None;
        }
        let ahead = crate::private::run_ahead();
        if ahead == 0 {
            note_full(FullWait::Knob);
            return None;
        }
        if fence.0 > ahead {
            let older = fence.0 - ahead;
            if older > self.done.load(Ordering::Acquire) {
                let t0 = std::time::Instant::now();
                let ok = self.wait_done(older);
                note_bounded(t0.elapsed());
                if !ok {
                    return Some(WaitResult::Failed);
                }
            }
        }
        self.harvest();
        note_early();
        Some(WaitResult::Done)
    }

    /// Waits until the GPU is done with `older` and every frame before it, and writes the reports of those frames.
    fn wait_done(&self, older: u64) -> bool {
        if let Some(enc) = &self.enc {
            let reached = || self.encoded.load(Ordering::Acquire) >= older;
            if !enc.wait_for(reached) || enc.has_error() {
                return false;
            }
            let (finished, ok) = self.finish_batches(older);
            if !ok {
                return false;
            }
            if let Some((_, stats)) = finished.last() {
                nxkit_nvn::screen::set_gpu_ms((stats.gpu_end_s - stats.gpu_start_s) * 1e3);
            }
            self.write_counters(older, &finished);
            self.done.fetch_max(older, Ordering::AcqRel);
            return true;
        }
        let (finished, ok) = self.finish_batches(older);
        if !ok {
            return false;
        }
        if let Some((_, stats)) = finished.last() {
            nxkit_nvn::screen::set_gpu_ms((stats.gpu_end_s - stats.gpu_start_s) * 1e3);
        }
        self.write_counters(older, &finished);
        self.done.fetch_max(older, Ordering::AcqRel);
        true
    }

    /// Writes the reports of the frames that the GPU finished and that no wait has taken yet, without waiting for any.
    /// The counters of a frame are written when it is seen to be done, which is later than the end of the frame when the
    /// CPU does not wait for it.
    fn harvest(&self) {
        let upto = crate::private::completed().min(self.encoded.load(Ordering::Acquire));
        if upto <= self.done.load(Ordering::Acquire) {
            return;
        }
        let (finished, ok) = self.finish_batches(upto);
        if let Some((_, stats)) = finished.last() {
            nxkit_nvn::screen::set_gpu_ms((stats.gpu_end_s - stats.gpu_start_s) * 1e3);
        }
        self.write_counters(upto, &finished);
        if ok {
            self.done.fetch_max(upto, Ordering::AcqRel);
        }
    }
}

/// The frames after a frame that copied back keep the full wait.
const COPYBACK_SPAN: u64 = 3;

impl GpuBackend {
    /// The 32-bit counter of an event at the host address `word`.
    fn counter_of(&self, word: u64) -> Option<u32> {
        let mut b = [0u8; 4];
        nxkit_nvn::mem::GuestMem::read(&GuestView(self.guest.clone()), word, &mut b).ok()?;
        Some(u32::from_le_bytes(b))
    }

    /// The waits of `waits` that the counter of their event does not satisfy yet.
    fn unsatisfied(
        &self,
        waits: &[nxkit_nvn::backend::ForcedWait],
    ) -> Vec<nxkit_nvn::backend::ForcedWait> {
        waits
            .iter()
            .filter(|w| {
                !self
                    .counter_of(w.word)
                    .is_some_and(|v| nxkit_nvn::marks::event_wait_done(w.mode, v, w.value))
            })
            .copied()
            .collect()
    }
}
