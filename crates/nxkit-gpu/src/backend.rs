//! The NVN backend: the front end hands it whole frames, it encodes them with [`Gpu`] and shows them.
//!
//! The flow of a frame:
//!
//! 1. `submit` walks the recordings of each queue submit and builds the [`FrameDetail`] (`walk`).
//! 2. `flush_frame` encodes the frame and applies the report to the texture table.
//! 3. `present` keeps the Metal texture of the game and tells the [`Pacer`] that a frame is ready.
//! 4. A display tick reaches [`TickSink::tick`] on the main thread. The tick draws the texture into its drawable and
//!    presents it at the target time of the tick (`blit`).
//!
//! At most [`MAX_PENDING`] presented frames wait for a tick. `acquire` blocks the game while that many wait.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use nxkit_metal::{Context, DisplayTick, GuestMemory, Pacer, Shown, TickAction, TickSink, Texture};
use nxkit_nvn::backend::{
    Backend, BackendError, BackendResult, Fence, FlushCx, PoolInfo, Present, StageKey, WaitResult,
};
use nxkit_nvn::cmdbuf::CommandHandle;
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::front::FlushReason;
use nxkit_nvn::passes::PassPlan;
use nxkit_nvn::resource::{TexId, TextureRecord};
use nxkit_nvn::tic::Tsc;

use crate::blit::Blit;
use crate::detail::FrameDetail;
use crate::guest::GuestView;
use crate::settings::GpuSettings;
use crate::walk::Walker;
use crate::Gpu;

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
}

impl GpuBackend {
    /// Makes the backend over the mapped guest memory.
    pub fn new(ctx: &Context, guest: Arc<GuestMemory>, settings: &GpuSettings) -> nxkit_metal::Result<Arc<Self>> {
        let gpu = Gpu::with_config(ctx, guest.clone(), settings.config())?;
        let walker = Walker::new(settings.shader_dir.clone(), guest.address_map());
        Ok(Arc::new(Self {
            ctx: ctx.clone(),
            guest,
            gpu: Mutex::new(gpu),
            walker: Mutex::new(walker),
            scale: settings.scale,
            encoded: AtomicU64::new(0),
            done: AtomicU64::new(0),
            presented: AtomicU64::new(0),
            pending: Mutex::new(Vec::new()),
            freed: Condvar::new(),
            blit: Mutex::new(None),
            pacer: OnceLock::new(),
            reports: AtomicU64::new(0),
            blit_failed: AtomicBool::new(false),
        }))
    }

    /// Sets the pacer that learns about each presented frame. It is set once.
    pub fn set_pacer(&self, pacer: Arc<Pacer>) {
        let _ = self.pacer.set(pacer);
    }

    fn drain(&self) -> bool {
        let mut gpu = locked(&self.gpu);
        let upto = self.encoded.load(Ordering::Acquire);
        let ok = gpu.wait().is_ok();
        self.done.fetch_max(upto, Ordering::AcqRel);
        ok
    }
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
        locked(&self.gpu).register_pool(gpu_addr, pool.len).map_err(fail)
    }

    fn unregister_pool(&self, _host: u64) {}

    fn submit(&self, queue: u64, handles: &[CommandHandle], _origin_lower_left: bool) {
        let mut mem = GuestView(self.guest.clone());
        locked(&self.walker).submit(&mut mem, queue, handles);
    }

    fn flush_frame(
        &self,
        frame: &Frame,
        plan: &PassPlan,
        _reason: FlushReason,
        fence: Fence,
        cx: FlushCx<'_>,
    ) -> BackendResult<()> {
        let mut detail = locked(&self.walker).take();
        if !matches(frame, &detail) {
            eprintln!("nxkit-gpu: the walked detail does not match the frame, its draws are skipped");
            detail = FrameDetail::default();
        }
        let result = locked(&self.gpu).encode_frame(frame, plan, cx.table, cx.descriptors, &detail, self.scale);
        self.encoded.fetch_max(fence.0, Ordering::AcqRel);
        let report = result.map_err(fail)?;
        for id in report.deswizzled.iter().chain(&report.gpu_wrote) {
            let _ = cx.table.gpu_wrote(*id);
        }
        for id in &report.deswizzled {
            let _ = cx.table.synced(*id);
        }
        if !report.copied_back.is_empty() {
            self.drain();
            for id in &report.copied_back {
                let _ = cx.table.synced(*id);
            }
        }
        let n = self.reports.fetch_add(1, Ordering::Relaxed);
        if report.stats.skipped_total() > 0 && n < 16 {
            eprintln!("nxkit-gpu: frame {n}: {:?}", report.stats);
        }
        Ok(())
    }

    fn present(&self, present: Present) -> BackendResult<()> {
        let texture = locked(&self.gpu)
            .texture(present.texture)
            .ok_or_else(|| BackendError("the presented texture has no Metal texture".into()))?;
        let number = self.presented.fetch_add(1, Ordering::AcqRel) + 1;
        locked(&self.pending).push(Pending { number, texture });
        if let Some(p) = self.pacer.get() {
            p.game_frame_ready(number);
        }
        Ok(())
    }

    fn wait_fence(&self, fence: Fence, _timeout_ns: u64) -> WaitResult {
        if fence.0 == 0 || fence.0 <= self.done.load(Ordering::Acquire) {
            return WaitResult::AlreadyDone;
        }
        if self.drain() { WaitResult::Done } else { WaitResult::Failed }
    }

    fn wait_idle(&self) {
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
    }

    fn acquire(&self, _window: u64, _index: u32) {
        let mut pending = locked(&self.pending);
        while pending.len() >= MAX_PENDING {
            let (guard, timeout) = self.freed.wait_timeout(pending, ACQUIRE_WAIT).unwrap_or_else(PoisonError::into_inner);
            pending = guard;
            if timeout.timed_out() {
                pending.remove(0);
            }
        }
    }

    fn texture_created(&self, record: &TextureRecord) {
        locked(&self.gpu).texture_created(record.id);
    }

    fn texture_finalized(&self, id: TexId) {
        locked(&self.gpu).texture_destroyed(id);
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
        let texture = {
            let mut pending = locked(&self.pending);
            let found = pending.iter().position(|p| p.number == number).map(|i| pending.remove(i).texture);
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
        if crate::diag("pixelall") {
            let d = texture.desc();
            let pts: Vec<[u32; 2]> = (1..4)
                .flat_map(|j| (1..4).map(move |i| [d.width * i / 4, d.height * j / 4]))
                .collect();
            let px = texture.debug_read(&self.ctx, &pts).unwrap_or_default();
            let lit = px.iter().filter(|p| p[0] | p[1] | p[2] > 8).count();
            eprintln!("PX {} lit {lit}", PRESENT_DIAG[2].load(Ordering::Relaxed));
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
