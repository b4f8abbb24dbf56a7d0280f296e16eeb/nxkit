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

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

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
    /// The guest clock in nanoseconds, for `nvnDeviceGetCurrentTimestampInNanoseconds`. The default is a monotonic
    /// clock that starts at zero at the first call.
    pub clock_ns: fn() -> u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_major: 55,
            api_minor: 15,
            shader_key_skip: None,
            gpu_code: (1, 16),
            clock_ns: monotonic_ns,
        }
    }
}

fn monotonic_ns() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
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
    super::wire::wire();
    Ok(())
}

/// The installed context. A call before [`install`] ends the game with the name of the function.
pub(crate) fn gpu(name: &str) -> &'static Gpu {
    GPU.get()
        .unwrap_or_else(|| fatal(name, "no GPU backend is installed"))
}

/// The settings of the installed context. None before [`install`]. The SDK take-overs of `gfx` read the API version
/// with it and must not end the game when no context exists.
pub(crate) fn settings_if_installed() -> Option<&'static Settings> {
    GPU.get().map(Gpu::settings)
}

/// The command buffer numbers: a number for each live guest `NVNcommandBuffer`, with the numbers of released ones to reuse.
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
    /// The ranges of `nvnMemoryPoolFlushMappedRange` that [`Gpu::lock`] has not yet told the front end.
    pending_writes: Mutex<Vec<(u64, u64)>>,
    /// True when `pending_writes` may hold a range. Keeps the common case of [`Gpu::lock`] to one atomic load.
    writes_dirty: AtomicBool,
    /// The guest address of an initialised `NVNtexture` to its id in the resource table. The texture group maintains it.
    pub(crate) textures: Registry<TexId>,
    /// The guest address of an initialised `NVNprogram` to its stages. The program group maintains it.
    pub(crate) programs: Registry<ProgramRecord>,
    cb_ids: Registry<usize>,
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
            pending_writes: Mutex::new(Vec::new()),
            writes_dirty: AtomicBool::new(false),
            textures: Registry::new(),
            programs: Registry::new(),
            cb_ids: Registry::new(),
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
    /// `guest_wrote` is logged once and does not fail the caller.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Front> {
        let mut front = self.front.lock().unwrap_or_else(PoisonError::into_inner);
        if self.writes_dirty.swap(false, Ordering::Acquire) {
            let ranges = std::mem::take(
                &mut *self
                    .pending_writes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
            );
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
        if front.frame_is_empty() {
            return Ok(Fence(self.flushed()));
        }
        let fence = Fence(self.flushed() + 1);
        front.end_frame(reason)?;
        let (frame, plan, table, descriptors) = front.flush_parts();
        let sent = self.backend.flush_frame(
            frame,
            plan,
            reason,
            fence,
            FlushCx { table, descriptors },
        );
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
        Fence(if front.frame_is_empty() {
            last
        } else {
            last + 1
        })
    }

    /// Waits for `fence` for at most `timeout_ns` nanoseconds (`u64::MAX` for no limit). The open frame is flushed
    /// first when `fence` belongs to it. The lock is not held during the wait.
    pub(crate) fn wait(&self, fence: Fence, timeout_ns: u64) -> AbiResult<WaitResult> {
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

    /// `nvnQueueSubmitCommands`: submits `handles` in order on the queue at the guest address `queue`. The queue keeps
    /// the state that the recordings leave. The backend sees the handles after the front end accepted all of them. The
    /// backend claims its order under the lock and its deferred work (the walk) runs after the lock is released.
    pub(crate) fn submit(
        &self,
        queue: u64,
        handles: &[CommandHandle],
        origin_lower_left: bool,
    ) -> AbiResult<()> {
        let mut front = self.lock();
        let mut accepted = 0;
        let mut failed = None;
        for h in handles {
            if let Err(e) = front.submit_on(queue as usize, *h) {
                failed = Some(e);
                break;
            }
            accepted += 1;
        }
        let deferred = self
            .backend
            .submit(queue, &handles[..accepted], origin_lower_left);
        drop(front);
        if let Some(run) = deferred {
            run();
        }
        failed.map_or(Ok(()), |e| Err(e.into()))
    }

    /// `nvnQueuePresentTexture`: puts the present into the frame, flushes the frame and tells the backend.
    pub(crate) fn present(&self, present: Present) -> AbiResult<()> {
        {
            let mut front = self.lock();
            front.present(present.texture);
            self.flush_locked(&mut front, FlushReason::Present)?;
        }
        Ok(self.backend.present(present)?)
    }

    /// The number that the front end uses for the command buffer at the guest address `addr`. The same address gives the
    /// same number until [`Gpu::command_buffer_release`]. A new command buffer gets a number that is not in use.
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
            _: &Frame,
            _: &PassPlan,
            _: FlushReason,
            _: Fence,
            _: FlushCx<'_>,
        ) -> BackendResult<()> {
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
}
