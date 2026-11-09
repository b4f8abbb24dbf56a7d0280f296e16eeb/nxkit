//! The public entry of the crate: [`Gpu`] turns a held NVN frame into Metal 4 work.
//!
//! One call of [`Gpu::encode_frame`] does these steps in order.
//!
//! 1. It finds out how each texture is used (attachment, memoryless, CPU read) and makes the Metal textures at the
//!    frame scale.
//! 2. It resolves the texture handles of all draws and dispatches into resource IDs and fills the pool tables.
//! 3. It encodes the uploads of the textures that the guest wrote into the first command buffer.
//! 4. It plans the barriers over the whole frame.
//! 5. It cuts the passes into runs and encodes each run into its own command buffer on its own thread.
//! 6. It commits the command buffers in order.

use crate::draws::{FastMap, FastSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use nxkit_metal::{
    ArchiveConfig, BatchRing, CommandBuffer, CommandBufferStats, Context, DataBuffer, GobOrder,
    GuestMemory, MetalError, Metrics, Result, Scratch, Stages, SubmittedBatch, Texture,
};
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::{PassKind, PassPlan};
use nxkit_nvn::resource::TexId;

use crate::barriers::{BarrierState, touch};
use crate::bindless::{Bindless, Env, Handles, Resolved};
use crate::copyback::CopyBack;
use crate::detail::{DrawArgs, FrameDetail, RecordingRun};
use crate::draws::{Shared, ordinals};
use crate::encode::{Tables, render_pass};
use crate::icbs::Icbs;
use crate::jobs;
use crate::passes::{compute_pass, cpu_read};
use crate::pipelines::Pipelines;
use crate::programs::Programs;
use crate::snapshot::EncodeView;
use crate::stats::{EncodeReport, EncodeStats, Skip};
use crate::textures::{Res, TextureStore, Use};
use crate::upload::{Dirty, Transfer};

mod diag;
mod frame;
mod handles;
mod timing;
mod uses;

use diag::*;
use timing::*;
use uses::*;

pub(crate) use uses::collect_uses;

/// The chunks of passes that the encoder makes for each thread (see `jobs::run_all`).
const CHUNKS_PER_THREAD: usize = 1;

/// What the caller chooses when it makes a [`Gpu`].
#[derive(Clone, Debug)]
pub struct GpuConfig {
    /// The directory with the Metal libraries and the `_b.json` files that the shader step wrote.
    pub shader_dir: PathBuf,
    /// The number of threads that encode passes. One means the calling thread alone.
    pub threads: usize,
    /// Where the pipeline cache loads an archive from, and whether it collects pipelines for one.
    pub archive: ArchiveConfig,
    /// The order of the 16 byte pieces in a GOB of the guest textures.
    pub gob_order: GobOrder,
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            shader_dir: PathBuf::from("shaders"),
            threads: std::thread::available_parallelism()
                .map_or(2, |n| n.get())
                .min(6),
            archive: ArchiveConfig::default(),
            gob_order: GobOrder::default(),
        }
    }
}

/// The objects that every command buffer of a frame keeps alive until the GPU is done with it.
type Keep = (
    Vec<Texture>,
    Arc<DataBuffer>,
    Arc<DataBuffer>,
    Option<Arc<DataBuffer>>,
);

/// The frame encoder.
pub struct Gpu {
    ctx: Context,
    guest: Arc<GuestMemory>,
    programs: Programs,
    /// The texture types that each shader declares for its texture bindings, by program key (fragment and compute
    /// first, vertex second), for [`Gpu::fit_handles`].
    fit_decl: [FastMap<String, Vec<(u32, nxkit_metal::TextureKind)>>; 2],
    /// The `img` members of the shaders by key, for [`crate::images`].
    img_decl: crate::images::Decls,
    /// The memory pools and the raw global memory table (`gmem.rs`).
    gmem: crate::gmem::Gmem,
    /// `NXKIT_DIAG=imgdump`: the textures that dispatches read and write in a sampled frame, read back a few frames
    /// later, when the GPU is done with them.
    imgdump: std::collections::VecDeque<(u64, Vec<ImgDumpEntry>)>,
    frames_encoded: u64,
    pipelines: Pipelines,
    bindless: Bindless,
    store: TextureStore,
    transfer: Transfer,
    icbs: Mutex<Icbs>,
    barriers: BarrierState,
    ring: BatchRing,
    threads: usize,
    last: Option<SubmittedBatch>,
    /// The batch before `last`, while it may still run: the frame that `wait_older` waits for.
    older: Option<SubmittedBatch>,
    pools: Vec<(u64, u64)>,
    copyback: CopyBack,
    native: crate::native::Native,
}

impl Gpu {
    /// Makes the encoder with the default configuration. The shader directory is `shaders` in the working directory.
    pub fn new(ctx: &Context, guest: Arc<GuestMemory>) -> Result<Self> {
        Self::with_config(ctx, guest, GpuConfig::default())
    }

    /// Makes the encoder.
    pub fn with_config(ctx: &Context, guest: Arc<GuestMemory>, config: GpuConfig) -> Result<Self> {
        let threads = config.threads.max(1);
        let pipelines = Pipelines::new(ctx, &config.archive)?;
        let transfer = Transfer::new(ctx, pipelines.cache(), config.gob_order)?;
        Ok(Self {
            ctx: ctx.clone(),
            guest,
            programs: Programs::new(ctx, config.shader_dir),
            fit_decl: [FastMap::default(), FastMap::default()],
            img_decl: [FastMap::default(), FastMap::default()],
            gmem: crate::gmem::Gmem::new(ctx)?,
            imgdump: std::collections::VecDeque::new(),
            frames_encoded: 0,
            pipelines,
            bindless: Bindless::new(ctx)?,
            store: TextureStore::new(ctx),
            transfer,
            icbs: Mutex::new(Icbs::default()),
            barriers: BarrierState::default(),
            ring: BatchRing::new(ctx, threads * CHUNKS_PER_THREAD + 1)?,
            threads,
            last: None,
            older: None,
            pools: Vec::new(),
            copyback: CopyBack::default(),
            native: crate::native::Native::new(),
        })
    }

    /// Tells the encoder about a texture or sampler pool that the game made. The pool memory is part of the guest
    /// memory, so nothing is copied. The call checks that the range is inside it.
    pub fn register_pool(&mut self, gpu_addr: u64, len: u64) -> Result<()> {
        self.guest.address_map().host_address(gpu_addr, len)?;
        if !self.pools.contains(&(gpu_addr, len)) {
            self.pools.push((gpu_addr, len));
        }
        Ok(())
    }

    /// Tells the encoder about a memory pool of the game and its `NVNmemoryPoolFlags`, like
    /// [`Gpu::register_pool`]. A texture that a frame writes and whose storage lies in a pool the CPU can access is
    /// copied back into guest memory at the end of the frame (see `copyback`).
    pub fn register_memory_pool(&mut self, gpu_addr: u64, len: u64, flags: u32) -> Result<()> {
        self.register_pool(gpu_addr, len)?;
        self.gmem.add_pool(gpu_addr, len);
        self.copyback.add_pool(gpu_addr, len, flags);
        Ok(())
    }

    /// The game finalised the memory pool at `gpu_addr`. Its textures are no longer copied back.
    pub fn unregister_memory_pool(&mut self, gpu_addr: u64) {
        self.copyback.remove_pool(gpu_addr);
        self.gmem.remove_pool(gpu_addr);
    }

    /// The pools that shaders wrote through raw global memory pointers, as the write logs of finished batches showed
    /// (guest GPU address ranges, a whole pool each). The host does not mark textures from it: a pool is far larger
    /// than any texture in it, and the logs do not say where in the pool the stores landed.
    pub fn gmem_written_pools(&self) -> &[(u64, u64)] {
        self.gmem.learned()
    }

    /// The pools that were registered, as (GPU address, length).
    pub fn pools(&self) -> &[(u64, u64)] {
        &self.pools
    }

    /// The game created or finalized a texture. The Metal texture of the id, if any, is stale and made again at the
    /// next use.
    pub fn texture_created(&mut self, id: TexId) {
        self.store.invalidate(id);
        self.barriers.forget(id);
        drop(self.store.take_retired());
    }

    /// The game destroyed a texture.
    pub fn texture_destroyed(&mut self, id: TexId) {
        self.texture_created(id);
    }

    /// Names the recordings that `RecCache::icb_candidates` found. Only runs of these recordings get an ICB.
    pub fn set_icb_candidates(&self, handles: impl IntoIterator<Item = u64>) {
        self.icbs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_candidates(handles);
    }

    /// The cache dropped the summary of a recording, so its ICB is built again at the next use.
    pub fn recording_dropped(&self, handle: u64) {
        self.icbs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drop_recording(handle);
    }

    /// The number of ICBs that are alive.
    pub fn icb_count(&self) -> usize {
        self.icbs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Forgets the programs and the ICBs, for example after the shader directory changed. The pipelines stay.
    pub fn reload_shaders(&self) {
        self.programs.clear();
        self.icbs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// The counters of the command buffers: CPU encode time and GPU time (R16).
    pub fn metrics(&self) -> Arc<Metrics> {
        self.ring.metrics()
    }

    /// The Metal texture that holds a texture of the game, as the last frame left it. None when no frame used it.
    pub fn texture(&self, id: TexId) -> Option<Texture> {
        self.store.get(id).map(|r| r.tex.clone())
    }

    /// The newest batch that went to the GPU, to wait for it without this lock.
    pub fn last_submitted(&self) -> Option<SubmittedBatch> {
        self.last.clone()
    }

    /// Waits until the GPU finished the batch before the newest one, and leaves the newest one running.
    pub fn wait_older(&mut self) -> Result<Option<CommandBufferStats>> {
        self.older.take().map(|b| b.wait()).transpose()
    }

    /// Waits until the GPU finished the last submitted frame. The result is `None` when nothing was submitted.
    pub fn wait(&mut self) -> Result<Option<CommandBufferStats>> {
        let r = self.last.take().map(|b| b.wait()).transpose();
        if r.is_ok() {
            self.copyback.idle();
        }
        r
    }
}
