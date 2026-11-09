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

mod api;
mod encpace;
mod tick;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use nxkit_metal::{Context, DisplayTick, GuestMemory, Pacer, Shown, Texture, TickAction, TickSink};
use nxkit_nvn::backend::{
    Backend, BackendError, BackendResult, Deferred, Fence, FlushCx, PoolInfo, Present, StageKey,
    WaitResult,
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
    /// The part of the texture the game shows (`nvnWindowSetCrop`, or the vi layer crop); zero size is all of it.
    crop: nxkit_nvn::backend::Rect,
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
    /// The presents of the game, counted when it presents, and the newest present number a tick showed.
    asked: AtomicU64,
    shown_upto: AtomicU64,
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
        let threaded =
            settings.enc_thread || std::env::var("NXKIT_ENC_THREAD").is_ok_and(|v| v == "1");
        Ok(Arc::new_cyclic(|weak: &Weak<Self>| {
            let enc = threaded
                .then(|| {
                    let weak = weak.clone();
                    Encoder::spawn(move |msg| match weak.upgrade() {
                        Some(backend) => backend.serve(msg),
                        None => Reply::Stop,
                    })
                    .inspect_err(|e| {
                        eprintln!("nxkit-gpu: no encoder thread, encoding on the game thread: {e}")
                    })
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
                asked: AtomicU64::new(0),
                shown_upto: AtomicU64::new(0),
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
        let Some((start, end)) = *locked(&self.last_gpu) else {
            return;
        };
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
            if let Some(off) = r.addr.checked_sub(map.gpu_base())
                && off + 16 <= map.len()
            {
                let _ = self.guest.write(off, &data);
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
            let r = gpu.encode_frame(
                &job.frame,
                &job.plan,
                &job.snapshot,
                &job.detail,
                self.scale,
            );
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
            self.copied_recent
                .store(!r.copied_back.is_empty(), Ordering::Release);
            if crate::diag("frame") && job.fence.0.is_multiple_of(60) {
                eprintln!(
                    "DIAG frame copyback fence {} copied_back {}",
                    job.fence.0,
                    r.copied_back.len()
                );
            }
        }
        self.hold_counters(job.fence, &job.detail);
        self.encoded.fetch_max(job.fence.0, Ordering::AcqRel);
        let report = result.map_err(|e| e.to_string())?;
        let epoch = |id: &TexId| job.snapshot.record(*id).ok().map(|r| (*id, r.write_epoch));
        let wrote = report
            .deswizzled
            .iter()
            .chain(&report.gpu_wrote)
            .filter_map(&epoch)
            .collect();
        let mut synced: Vec<_> = report
            .deswizzled
            .iter()
            .chain(&report.mirrored)
            .filter_map(&epoch)
            .collect();
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
        if crate::diag("frame") && fence.0.is_multiple_of(60) {
            eprintln!(
                "DIAG frame copyback fence {} copied_back {} (sync)",
                fence.0,
                report.copied_back.len()
            );
        }
        if !report.copied_back.is_empty() {
            self.drain();
        }
        let synced: crate::draws::FastSet<TexId> = report
            .deswizzled
            .iter()
            .chain(&report.mirrored)
            .chain(&report.copied_back)
            .copied()
            .collect();
        for id in report.gpu_wrote.iter().filter(|id| !synced.contains(id)) {
            let _ = cx.table.gpu_wrote(*id);
        }
        for id in report
            .deswizzled
            .iter()
            .chain(&report.mirrored)
            .chain(&report.copied_back)
        {
            let _ = cx.table.synced(*id);
        }
        self.note_report(&report);
        Ok(())
    }

    /// Prints the diagnostics of a finished encode.
    fn note_report(&self, report: &EncodeReport) {
        let n = self.reports.fetch_add(1, Ordering::Relaxed);
        if crate::diag("memory") && n.is_multiple_of(120) {
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
                crate::nobar_range(),
                st.draws,
                st.render_passes,
                st.compute_passes,
                st.textures_created,
                st.textures_rescaled,
                st.uploads,
                st.copybacks,
                st.skipped_total(),
                st.skipped
            );
        }
        if (report.stats.skipped_total() > 0 && n < 16) || (diag && n.is_multiple_of(30)) {
            eprintln!("nxkit-gpu: frame {n}: {:?}", report.stats);
        }
        if diag && n.is_multiple_of(30) {
            eprintln!("nxkit-gpu: {}", crate::coverage::describe());
        }
        // The mesh and indirect draws: every 30 frames under `stats` or `mesh`, and the first frame that has any.
        let kinds = &report.stats.kinds;
        if !kinds.is_empty() {
            static FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
            let first = FIRST.swap(false, Ordering::Relaxed);
            if first || ((diag || crate::diag("mesh")) && n.is_multiple_of(30)) {
                eprintln!(
                    "nxkit-gpu: frame {n}: mesh and indirect draws: {}",
                    kinds.describe()
                );
            }
        }
    }

    /// Keeps the Metal texture of the presented frame and tells the pacer. Runs after the encode of that frame.
    fn present_now(&self, present: Present) -> BackendResult<()> {
        // The number goes first, so that a present without a texture still counts as one of the game's presents.
        let number = self.presented.fetch_add(1, Ordering::AcqRel) + 1;
        let texture = locked(&self.gpu)
            .texture(present.texture)
            .ok_or_else(|| BackendError("the presented texture has no Metal texture".into()))?;
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
        locked(&self.pending).push(Pending {
            number,
            texture,
            crop: present.crop,
        });
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
