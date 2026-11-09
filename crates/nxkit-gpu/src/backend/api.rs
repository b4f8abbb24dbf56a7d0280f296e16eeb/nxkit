//! The `Backend` trait of [`GpuBackend`]: what the front end calls for queues, fences, memory and presents.

use super::*;

impl Backend for GpuBackend {
    fn gpu_base(&self) -> u64 {
        self.guest.address_map().gpu_base()
    }

    fn host_range(&self) -> Option<(u64, u64)> {
        Some((self.guest.host_base(), self.guest.len()))
    }

    fn register_pool(&self, pool: PoolInfo) -> BackendResult<()> {
        let gpu_addr = self
            .guest
            .address_map()
            .gpu_address(pool.host, pool.len)
            .map_err(fail)?;
        // A pool of no bytes holds no address. The encoder still hears of it, as before.
        let id = (pool.len > 0)
            .then(|| self.pools.register(gpu_addr, pool.len, pool.flags))
            .transpose()
            .map_err(|e| fail(format_args!("bad pool range {:#x}+{:#x}", e.start, e.len)))?;
        let r = locked(&self.gpu)
            .register_memory_pool(gpu_addr, pool.len, pool.flags)
            .map_err(fail);
        if let (Err(_), Some(id)) = (&r, id) {
            self.pools.unregister_id(id);
        }
        r
    }

    fn unregister_pool(&self, host: u64) {
        if let Ok(gpu_addr) = self.guest.address_map().gpu_address(host, 1) {
            self.pools.unregister(gpu_addr);
            locked(&self.gpu).unregister_memory_pool(gpu_addr);
        }
    }

    fn submit<'a>(
        &'a self,
        queue: u64,
        handles: &'a [CommandHandle],
        _origin_lower_left: bool,
    ) -> Option<Deferred<'a>> {
        Some(Box::new(move || {
            let mut walker = locked(&self.walker);
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
            detail = FrameDetail::default();
        }
        // The snapshot of the flush: a pool the game finalizes after this point still serves the frame.
        let pools = self.pools.current();
        if let Some(enc) = &self.enc {
            let settled = enc.settle();
            if let Some(update) = &settled.update {
                update.apply(cx.table);
            }
            if !enc.is_dead() {
                let snapshot = Snapshot::capture(frame, plan, &detail, cx.table, cx.descriptors);
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

    fn settle(&self, table: &mut ResourceTable) {
        if let Some(update) = self.enc.as_ref().and_then(|enc| enc.settle().update) {
            update.apply(table);
        }
    }

    fn present(&self, present: Present) -> BackendResult<()> {
        encpace::asked();
        self.asked.fetch_add(1, Ordering::AcqRel);
        match &self.enc {
            Some(enc) if enc.send(Msg::Present(present)) => Ok(()),
            _ => self.present_now(present),
        }
    }

    fn wait_fence(&self, fence: Fence, _timeout_ns: u64) -> WaitResult {
        if fence.0 == 0 || fence.0 <= self.done.load(Ordering::Acquire) {
            return WaitResult::AlreadyDone;
        }
        if crate::knob("nowait") {
            return WaitResult::AlreadyDone;
        }
        // One frame of overlap: a wait for the newest frame waits for the one before it. A frame that copied back to
        // CPU memory waited in full when it was encoded, so `done` covers it already.
        if crate::overlap::on()
            && self.enc.is_none()
            && fence.0 == self.encoded.load(Ordering::Acquire)
            && fence.0 > 1
        {
            let older = fence.0 - 1;
            if older > self.done.load(Ordering::Acquire) {
                let result = locked(&self.gpu).wait_older();
                match result {
                    Ok(Some(stats)) => {
                        *locked(&self.last_gpu) = Some((stats.gpu_start_s, stats.gpu_end_s));
                        nxkit_nvn::screen::set_gpu_ms((stats.gpu_end_s - stats.gpu_start_s) * 1e3);
                    }
                    Ok(None) => {}
                    Err(_) => return WaitResult::Failed,
                }
                self.write_counters(older);
                self.done.fetch_max(older, Ordering::AcqRel);
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
        {
            // The `overlap2` knob waits for the frame two back: the encode on its own thread makes the way from a
            // present to the GPU's end longer than one frame.
            let older = fence.0 - if crate::knob("overlap2") { 2 } else { 1 };
            if older > self.done.load(Ordering::Acquire) {
                let reached = || self.encoded.load(Ordering::Acquire) >= older;
                if !enc.wait_for(reached) || enc.has_error() {
                    return WaitResult::Failed;
                }
                let batches: Vec<_> = {
                    let mut q = locked(&self.inflight);
                    let n = q.iter().take_while(|(f, _)| *f <= older).count();
                    q.drain(..n).collect()
                };
                let mut last = None;
                for (_, b) in &batches {
                    match b.wait() {
                        Ok(stats) => last = Some(stats),
                        Err(_) => return WaitResult::Failed,
                    }
                }
                if let Some(stats) = last {
                    *locked(&self.last_gpu) = Some((stats.gpu_start_s, stats.gpu_end_s));
                    nxkit_nvn::screen::set_gpu_ms((stats.gpu_end_s - stats.gpu_start_s) * 1e3);
                }
                self.write_counters(older);
                self.done.fetch_max(older, Ordering::AcqRel);
            }
            return WaitResult::Done;
        }
        if let Some(enc) = &self.enc {
            let reached = || self.encoded.load(Ordering::Acquire) >= fence.0;
            if !enc.wait_for(reached) || enc.has_error() {
                return WaitResult::Failed;
            }
        }
        if self.drain() {
            WaitResult::Done
        } else {
            WaitResult::Failed
        }
    }

    fn wait_idle(&self) {
        if let Some(enc) = &self.enc {
            enc.wait_idle();
        }
        self.drain();
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
