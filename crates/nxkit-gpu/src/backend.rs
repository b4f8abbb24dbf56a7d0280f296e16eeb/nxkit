//! The NVN backend: the front end hands it whole frames, it encodes them with [`Gpu`] and shows them.
//!
//! The flow of a frame:
//!
//! 1. `submit` walks the recordings of each queue submit and builds the [`FrameDetail`] (`walk`).
//! 2. `flush_frame` encodes the frame and applies the report to the texture table. With the encoder thread (`enc_thread`)
//!    it hands a copy of the frame and a [`Snapshot`] of the textures to the thread and returns. The report goes into
//!    the table at the next `flush_frame` or `settle`, and every wait for a fence waits for the thread first.
//! 3. `present` keeps the Metal texture of the game and tells the [`Pacer`] that a frame is ready.
//! 4. A display tick reaches [`TickSink::tick`] on the main thread. The tick draws the texture into its drawable and
//!    presents it at the target time of the tick (`blit`).
//!
//! At most [`MAX_PENDING`] presented frames wait for a tick. `acquire` blocks the game while that many wait.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use nxkit_metal::{Context, DisplayTick, GuestMemory, Pacer, Shown, Texture, TickAction, TickSink};
use nxkit_nvn::backend::{
    Backend, BackendError, BackendResult, Deferred, Fence, FlushCx, PoolInfo, Present, StageKey, WaitResult,
};
use nxkit_nvn::cmdbuf::CommandHandle;
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::front::FlushReason;
use nxkit_nvn::passes::PassPlan;
use nxkit_nvn::resource::{ResourceTable, TexId, TextureRecord};
use nxkit_nvn::tic::Tsc;

use crate::Gpu;
use crate::blit::Blit;
use crate::detail::FrameDetail;
use crate::encthread::{Encoder, Job, Msg, Reply, TableUpdate};
use crate::guest::GuestView;
use crate::settings::GpuSettings;
use crate::snapshot::{EncodeView, Live, Snapshot};
use crate::stats::EncodeReport;
use crate::walk::{Invalidations, Walker};

/// The presented frames that may wait for a display tick.
pub const MAX_PENDING: usize = 3;

const ACQUIRE_WAIT: Duration = Duration::from_millis(100);

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Pending {
    number: u64,
    texture: Texture,
}

/// The backend of the front end and the sink of the display ticks.
pub struct GpuBackend {
    ctx: Context,
    guest: Arc<GuestMemory>,
    gpu: Mutex<Gpu>,
    walker: Mutex<Walker>,
    /// The memory writes that the walker has not seen yet. It has a lock of its own, so a write never waits for a walk.
    invalidations: Arc<Invalidations>,
    /// The counters of `NXKIT_DIAG=overlap`.
    overlap: crate::overlap::Overlap,
    scale: f32,
    /// The last fence whose frame was encoded.
    encoded: AtomicU64,
    /// The last fence whose frame the GPU finished.
    done: AtomicU64,
    presented: AtomicU64,
    pending: Mutex<Vec<Pending>>,
    freed: Condvar,
    blit: Mutex<Option<Blit>>,
    pacer: OnceLock<Arc<Pacer>>,
    reports: AtomicU64,
    /// The timestamp reports of encoded frames that the GPU has not been seen to finish, with the fence of their
    /// frame. [`GpuBackend::drain`] writes them.
    counters: Mutex<Vec<(u64, crate::detail::CounterReport)>>,
    /// The GPU start and end of the last finished frame, in seconds of the host clock.
    last_gpu: Mutex<Option<(f64, f64)>>,
    blit_failed: AtomicBool,
    /// The encoder thread, when `enc_thread` is on.
    enc: Option<Encoder>,
    /// The newest fence that was flushed.
    flushed: AtomicU64,
    /// With the encoder thread: the batches on the GPU with the fence of their frame, so that a wait for the GPU needs
    /// no lock that the encoder holds.
    inflight: Mutex<std::collections::VecDeque<(u64, nxkit_metal::SubmittedBatch)>>,
    /// Whether the last encoded frame copied textures back to CPU memory. The overlap then waits in full, because the
    /// next frame likely copies back too and the CPU reads what it copied.
    copied_recent: AtomicBool,
}

impl GpuBackend {
    /// Makes the backend over the mapped guest memory.
    pub fn new(
        ctx: &Context,
        guest: Arc<GuestMemory>,
        settings: &GpuSettings,
    ) -> nxkit_metal::Result<Arc<Self>> {
        let gpu = Gpu::with_config(ctx, guest.clone(), settings.config())?;
        let walker = Walker::new(settings.shader_dir.clone(), guest.address_map());
        let invalidations = walker.invalidations();
        let threaded = settings.enc_thread || std::env::var("NXKIT_ENC_THREAD").is_ok_and(|v| v == "1");
        Ok(Arc::new_cyclic(|weak: &Weak<Self>| {
            let enc = threaded
                .then(|| {
                    let weak = weak.clone();
                    Encoder::spawn(move |msg| match weak.upgrade() {
                        Some(backend) => backend.serve(msg),
                        None => Reply::Stop,
                    })
                    .inspect_err(|e| eprintln!("nxkit-gpu: no encoder thread, encoding on the game thread: {e}"))
                    .ok()
                })
                .flatten();
            Self {
                ctx: ctx.clone(),
                guest,
                gpu: Mutex::new(gpu),
                walker: Mutex::new(walker),
                invalidations,
                overlap: crate::overlap::Overlap::default(),
                scale: settings.scale,
                encoded: AtomicU64::new(0),
                done: AtomicU64::new(0),
                presented: AtomicU64::new(0),
                pending: Mutex::new(Vec::new()),
                freed: Condvar::new(),
                blit: Mutex::new(None),
                pacer: OnceLock::new(),
                reports: AtomicU64::new(0),
                counters: Mutex::new(Vec::new()),
                last_gpu: Mutex::new(None),
                blit_failed: AtomicBool::new(false),
                enc,
                flushed: AtomicU64::new(0),
                inflight: Mutex::new(std::collections::VecDeque::new()),
                copied_recent: AtomicBool::new(false),
            }
        }))
    }

    /// Sets the pacer that learns about each presented frame. It is set once.
    pub fn set_pacer(&self, pacer: Arc<Pacer>) {
        let _ = self.pacer.set(pacer);
    }

    fn drain(&self) -> bool {
        let mut gpu = locked(&self.gpu);
        let upto = self.encoded.load(Ordering::Acquire);
        let result = gpu.wait();
        drop(gpu);
        if let Ok(Some(stats)) = &result {
            *locked(&self.last_gpu) = Some((stats.gpu_start_s, stats.gpu_end_s));
            nxkit_nvn::screen::set_gpu_ms((stats.gpu_end_s - stats.gpu_start_s) * 1e3);
        }
        self.write_counters(upto);
        self.done.fetch_max(upto, Ordering::AcqRel);
        result.is_ok()
    }

    /// Keeps the timestamp reports of a frame until the GPU is seen to finish it.
    fn hold_counters(&self, fence: Fence, detail: &FrameDetail) {
        if !detail.reports.is_empty() {
            locked(&self.counters).extend(detail.reports.iter().map(|r| (fence.0, *r)));
        }
    }

    /// Writes the timestamp reports of the frames up to fence `upto`. A `TIMESTAMP_TOP` gets the GPU start of the
    /// last finished frame and a `TIMESTAMP` its end, in GPU ticks (19.2 MHz). The game takes its GPU time from the
    /// difference and picks its frame interval from it; without the reports it saw no GPU time and kept a 30 fps
    /// step on frames that took longer, which ran the game in slow motion.
    fn write_counters(&self, upto: u64) {
        let Some((start, end)) = *locked(&self.last_gpu) else { return };
        let due: Vec<_> = {
            let mut c = locked(&self.counters);
            let (due, keep): (Vec<_>, Vec<_>) = c.drain(..).partition(|(f, _)| *f <= upto);
            *c = keep;
            due
        };
        let ticks = |s: f64| (s.max(0.0) * 1e9 * 384.0 / 625.0) as u64;
        let map = self.guest.address_map();
        for (_, r) in due {
            let t = ticks(if r.kind == 0x10 { start } else { end });
            let mut data = [0u8; 16];
            data[8..].copy_from_slice(&t.to_le_bytes());
            if let Some(off) = r.addr.checked_sub(map.gpu_base()) {
                if off + 16 <= map.len() {
                    let _ = self.guest.write(off, &data);
                }
            }
        }
    }
}

impl GpuBackend {
    /// Serves one message on the encoder thread.
    fn serve(&self, msg: Msg) -> Reply {
        match msg {
            Msg::Job(job) => Reply::Job(self.encode_job(&job)),
            Msg::Present(present) => {
                if let Err(e) = self.present_now(present) {
                    eprintln!("nxkit-gpu: the present failed on the encoder thread: {e}");
                }
                Reply::Done
            }
            Msg::Retire(id) => {
                locked(&self.gpu).texture_destroyed(id);
                Reply::Done
            }
        }
    }

    /// Encodes a job and waits for the copy-back that its report asks for. The result tells the front end what to change
    /// in its table.
    fn encode_job(&self, job: &Job) -> Result<TableUpdate, String> {
        let (result, submitted) = {
            let mut gpu = locked(&self.gpu);
            let r = gpu.encode_frame(&job.frame, &job.plan, &job.snapshot, &job.detail, self.scale);
            (r, gpu.last_submitted())
        };
        if let Some(b) = submitted {
            let mut q = locked(&self.inflight);
            if q.back().is_none_or(|(_, last)| last.number() != b.number()) {
                q.push_back((job.fence.0, b));
            }
            while q.len() > 8 {
                q.pop_front();
            }
        }
        if let Ok(r) = &result {
            self.copied_recent.store(!r.copied_back.is_empty(), Ordering::Release);
            if crate::diag("frame") && job.fence.0 % 60 == 0 {
                eprintln!("DIAG frame copyback fence {} copied_back {}", job.fence.0, r.copied_back.len());
            }
        }
        self.hold_counters(job.fence, &job.detail);
        self.encoded.fetch_max(job.fence.0, Ordering::AcqRel);
        let report = result.map_err(|e| e.to_string())?;
        let epoch = |id: &TexId| job.snapshot.record(*id).ok().map(|r| (*id, r.write_epoch));
        let wrote = report.deswizzled.iter().chain(&report.gpu_wrote).filter_map(&epoch).collect();
        let mut synced: Vec<_> = report.deswizzled.iter().chain(&report.mirrored).filter_map(&epoch).collect();
        if !report.copied_back.is_empty() {
            self.drain();
            synced.extend(report.copied_back.iter().filter_map(&epoch));
        }
        self.note_report(&report);
        Ok(TableUpdate { wrote, synced })
    }

    /// Encodes a frame on the calling thread and applies the report to the live table.
    fn flush_sync(
        &self,
        frame: &Frame,
        plan: &PassPlan,
        fence: Fence,
        detail: &FrameDetail,
        cx: FlushCx<'_>,
    ) -> BackendResult<()> {
        let view = Live {
            table: &*cx.table,
            descriptors: cx.descriptors,
        };
        let result = locked(&self.gpu).encode_frame(frame, plan, &view, detail, self.scale);
        self.hold_counters(fence, detail);
        self.encoded.fetch_max(fence.0, Ordering::AcqRel);
        let report = result.map_err(fail)?;
        // Only the state at the end of the frame is set: a texture that is synced in this frame is not first marked
        // as written. Nothing reads the state in between, and setting both every frame made every texture that is
        // copied back flip twice a frame, which defeated the alias cache of the table.
        if crate::diag("frame") && fence.0 % 60 == 0 {
            eprintln!("DIAG frame copyback fence {} copied_back {} (sync)", fence.0, report.copied_back.len());
        }
        if !report.copied_back.is_empty() {
            self.drain();
        }
        let synced: crate::draws::FastSet<TexId> =
            report.deswizzled.iter().chain(&report.mirrored).chain(&report.copied_back).copied().collect();
        for id in report.gpu_wrote.iter().filter(|id| !synced.contains(id)) {
            let _ = cx.table.gpu_wrote(*id);
        }
        for id in report.deswizzled.iter().chain(&report.mirrored).chain(&report.copied_back) {
            let _ = cx.table.synced(*id);
        }
        self.note_report(&report);
        Ok(())
    }

    /// Prints the diagnostics of a finished encode.
    fn note_report(&self, report: &EncodeReport) {
        let n = self.reports.fetch_add(1, Ordering::Relaxed);
        if crate::diag("memory") && n % 120 == 0 {
            eprintln!(
                "DIAG memory frame {n}: Metal {} MiB, resident allocations {}",
                self.ctx.allocated_bytes() / (1024 * 1024),
                self.ctx.resident_count()
            );
        }
        let diag = stats_diag();
        if crate::diag("pixelall") && n.is_multiple_of(10) {
            let st = &report.stats;
            eprintln!(
                "DIAG pixelall frame {n} bar {:?} draws {} passes {}/{} created {} rescaled {} uploads {} copybacks {} skipped {} {:?}",
                crate::nobar_range(), st.draws, st.render_passes, st.compute_passes, st.textures_created, st.textures_rescaled,
                st.uploads, st.copybacks, st.skipped_total(), st.skipped
            );
        }
        if (report.stats.skipped_total() > 0 && n < 16) || (diag && n % 30 == 0) {
            eprintln!("nxkit-gpu: frame {n}: {:?}", report.stats);
        }
    }

    /// Keeps the Metal texture of the presented frame and tells the pacer. Runs after the encode of that frame.
    fn present_now(&self, present: Present) -> BackendResult<()> {
        let texture = locked(&self.gpu)
            .texture(present.texture)
            .ok_or_else(|| BackendError("the presented texture has no Metal texture".into()))?;
        let number = self.presented.fetch_add(1, Ordering::AcqRel) + 1;
        encpace::ready(number);
        if crate::diag("present") && number % 120 == 1 {
            eprintln!(
                "DIAG present id {:?} {}x{} {:?}",
                present.texture,
                texture.desc().width,
                texture.desc().height,
                texture.desc().format
            );
        }
        locked(&self.pending).push(Pending { number, texture });
        if let Some(p) = self.pacer.get() {
            p.game_frame_ready(number);
        }
        Ok(())
    }

    /// Drops the Metal texture of `id`, in the order of the frames: on the encoder thread when there is one.
    fn retire(&self, id: TexId) {
        match &self.enc {
            Some(enc) if enc.send(Msg::Retire(id)) => {}
            _ => locked(&self.gpu).texture_destroyed(id),
        }
    }
}

/// Drops `v` on a thread of its own. The detail of a frame holds thousands of draws; freeing them took about a millisecond
/// of the game's render thread at the end of every frame.
fn drop_later<T: Send + 'static>(v: T) {
    type Sender = std::sync::mpsc::Sender<Box<dyn Send>>;
    static TX: OnceLock<Option<Mutex<Sender>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Box<dyn Send>>();
        let spawned = std::thread::Builder::new()
            .name("nxkit-drop".into())
            .spawn(move || rx.into_iter().for_each(drop));
        spawned.ok().map(|_| Mutex::new(tx))
    });
    let v: Box<dyn Send> = Box::new(v);
    match tx {
        Some(tx) => {
            if let Err(e) = locked(tx).send(v) {
                drop(e.0);
            }
        }
        None => drop(v),
    }
}

/// Whether `NXKIT_DIAG` asks for the overlap counters.
fn overlap_diag() -> bool {
    static DIAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DIAG.get_or_init(|| crate::diag("overlap"))
}

/// Whether `NXKIT_DIAG` asks for the periodic counters.
fn stats_diag() -> bool {
    static DIAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DIAG.get_or_init(|| crate::diag("stats"))
}

/// True when the detail has one entry for each command that needs one.
fn matches(frame: &Frame, d: &FrameDetail) -> bool {
    let (mut draws, mut clears, mut dispatches) = (0, 0, 0);
    for c in &frame.cmds {
        match c {
            Cmd::Draw { .. } => draws += 1,
            Cmd::ClearColor { .. } | Cmd::ClearDepthStencil { .. } => clears += 1,
            Cmd::Dispatch { .. } => dispatches += 1,
            _ => {}
        }
    }
    d.draws.len() == draws && d.clears.len() == clears && d.dispatches.len() == dispatches
}

fn fail(e: impl std::fmt::Display) -> BackendError {
    BackendError(e.to_string())
}

impl Backend for GpuBackend {
    fn gpu_base(&self) -> u64 {
        self.guest.address_map().gpu_base()
    }

    fn host_range(&self) -> Option<(u64, u64)> {
        Some((self.guest.host_base(), self.guest.len()))
    }

    fn register_pool(&self, pool: PoolInfo) -> BackendResult<()> {
        let gpu_addr = self.guest.gpu_address(pool.host, pool.len).map_err(fail)?;
        locked(&self.gpu)
            .register_memory_pool(gpu_addr, pool.len, pool.flags)
            .map_err(fail)
    }

    fn unregister_pool(&self, host: u64) {
        if let Ok(gpu_addr) = self.guest.gpu_address(host, 1) {
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
                    fence,
                });
                if !enc.submit(job) {
                    return Err(BackendError("the encoder thread is gone".into()));
                }
                return settled.error.map_or(Ok(()), |e| Err(BackendError(e)));
            }
        }
        let r = self.flush_sync(frame, plan, fence, &detail, cx);
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
        if crate::overlap::on() && self.enc.is_none() && fence.0 == self.encoded.load(Ordering::Acquire) && fence.0 > 1 {
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
            && fence.0 > 1
            && fence.0 == self.flushed.load(Ordering::Acquire)
            && !self.copied_recent.load(Ordering::Acquire)
        {
            let older = fence.0 - 1;
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
            && let Ok(gpu) = self.guest.gpu_address(start, end.saturating_sub(start).max(1))
        {
            self.overlap.flushed(gpu, end.saturating_sub(start), self.done.load(Ordering::Acquire));
        }
    }

    fn acquire(&self, _window: u64, _index: u32) {
        let mut pending = locked(&self.pending);
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

/// Counters of the present path for the `present` diagnostics: ticks, presents asked, textures found, blits done.
static PRESENT_DIAG: [std::sync::atomic::AtomicU64; 4] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 4];

fn present_diag(i: usize) {
    if !crate::diag("present") {
        return;
    }
    let n = PRESENT_DIAG[i].fetch_add(1, Ordering::Relaxed) + 1;
    if i == 0 && n % 120 == 0 {
        let v: Vec<u64> = PRESENT_DIAG
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect();
        eprintln!(
            "DIAG present ticks {} asked {} found {} blitted {}",
            v[0], v[1], v[2], v[3]
        );
    }
}

impl TickSink for GpuBackend {
    fn tick(&self, action: TickAction, tick: DisplayTick) {
        present_diag(0);
        let TickAction::Present(shown) = action else {
            return;
        };
        present_diag(1);
        let number = match shown {
            Shown::Game { frame } => frame,
            Shown::Interpolated { to, .. } => to,
        };
        encpace::shown(number, locked(&self.pending).len());
        let texture = {
            let mut pending = locked(&self.pending);
            let found = pending
                .iter()
                .position(|p| p.number == number)
                .map(|i| pending.remove(i).texture);
            for p in pending.iter().filter(|p| p.number < number) {
                nxkit_nvn::screen::shown(p.number, -1.0);
            }
            pending.retain(|p| p.number > number);
            self.freed.notify_all();
            found
        };
        let Some(texture) = texture else {
            return;
        };
        present_diag(2);
        if crate::diag("present") && PRESENT_DIAG[2].load(Ordering::Relaxed) % 300 == 1 {
            eprintln!("DIAG present texture {:?}", texture.desc());
        }
        if crate::diag("pixelall") && PRESENT_DIAG[2].load(Ordering::Relaxed).is_multiple_of(10) {
            let d = texture.desc();
            let pts: Vec<[u32; 2]> = (1..4)
                .flat_map(|j| (1..4).map(move |i| [d.width * i / 4, d.height * j / 4]))
                .collect();
            let px = texture.debug_read(&self.ctx, &pts).unwrap_or_default();
            let lit = px.iter().filter(|p| p[0] | p[1] | p[2] > 8).count();
            eprintln!("DIAG pixelall present {} lit {lit}/9", PRESENT_DIAG[2].load(Ordering::Relaxed));
        }
        if crate::diag("pixel") && PRESENT_DIAG[2].load(Ordering::Relaxed) % 60 == 1 {
            let d = texture.desc();
            let pts: Vec<[u32; 2]> = (1..4)
                .flat_map(|j| (1..4).map(move |i| [d.width * i / 4, d.height * j / 4]))
                .collect();
            let px = texture.debug_read(&self.ctx, &pts).unwrap_or_default();
            let lit = px.iter().filter(|p| p[0] | p[1] | p[2] != 0).count();
            eprintln!(
                "DIAG pixel {}x{} {:?} lit {lit}/9 {px:?}",
                d.width, d.height, d.format
            );
        }
        let mut blit = locked(&self.blit);
        if blit.is_none() {
            *blit = Blit::new(&self.ctx)
                .inspect_err(|e| {
                    if !self.blit_failed.swap(true, Ordering::Relaxed) {
                        eprintln!("nxkit-gpu: the present blit cannot be made: {e}");
                    }
                })
                .ok();
        }
        let Some(b) = blit.as_mut() else {
            return;
        };
        if matches!(shown, Shown::Game { .. }) {
            tick.drawable.on_presented(move |secs| nxkit_nvn::screen::shown(number, secs));
        }
        let r = b.present(&texture, tick.drawable, tick.target_presentation_timestamp);
        if r.is_ok() {
            present_diag(3);
        }
        if let Err(e) = r {
            if !self.blit_failed.swap(true, Ordering::Relaxed) {
                eprintln!("nxkit-gpu: the present failed: {e}");
            }
        }
    }
}

/// `NXKIT_DIAG=encpace`: for every 60th frame, the time from the game's present to the end of its encode and from there
/// to the tick that shows it, to see where a deeper pipeline loses time.
mod encpace {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    const N: usize = 256;
    static ASKED: AtomicU64 = AtomicU64::new(0);
    static AT_ASK: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
    static AT_READY: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

    fn on() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| crate::diag("encpace"))
    }

    fn now() -> u64 {
        static BASE: OnceLock<Instant> = OnceLock::new();
        u64::try_from(BASE.get_or_init(Instant::now).elapsed().as_micros()).unwrap_or(0)
    }

    /// The game presented its next frame.
    pub(super) fn asked() {
        if on() {
            let n = ASKED.fetch_add(1, Ordering::Relaxed) + 1;
            AT_ASK[n as usize % N].store(now(), Ordering::Relaxed);
        }
    }

    /// Frame `n` is encoded and queued for the screen.
    pub(super) fn ready(n: u64) {
        if on() {
            AT_READY[n as usize % N].store(now(), Ordering::Relaxed);
        }
    }

    /// A tick shows frame `n`, with `pending` frames queued.
    pub(super) fn shown(n: u64, pending: usize) {
        if on() && n % 60 == 0 {
            let t = now();
            let ask = AT_ASK[n as usize % N].load(Ordering::Relaxed);
            let ready = AT_READY[n as usize % N].load(Ordering::Relaxed);
            eprintln!(
                "DIAG encpace frame {n}: present->ready {:.2} ms, ready->shown {:.2} ms, asked {} pending {pending}",
                ready.saturating_sub(ask) as f64 / 1e3,
                t.saturating_sub(ready) as f64 / 1e3,
                ASKED.load(Ordering::Relaxed),
            );
        }
    }
}
