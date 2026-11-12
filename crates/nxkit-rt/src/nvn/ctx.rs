//! The process-wide GPU context of the NVN C ABI.
//!
//! One [`Gpu`] exists per process. The host installs it with [`install`] before the game starts, with the backend
//! (`nxkit-gpu` implements [`Backend`]) and the [`Settings`] of the game. A function of the C ABI gets it with [`gpu`].
//! A call before `install` ends the game with the name of the function (R15).
//!
//! What it holds:
//! - the [`FrontEnd`] behind one lock. It owns the resource table, the descriptor pools, the command buffers, the queue
//!   states, the recording cache and the frame. [`Gpu::lock`] and [`Gpu::with_front`] give access. The lock is the one
//!   global lock of the ABI. A call that only reads or writes its own object in guest memory (a builder, a getter) does
//!   not take it. A recording call (command buffer) and a submit do. The lock is not re-entrant. The memory callback of a
//!   command buffer runs with the lock held, so a callback must not call back into [`Gpu`] or the front end. The
//!   command buffer group must give the front end a `MemoryCallback` that only returns a block.
//! - the backend ([`Gpu::backend`]).
//! - the flush counter and the fence rules ([`Gpu::flush`], [`Gpu::fence_now`], [`Gpu::wait`], [`Gpu::finish`]).
//! - the registries that map a guest address to a host value: [`Gpu::textures`] (guest `NVNtexture` to `TexId`),
//!   [`Gpu::programs`] and the command buffer numbers ([`Gpu::command_buffer_index`]).
//!
//! Fences. The front end numbers the flushes 1, 2, 3 (see [`Backend`]). The number of a flush is the fence of the work
//! that it holds. [`Gpu::fence_now`] is the fence of everything that the game submitted so far: it is the number of the
//! last flush when the frame is empty, and the number of the next flush otherwise. [`Gpu::wait`] flushes the frame first
//! when the fence is the one of the open frame, so a CPU wait never waits for a flush that nobody will make. A wait with
//! a timeout of zero flushes too, so a game that polls does not wait for ever.
//!
//! Queues. Every queue's submits go into the one open frame in the order of the calls. A submit that waits for an event
//! or a sync that another queue has not signalled yet is held, with what its queue does after it, until the signal is
//! in the frame (`gate` decides, `order` runs). A flush runs what is still held first. A game that never records a
//! wait or an event signal skips the gate.
//!
//! A CPU event signal that releases held submits with at least `NXKIT_RELEASE_FLUSH_MIN` draws, dispatches and copies
//! (default 64) flushes the frame at once (`FlushReason::Release`, `NXKIT_RELEASE_FLUSH=0` turns it off): the game
//! signals the event when it has recorded the work of the frame, and the present comes a few ms later, so the encode
//! of the work starts those ms earlier. The flush runs no held op. The submits that it carries and the fences they make
//! belong to its number; what is still held and what comes after goes into the next flush, which the present makes.
//!
//! A backend may encode a flushed frame on a thread of its own. [`Backend::wait_fence`] and [`Backend::wait_idle`] then
//! wait for that thread, and [`Backend::settle`] brings the texture table up to date before the front end reads the
//! residency of a texture (`nvnTextureReadTexels`). [`Backend::flush_frame`] settles the frame before it.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use nxkit_abi::guest;
use nxkit_nvn::backend::{Backend, Fence, FlushCx, Present, WaitResult};
use nxkit_nvn::cmdbuf::CommandHandle;
use nxkit_nvn::error::{NvnError, Result as NvnResult};
use nxkit_nvn::front::{FlushReason, FrontEnd};
use nxkit_nvn::mem::GuestMem;
use nxkit_nvn::resource::TexId;
use nxkit_nvn::space::{GpuSpace, LinearSpace};

use super::abi::{AbiResult, Registry, fatal, log_once};
use super::program::ProgramRecord;

mod gate;
mod order;

/// Guest memory for the front end: the range that the host named with `nxkit_abi::guest::set_range`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct GuestMemory;

impl GuestMem for GuestMemory {
    fn read(&self, host: u64, out: &mut [u8]) -> NvnResult<()> {
        guest::read(host, out).map_err(|f| NvnError::OutOfRange {
            addr: f.addr,
            len: f.len,
        })
    }

    fn write(&mut self, host: u64, data: &[u8]) -> NvnResult<()> {
        guest::write(host, data).map_err(|f| NvnError::OutOfRange {
            addr: f.addr,
            len: f.len,
        })
    }
}

/// The front end as the ABI uses it.
pub(crate) type Front = FrontEnd<LinearSpace, GuestMemory>;

/// What the host decides per game.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The first number of the API version that `nvnDeviceGetInteger` reports (item 0). Default 55.
    pub api_major: u32,
    /// The second number (item 1). Default 15.
    pub api_minor: u32,
    /// A byte range of the shader control section that the shader key sets to zero, as offset and length. The profile
    /// of a game has it when the section holds a value that changes from run to run. None keeps every byte.
    pub shader_key_skip: Option<(u32, u32)>,
    /// The GPU code version that `nvnDeviceGetInteger` reports for items 0x2E (major) and 0x30 (minor): the shader
    /// binaries that the host translates. Default 1.16, the range that the shader step translates. A game of an older
    /// SDK still gets 1.16, because it rejects a shader of a newer compiler when the device reports less.
    pub gpu_code: (u32, u32),
    /// The guest clock in nanoseconds, for `nvnDeviceGetCurrentTimestampInNanoseconds`. The default is the monotonic
    /// clock of the host (`mach_absolute_time` in nanoseconds), the base of the system tick and of the GPU times in
    /// the reports.
    pub clock_ns: fn() -> u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_major: 55,
            api_minor: 15,
            shader_key_skip: None,
            gpu_code: (1, 16),
            clock_ns: nxkit_hos::sys::monotonic_ns,
        }
    }
}

/// Why [`install`] failed.
#[derive(Debug)]
pub enum InstallError {
    /// A context was installed before.
    AlreadyInstalled,
    /// The host did not name the guest range, or the range does not fit the GPU address space.
    Space(NvnError),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstallError::AlreadyInstalled => f.write_str("a GPU context is installed already"),
            InstallError::Space(e) => write!(f, "the guest range cannot be mapped: {e}"),
        }
    }
}

impl std::error::Error for InstallError {}

static GPU: OnceLock<Gpu> = OnceLock::new();

/// Installs the GPU context. The host calls it once, after `nxkit_abi::guest::set_range`.
/// The backend gives the GPU address of the first byte of the memory that the GPU buffer covers
/// ([`Backend::gpu_base`]) and, when that is less than the guest range, the host range of the buffer
/// ([`Backend::host_range`]).
pub fn install(backend: Arc<dyn Backend>, settings: Settings) -> Result<(), InstallError> {
    let (base, len) = backend.host_range().unwrap_or_else(guest::range);
    let space = LinearSpace::new(base, backend.gpu_base(), len).map_err(InstallError::Space)?;
    GPU.set(Gpu::new(space, backend, settings))
        .map_err(|_| InstallError::AlreadyInstalled)?;
    // The video service writes decoded pictures into native images itself; the textures over them load again.
    nxkit_svc_video::set_guest_write_hook(|addr, len| {
        if let Some(g) = GPU.get() {
            g.guest_wrote_deferred(addr, len);
        }
    });
    super::wire::wire();
    Ok(())
}

/// The installed context. A call before [`install`] ends the game with the name of the function.
pub(crate) fn gpu(name: &str) -> &'static Gpu {
    GPU.get()
        .unwrap_or_else(|| fatal(name, "no GPU backend is installed"))
}

/// The installed context, None before [`install`].
pub(crate) fn installed() -> Option<&'static Gpu> {
    GPU.get()
}

/// The settings of the installed context. None before [`install`]. The SDK take-overs of `gfx` read the API version
/// with it and must not end the game when no context exists.
pub(crate) fn settings_if_installed() -> Option<&'static Settings> {
    GPU.get().map(Gpu::settings)
}

/// The command buffer numbers: a number for each live guest `NVNcommandBuffer`, with the numbers of released ones to reuse.
#[cfg(test)]
#[derive(Debug, Default)]
struct CommandBufferIds {
    free: Vec<usize>,
    next: usize,
}

/// The GPU context. See the module documentation.
pub(crate) struct Gpu {
    front: Mutex<Front>,
    /// A copy of the address mapping of the front end. It never changes, so address queries need no lock.
    space: LinearSpace,
    backend: Arc<dyn Backend>,
    settings: Settings,
    flushed: AtomicU64,
    /// The work that waits for a signal of another queue (`gate`). Taken after the front lock, never before it.
    gate: Mutex<gate::Gate>,
    /// Whether a CPU event signal that released a lot of work flushes the frame at once, and how much work is a lot.
    release_flush: order::ReleaseFlush,
    /// The ranges of `nvnMemoryPoolFlushMappedRange` that [`Gpu::lock`] has not yet told the front end.
    pending_writes: Mutex<Vec<(u64, u64)>>,
    /// The memory spans of recordings that ended since the last [`Gpu::lock`] (`nvnCommandBufferEndRecording`): the front
    /// end drops the summaries it kept of recordings in them.
    pending_recorded: Mutex<Vec<std::ops::Range<u64>>>,
    /// True when `pending_writes` or `pending_recorded` may hold a range. Keeps the common case of [`Gpu::lock`] to one
    /// atomic load.
    writes_dirty: AtomicBool,
    /// The guest address of an initialised `NVNtexture` to its id in the resource table. The texture group maintains it.
    pub(crate) textures: Registry<TexId>,
    /// The guest address of an initialised `NVNprogram` to its stages. The program group maintains it.
    pub(crate) programs: Registry<ProgramRecord>,
    #[cfg(test)]
    cb_ids: Registry<usize>,
    #[cfg(test)]
    cb_alloc: Mutex<CommandBufferIds>,
}

impl fmt::Debug for Gpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gpu")
            .field("flushed", &self.flushed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Gpu {
    /// A context over `space`. [`install`] builds the space from the guest range.
    pub(crate) fn new(space: LinearSpace, backend: Arc<dyn Backend>, settings: Settings) -> Self {
        Self {
            front: Mutex::new(FrontEnd::new(space, GuestMemory)),
            space,
            backend,
            settings,
            flushed: AtomicU64::new(0),
            gate: Mutex::new(gate::Gate::new(order::ordering_on())),
            release_flush: order::release_flush_config(),
            pending_writes: Mutex::new(Vec::new()),
            pending_recorded: Mutex::new(Vec::new()),
            writes_dirty: AtomicBool::new(false),
            textures: Registry::new(),
            programs: Registry::new(),
            #[cfg(test)]
            cb_ids: Registry::new(),
            #[cfg(test)]
            cb_alloc: Mutex::new(CommandBufferIds::default()),
        }
    }

    /// The backend.
    pub(crate) fn backend(&self) -> &dyn Backend {
        self.backend.as_ref()
    }

    /// The settings of the game.
    pub(crate) fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The number of the last flush that was handed to the backend.
    pub(crate) fn flushed(&self) -> u64 {
        self.flushed.load(Ordering::Acquire)
    }

    /// Takes the lock of the front end. The lock is not re-entrant. A thread that panicked while it held the lock does
    /// not poison it, because the ABI never unwinds into the guest (a failure aborts).
    ///
    /// The ranges that [`Gpu::guest_wrote_deferred`] queued so far are handed to [`Front::guest_wrote`] before the
    /// guard returns, so every user of the front end sees every flush that completed before its call. An error of
    /// `guest_wrote` is logged once and does not fail the caller. The same holds for the spans of the recordings that
    /// ended ([`Gpu::recorded_deferred`]): [`Front::recorded`] runs for them before anything else uses the front end.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Front> {
        let mut front = self.front.lock().unwrap_or_else(PoisonError::into_inner);
        if self.writes_dirty.swap(false, Ordering::Acquire) {
            let spans = std::mem::take(&mut *self.pending_recorded.lock().unwrap_or_else(PoisonError::into_inner));
            if !spans.is_empty() {
                front.recorded(&spans);
            }
            let ranges = std::mem::take(&mut *self.pending_writes.lock().unwrap_or_else(PoisonError::into_inner));
            for (start, len) in ranges {
                if let Err(e) = front.guest_wrote(start, len) {
                    log_once(
                        "nvnMemoryPoolFlushMappedRange",
                        format_args!("nvnMemoryPoolFlushMappedRange: {e}"),
                    );
                }
            }
        }
        front
    }

    /// Tells the backend that the guest wrote `len` bytes at the host address `start`, so that it drops what it kept of the
    /// recordings there. It takes no lock of the front end. Every write that the front end learns about goes through
    /// here, and so does the end of a recording ([`Front::recorded`]).
    pub(crate) fn invalidated(&self, start: u64, len: u64) {
        self.backend.recording_invalidated(start, start.saturating_add(len));
    }

    /// Tells the front end that a recording ended in `spans` of host memory, without taking its lock: the summaries it
    /// kept of recordings there are dropped by the next [`Gpu::lock`], before anything decodes. (A thread that ends a
    /// recording while the present holds the lock, 16 ms, no longer waits for it.)
    pub(crate) fn recorded_deferred(&self, spans: &[std::ops::Range<u64>]) {
        if spans.is_empty() {
            return;
        }
        self.pending_recorded.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(spans);
        self.writes_dirty.store(true, Ordering::Release);
    }

    /// Tells the front end that the guest wrote `len` bytes at the host address `start`, without taking its lock. The
    /// next [`Gpu::lock`] applies the range, in the order of the calls.
    pub(crate) fn guest_wrote_deferred(&self, start: u64, len: u64) {
        self.invalidated(start, len);
        self.pending_writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((start, len));
        self.writes_dirty.store(true, Ordering::Release);
    }

    /// Runs `f` on the front end under the lock. An error of the front end becomes an [`AbiError`](super::abi::AbiError).
    pub(crate) fn with_front<R>(&self, f: impl FnOnce(&mut Front) -> NvnResult<R>) -> AbiResult<R> {
        Ok(f(&mut self.lock())?)
    }

    /// The GPU address of `len` bytes at the guest address `host`.
    pub(crate) fn gpu_address(&self, host: u64, len: u64) -> AbiResult<u64> {
        Ok(self.space.gpu_address(host, len)?.get())
    }

    /// The host address of `len` bytes at the GPU address `gpu`.
    pub(crate) fn host_address(&self, gpu: u64, len: u64) -> AbiResult<u64> {
        let gpu = nxkit_nvn::space::GpuAddr::new(gpu)?;
        Ok(self.space.host_address(gpu, len)?)
    }

    /// The width and height of the texture at the guest address of an `NVNtexture`. None when the game did not
    /// initialise a texture there.
    pub(crate) fn texture_extent(&self, texture: u64) -> Option<(u32, u32)> {
        let id = self.textures.get(texture)?;
        let front = self.lock();
        let r = front.resources().get(id).ok()?;
        Some((r.desc.width, r.desc.height))
    }

    /// Ends the open frame and hands it to the backend, with the lock held by the caller. An empty frame ends nothing.
    /// The result is the fence of the work that was flushed, which is the last fence when the frame was empty.
    pub(crate) fn flush_locked(&self, front: &mut Front, reason: FlushReason) -> AbiResult<Fence> {
        if super::gateseq::active() {
            super::gateseq::note(format_args!(
                "FLUSH {reason:?} begins: {} cmds in the frame, held ops {}",
                front.frame().cmds.len(),
                self.gate().held.len()
            ));
        }
        // A release flush runs nothing that is still held: what waits for a signal stays held for the flush after.
        let (held, forced) = {
            let mut gate = self.gate();
            let held = if reason == FlushReason::Release {
                Ok(())
            } else {
                self.release_all(front, &mut gate)
            };
            self.gate_report(&mut gate, self.flushed() + 1);
            (held, std::mem::take(&mut gate.forced_waits))
        };
        if let Err(e) = held {
            log_once(
                "flush-held",
                format_args!("a held submit failed at the flush: {e}"),
            );
        }
        if front.frame_is_empty() {
            super::gateseq::note(format_args!("FLUSH {reason:?} found an empty frame"));
            return Ok(Fence(self.flushed()));
        }
        let fence = Fence(self.flushed() + 1);
        if super::gateseq::active() {
            super::gateseq::note(format_args!(
                "FLUSH {reason:?} sends fence {} with {} cmds (work {}), held ops {}",
                fence.0,
                front.frame().cmds.len(),
                front.frame_work(),
                self.gate().held.len()
            ));
        }
        super::flushstat::flush(fence.0, reason);
        front.end_frame(reason)?;
        if !forced.is_empty() {
            self.backend.forced_waits(fence, &forced);
        }
        let (frame, plan, table, descriptors) = front.flush_parts();
        let sent =
            self.backend
                .flush_frame(frame, plan, reason, fence, FlushCx { table, descriptors });
        front.begin_frame();
        self.flushed.store(fence.0, Ordering::Release);
        sent?;
        Ok(fence)
    }

    /// Ends the open frame. See [`Gpu::flush_locked`].
    pub(crate) fn flush(&self, reason: FlushReason) -> AbiResult<Fence> {
        let mut front = self.lock();
        self.flush_locked(&mut front, reason)
    }

    /// The fence of everything that the game submitted so far.
    pub(crate) fn fence_now(&self) -> Fence {
        let front = self.lock();
        let last = self.flushed();
        Fence(if front.frame_is_empty() && self.gate().is_empty() {
            last
        } else {
            last + 1
        })
    }

    /// Waits for `fence` for at most `timeout_ns` nanoseconds (`u64::MAX` for no limit). The open frame is flushed
    /// first when `fence` belongs to it. The lock is not held during the wait.
    pub(crate) fn wait(&self, fence: Fence, timeout_ns: u64) -> AbiResult<WaitResult> {
        super::gateseq::note(format_args!(
            "CPU WAIT fence {} (flushed {}), timeout {timeout_ns}",
            fence.0,
            self.flushed()
        ));
        super::gateseq::timed("CPU WAIT", || self.wait_in(fence, timeout_ns))
    }

    fn wait_in(&self, fence: Fence, timeout_ns: u64) -> AbiResult<WaitResult> {
        if fence.0 > self.flushed() {
            let mut front = self.lock();
            if fence.0 > self.flushed() {
                self.flush_locked(&mut front, FlushReason::CpuWait)?;
            }
        }
        Ok(self.backend.wait_fence(fence, timeout_ns))
    }

    /// `nvnQueueFinish`: flushes the open frame and waits until the GPU is idle.
    pub(crate) fn finish(&self) -> AbiResult<()> {
        self.flush(FlushReason::CpuWait)?;
        self.backend.wait_idle();
        Ok(())
    }

    /// Puts `handles` into the frame, in order on the queue at the guest address `queue` (`nvnQueueSubmitCommands`
    /// once the gate lets them run, see `order`). The queue keeps the state that the recordings leave. The backend
    /// sees the handles after the front end accepted all of them.
    fn run_submit(
        &self,
        front: &mut Front,
        queue: u64,
        handles: &[CommandHandle],
        origin_lower_left: bool,
    ) -> AbiResult<()> {
        // The backend's walk and the front end's decode read the same recordings and do not depend on each other, so the
        // walk runs on a helper thread while this one decodes. The walk ends before the front lock goes, so the next
        // flush finds it done, as when both ran here one after the other.
        let deferred = self.backend.submit(queue, handles, origin_lower_left);
        let qos = nxkit_hos::sys::current_qos();
        let failed = std::thread::scope(|s| {
            let walk = deferred.map(|run| {
                s.spawn(move || {
                    if let Some(q) = qos {
                        nxkit_hos::sys::set_current_qos(q);
                    }
                    run();
                })
            });
            let failed = super::frametime::sub(2, || {
                handles
                    .iter()
                    .find_map(|h| front.submit_on(queue as usize, *h).err())
            });
            if let Some(w) = walk
                && let Err(p) = super::frametime::sub(3, || w.join())
            {
                std::panic::resume_unwind(p);
            }
            failed
        });
        failed.map_or(Ok(()), |e| Err(e.into()))
    }

    /// `nvnQueuePresentTexture`: puts the present into the frame, flushes the frame and tells the backend.
    pub(crate) fn present(&self, present: Present) -> AbiResult<()> {
        super::gateseq::note(format_args!("PRESENT"));
        {
            let mut front = self.lock();
            let held = self.release_all(&mut front, &mut self.gate());
            if let Err(e) = held {
                log_once(
                    "present-held",
                    format_args!("a held submit failed at the present: {e}"),
                );
            }
            front.present(present.texture);
            super::frametime::sub(0, || self.flush_locked(&mut front, FlushReason::Present))?;
        }
        let r = super::frametime::sub(1, || self.backend.present(present));
        super::gateseq::end_frame();
        Ok(r?)
    }

    /// The number that the front end uses for the command buffer at the guest address `addr`. The same address gives the
    /// same number until [`Gpu::command_buffer_release`]. A new command buffer gets a number that is not in use.
    #[cfg(test)]
    pub(crate) fn command_buffer_index(&self, addr: u64) -> usize {
        if let Some(i) = self.cb_ids.get(addr) {
            return i;
        }
        let mut alloc = self.cb_alloc.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = self.cb_ids.get(addr) {
            return i;
        }
        let i = alloc.free.pop().unwrap_or_else(|| {
            alloc.next += 1;
            alloc.next - 1
        });
        self.cb_ids.insert(addr, i);
        i
    }

    /// Frees the number of the command buffer at `addr` and resets its state in the front end
    /// (`FrontEnd::release_command_buffer`). Nothing happens when `addr` has no number.
    #[cfg(test)]
    pub(crate) fn command_buffer_release(&self, addr: u64) {
        let Some(i) = self.cb_ids.remove(addr) else {
            return;
        };
        self.lock().release_command_buffer(i);
        self.cb_alloc
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .free
            .push(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_nvn::backend::{BackendResult, PoolInfo};
    use nxkit_nvn::frame::Frame;
    use nxkit_nvn::passes::PassPlan;
    use nxkit_nvn::resource::TextureRecord;
    use nxkit_nvn::tic::Tsc;

    #[derive(Default)]
    struct Spy {
        waits: Mutex<Vec<(Fence, u64)>>,
        idle: AtomicU64,
        /// The flushes that reached the backend: reason, fence and commands of the frame.
        flushes: Mutex<Vec<(FlushReason, Fence, usize)>>,
    }

    impl Backend for Spy {
        fn gpu_base(&self) -> u64 {
            0x10_0000_0000
        }
        fn register_pool(&self, _: PoolInfo) -> BackendResult<()> {
            Ok(())
        }
        fn unregister_pool(&self, _: u64) {}
        fn flush_frame(
            &self,
            frame: &Frame,
            _: &PassPlan,
            reason: FlushReason,
            fence: Fence,
            _: FlushCx<'_>,
        ) -> BackendResult<()> {
            if let Ok(mut f) = self.flushes.lock() {
                f.push((reason, fence, frame.cmds.len()));
            }
            Ok(())
        }
        fn present(&self, _: Present) -> BackendResult<()> {
            Ok(())
        }
        fn wait_fence(&self, fence: Fence, timeout_ns: u64) -> WaitResult {
            if let Ok(mut w) = self.waits.lock() {
                w.push((fence, timeout_ns));
            }
            WaitResult::Done
        }
        fn wait_idle(&self) {
            self.idle.fetch_add(1, Ordering::Relaxed);
        }
        fn texture_created(&self, _: &TextureRecord) {}
        fn texture_finalized(&self, _: TexId) {}
        fn sampler_registered(&self, _: u32, _: &Tsc) {}
        fn sampler_released(&self, _: u32) {}
    }

    fn gpu_with(spy: Arc<Spy>) -> Gpu {
        let space = LinearSpace::new(0x1000_0000, 0x10_0000_0000, 0x10_0000).expect("space");
        Gpu::new(space, spy, Settings::default())
    }

    #[test]
    fn an_empty_frame_has_the_last_fence_and_flushes_nothing() {
        let spy = Arc::new(Spy::default());
        let g = gpu_with(spy.clone());
        assert_eq!(g.fence_now(), Fence(0));
        assert_eq!(g.flush(FlushReason::CpuWait).expect("flush"), Fence(0));
        assert_eq!(g.flushed(), 0);
    }

    #[test]
    fn a_wait_on_a_done_fence_goes_to_the_backend_with_its_timeout() {
        let spy = Arc::new(Spy::default());
        let g = gpu_with(spy.clone());
        assert_eq!(g.wait(Fence(0), 7).expect("wait"), WaitResult::Done);
        assert_eq!(spy.waits.lock().expect("lock").as_slice(), &[(Fence(0), 7)]);
    }

    #[test]
    fn the_end_of_a_recording_takes_no_lock_and_the_next_lock_applies_it() {
        let g = gpu_with(Arc::new(Spy::default()));
        let held = g.lock();
        // Another thread ends a recording while the present holds the lock: it returns at once.
        std::thread::scope(|s| {
            s.spawn(|| g.recorded_deferred(&[0x1000..0x2000, 0x8000..0x9000])).join().expect("thread");
        });
        assert_eq!(g.pending_recorded.lock().expect("lock").len(), 2);
        drop(held);
        drop(g.lock());
        assert!(g.pending_recorded.lock().expect("lock").is_empty(), "the lock applied the spans");
    }

    #[test]
    fn finish_waits_for_idle() {
        let spy = Arc::new(Spy::default());
        let g = gpu_with(spy.clone());
        g.finish().expect("finish");
        assert_eq!(spy.idle.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn command_buffer_numbers_are_stable_and_reused() {
        let g = gpu_with(Arc::new(Spy::default()));
        let a = g.command_buffer_index(0x1000);
        let b = g.command_buffer_index(0x2000);
        assert_ne!(a, b);
        assert_eq!(g.command_buffer_index(0x1000), a);
        g.command_buffer_release(0x1000);
        assert_eq!(g.command_buffer_index(0x3000), a);
    }

    #[test]
    fn the_default_settings_are_api_55_15() {
        let s = Settings::default();
        assert_eq!((s.api_major, s.api_minor), (55, 15));
        assert!(s.shader_key_skip.is_none());
        let t = (s.clock_ns)();
        assert!((s.clock_ns)() >= t);
    }

    use nxkit_nvn::frame::Cmd;

    fn work(g: &Gpu, n: usize) {
        g.lock().frame_mut().cmds.extend((0..n).map(|_| Cmd::BufferCopy));
    }

    fn sent(spy: &Spy) -> Vec<(FlushReason, u64, usize)> {
        let f = spy.flushes.lock().expect("lock");
        f.iter().map(|(r, f, n)| (*r, f.0, *n)).collect()
    }

    #[test]
    fn a_release_that_brought_a_lot_of_work_flushes_the_frame_and_the_fences_follow() {
        let spy = Arc::new(Spy::default());
        let mut g = gpu_with(spy.clone());
        g.release_flush = order::ReleaseFlush { on: true, min_work: 4 };
        // Two commands were in the frame before the release; five came with it.
        work(&g, 2);
        let from = g.lock().frame().cmds.len();
        work(&g, 5);
        g.flush_released(&mut g.lock(), from).expect("flush");
        assert_eq!(sent(&spy), vec![(FlushReason::Release, 1, 7)]);
        assert_eq!(g.flushed(), 1);
        assert!(g.lock().frame_is_empty());
        // The work of the game after the release, and the present, are the next fence.
        assert_eq!(g.fence_now(), Fence(1));
        work(&g, 1);
        assert_eq!(g.fence_now(), Fence(2));
        assert_eq!(g.flush(FlushReason::Present).expect("present flush"), Fence(2));
        assert_eq!(
            sent(&spy),
            vec![(FlushReason::Release, 1, 7), (FlushReason::Present, 2, 1)]
        );
        let stats = g.lock().flushes();
        assert_eq!((stats.release, stats.present), (1, 1));
    }

    #[test]
    fn a_release_with_little_work_or_with_the_setting_off_flushes_nothing() {
        let spy = Arc::new(Spy::default());
        let mut g = gpu_with(spy.clone());
        g.release_flush = order::ReleaseFlush { on: true, min_work: 4 };
        work(&g, 3);
        g.flush_released(&mut g.lock(), 0).expect("flush");
        // The work before the release does not count: only the 2 commands that came with it.
        work(&g, 2);
        g.flush_released(&mut g.lock(), 3).expect("flush");
        g.release_flush.on = false;
        work(&g, 10);
        g.flush_released(&mut g.lock(), 5).expect("flush");
        assert!(sent(&spy).is_empty());
        assert_eq!(g.fence_now(), Fence(1));
    }

    #[test]
    fn a_release_flush_leaves_the_held_ops_held() {
        let spy = Arc::new(Spy::default());
        let mut g = gpu_with(spy.clone());
        g.release_flush = order::ReleaseFlush { on: true, min_work: 1 };
        work(&g, 3);
        g.gate().push(
            0xa,
            gate::Op::Submit {
                handles: vec![CommandHandle(1)],
                origin: false,
            },
        );
        g.flush_released(&mut g.lock(), 0).expect("flush");
        assert_eq!(sent(&spy), vec![(FlushReason::Release, 1, 3)]);
        assert_eq!(g.gate().held.len(), 1, "the op that waits stays held");
        // Held work is work of the next flush: its fence is the next number.
        assert_eq!(g.fence_now(), Fence(2));
    }
}
