//! [`VkGpu`]: the backend of the Vulkan front end on Metal 4 and the sink of the display ticks.
//!
//! - Objects: [`nxkit_vk::Backend::create_memory`] maps memory of its own below `1 << 39` and wraps it as a no-copy
//!   buffer (`GuestMemory::map_below`): Vulkan 1.1 gives the game no GPU addresses, the game only maps the memory, and
//!   the host address is the guest address (R18). Images, views, samplers and pipelines are made at once (`make`).
//! - Submits (`submit`): one command buffer per submit from a ring of two in flight. The replay resolves the ids
//!   while it encodes, and the command buffer keeps what it encodes alive. Its completion advances the completed
//!   serial in order and writes the query results.
//! - Present (`present`, `tick`): a present queues the swapchain image for the [`Pacer`]. A display tick blits it into
//!   the drawable at the tick's target time. `acquire` waits while [`MAX_PENDING`] presents wait for a tick, and until
//!   the image's previous present has been shown.

mod api;
mod present;
mod submit;
mod tick;

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use nxkit_metal::{ArchiveConfig, Context, FrameRing, GuestMemory, Pacer, PipelineCache, Texture};
use nxkit_vk::backend::BackendError;
use nxkit_vk::desc::{MemoryId, QueryPoolId, TextureId};

use crate::blit::Blit;
use crate::log::OnceLog;
use crate::objects::Objects;
use crate::replay::{Stats, Tables};
use crate::settings::VkGpuSettings;
use crate::table::ShaderTable;

/// The presents that may wait for a display tick.
pub const MAX_PENDING: usize = 3;

/// One wait of `acquire` before it gives up on a tick and drops the oldest present.
const ACQUIRE_WAIT: Duration = Duration::from_millis(100);

/// The end of the address space that the game's pointers must fit (the Switch's 39 bits).
const ADDRESS_LIMIT: u64 = 1 << 39;

/// Where `create_memory` asks for memory first: the system's choice, then two hints low in the 39-bit space.
const MEMORY_HINTS: [u64; 3] = [0, 0x10_0000_0000, 0x40_0000_0000];

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The completed serials. The GPU completes the submits in order, but their handlers may run in any order, and a
/// submit that failed to encode completes at once: a serial counts as completed only when every earlier one has.
#[derive(Default)]
pub(crate) struct Done {
    completed: AtomicU64,
    early: Mutex<BTreeSet<u64>>,
    cv: Condvar,
}

impl Done {
    /// Marks `serial` complete.
    pub(crate) fn finish(&self, serial: u64) {
        let mut early = locked(&self.early);
        early.insert(serial);
        let mut c = self.completed.load(Ordering::Acquire);
        while early.remove(&(c + 1)) {
            c += 1;
        }
        early.retain(|&s| s > c);
        self.completed.store(c, Ordering::Release);
        drop(early);
        self.cv.notify_all();
    }

    /// The highest serial that has completed with all before it.
    pub(crate) fn completed(&self) -> u64 {
        self.completed.load(Ordering::Acquire)
    }

    /// Waits until `serial` has completed or `timeout_ns` has passed.
    pub(crate) fn wait(&self, serial: u64, timeout_ns: u64) -> bool {
        if self.completed() >= serial {
            return true;
        }
        let deadline = Instant::now().checked_add(Duration::from_nanos(timeout_ns));
        let mut early = locked(&self.early);
        while self.completed() < serial {
            let left = match deadline {
                Some(d) => match d.checked_duration_since(Instant::now()) {
                    Some(l) if !l.is_zero() => l,
                    _ => return false,
                },
                None => Duration::from_secs(1),
            };
            early = self
                .cv
                .wait_timeout(early, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

/// The submit side: the frame ring, the argument tables and the last committed serial. One submit encodes at a time.
pub(crate) struct Queue {
    pub(crate) ring: FrameRing,
    pub(crate) tables: Tables,
    pub(crate) committed: u64,
}

/// A present that waits for a display tick.
pub(crate) struct Pending {
    pub(crate) number: u64,
    pub(crate) texture: Texture,
}

/// The presents of the game.
#[derive(Default)]
pub(crate) struct Presents {
    /// The presents that wait for a tick, oldest first.
    pub(crate) pending: Vec<Pending>,
    /// The number of the last present of each swapchain image.
    pub(crate) last: HashMap<TextureId, u64>,
    /// The newest present that was shown or dropped.
    pub(crate) shown_upto: u64,
    /// The presents so far.
    pub(crate) presented: u64,
}

/// The backend. Make it with [`VkGpu::new`]. The host installs it as the `Backend` of `nxkit_vk::Vk` and as the
/// `TickSink` of its pacer.
pub struct VkGpu {
    ctx: Context,
    settings: VkGpuSettings,
    shaders: ShaderTable,
    cache: PipelineCache,
    objects: Mutex<Objects>,
    queue: Mutex<Queue>,
    done: Arc<Done>,
    /// Memory that the game freed while a committed submit may still use it, with the serial to wait for.
    retired: Mutex<Vec<(u64, Arc<GuestMemory>)>>,
    /// The values of the query pools.
    queries: Arc<Mutex<HashMap<QueryPoolId, Vec<u64>>>>,
    presents: Mutex<Presents>,
    freed: Condvar,
    blit: Mutex<Option<Blit>>,
    blit_failed: AtomicBool,
    pacer: OnceLock<Arc<Pacer>>,
    log: OnceLog,
    /// The start of the timestamps.
    epoch: Instant,
    /// The counters since the start: draws, skipped draws, passes, copies, dispatches, submits.
    totals: Mutex<(Stats, u64)>,
}

impl VkGpu {
    /// Makes the backend. The shader table and library come from `settings.shader_dir`. When they are missing, the
    /// backend still runs and every pipeline fails with the reason.
    pub fn new(ctx: &Context, settings: VkGpuSettings) -> Result<Arc<Self>, BackendError> {
        let shaders = ShaderTable::load(ctx, &settings.shader_dir);
        Self::with_shaders(ctx, shaders, settings)
    }

    /// Makes the backend over a shader table that the caller made (a test compiles its own library).
    pub fn with_shaders(
        ctx: &Context,
        shaders: ShaderTable,
        settings: VkGpuSettings,
    ) -> Result<Arc<Self>, BackendError> {
        let e = |what: &str, err: nxkit_metal::MetalError| BackendError(format!("{what}: {err}"));
        let cache = PipelineCache::new(ctx, &ArchiveConfig::default())
            .map_err(|x| e("pipeline cache", x))?;
        let ring = FrameRing::new(ctx).map_err(|x| e("frame ring", x))?;
        let tables = Tables::new(ctx).map_err(|x| e("argument tables", x))?;
        Ok(Arc::new(Self {
            ctx: ctx.clone(),
            settings,
            shaders,
            cache,
            objects: Mutex::new(Objects::default()),
            queue: Mutex::new(Queue {
                ring,
                tables,
                committed: 0,
            }),
            done: Arc::new(Done::default()),
            retired: Mutex::new(Vec::new()),
            queries: Arc::new(Mutex::new(HashMap::new())),
            presents: Mutex::new(Presents::default()),
            freed: Condvar::new(),
            blit: Mutex::new(None),
            blit_failed: AtomicBool::new(false),
            pacer: OnceLock::new(),
            log: OnceLog::default(),
            epoch: Instant::now(),
            totals: Mutex::new((Stats::default(), 0)),
        }))
    }

    /// Sets the pacer that learns about each present. It is set once. Without one a present counts as shown at once.
    pub fn set_pacer(&self, pacer: Arc<Pacer>) {
        let _ = self.pacer.set(pacer);
    }

    /// Copies bytes out of the storage of a `VkDeviceMemory`, for diagnostics and tests. Gives false when the memory
    /// has no storage or the range is outside it. The caller orders it after the GPU work that wrote the bytes.
    pub fn read_memory(&self, id: MemoryId, offset: u64, out: &mut [u8]) -> bool {
        let mem = locked(&self.objects).memories.get(&id).cloned();
        mem.is_some_and(|m| m.read(offset, out).is_ok())
    }

    /// Frees the memory that the game freed once the GPU is done with the submits that may use it. The drop commits the
    /// residency set, so it runs here and never in a completion handler.
    fn drop_retired(&self) {
        let done = self.done.completed();
        let freed: Vec<_> = {
            let mut r = locked(&self.retired);
            let (old, keep): (Vec<_>, Vec<_>) = r.drain(..).partition(|(s, _)| *s <= done);
            *r = keep;
            old
        };
        drop(freed);
    }
}
