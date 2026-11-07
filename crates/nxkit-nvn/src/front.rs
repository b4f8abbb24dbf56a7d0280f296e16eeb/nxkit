//! The front end: the one object that the NVN entry points of the host call.
//!
//! The front end owns the resource table, the descriptor pools, the command buffers, the cache of recordings and the frame.
//! It turns guest addresses into GPU addresses through a [`GpuSpace`] and rejects addresses outside the mapping.
//! A command buffer writes its records into the command memory of the game, through [`GuestMem`].
//! A submit gives the summary of the recording under the state of the queue and appends its effects to the frame.
//! The frame is held until a present or until the CPU waits for the GPU. [`FrontEnd::end_frame`] counts the reason,
//! runs the pass analysis and keeps the frame for the backend. The next [`FrontEnd::begin_frame`] drops the frame
//! and keeps its memory.

use crate::analyse::Analyser;
use crate::cache::{CacheStats, RecCache};
use crate::cmdbuf::{CommandBuffer, CommandHandle};
use crate::decode::{Ctx, State, summary};
use crate::descriptors::{Descriptors, TextureSlot};
use crate::error::{NvnError, Result};
use crate::frame::Frame;
use crate::layout::TextureDesc;
use crate::mem::GuestMem;
use crate::passes::PassPlan;
use crate::record::Record;
use crate::resource::{ResourceTable, TexId};
use crate::space::{GpuAddr, GpuSpace};
use crate::tic::{TextureView, Tsc};

/// Why a frame ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlushReason {
    /// The game presented.
    Present,
    /// The CPU waits for the GPU, for a fence, a finish or a read back.
    CpuWait,
}

/// How often each reason ended a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushStats {
    /// Frames that ended at a present.
    pub present: u64,
    /// Frames that a CPU wait ended early.
    pub cpu_wait: u64,
}

/// The NVN front end.
#[derive(Debug)]
pub struct FrontEnd<S: GpuSpace, M: GuestMem> {
    space: S,
    mem: M,
    table: ResourceTable,
    descriptors: Descriptors,
    buffers: Vec<CommandBuffer>,
    cache: RecCache,
    frame: Frame,
    plan: PassPlan,
    analyser: Analyser,
    flushes: FlushStats,
    hit: Vec<TexId>,
}

impl<S: GpuSpace, M: GuestMem> FrontEnd<S, M> {
    /// A front end over a guest memory mapping.
    pub fn new(space: S, mem: M) -> Self {
        Self {
            space,
            mem,
            table: ResourceTable::new(),
            descriptors: Descriptors::new(),
            buffers: Vec::new(),
            cache: RecCache::new(),
            frame: Frame::new(),
            plan: PassPlan::new(),
            analyser: Analyser::new(),
            flushes: FlushStats::default(),
            hit: Vec::new(),
        }
    }

    /// The address space.
    pub fn space(&self) -> &S {
        &self.space
    }

    /// The guest memory.
    pub fn mem(&self) -> &M {
        &self.mem
    }

    /// The textures.
    pub fn resources(&self) -> &ResourceTable {
        &self.table
    }

    /// The textures, for the host to set flags.
    pub fn resources_mut(&mut self) -> &mut ResourceTable {
        &mut self.table
    }

    /// The descriptor pools.
    pub fn descriptors(&self) -> &Descriptors {
        &self.descriptors
    }

    /// The cache of recordings.
    pub fn cache(&self) -> &RecCache {
        &self.cache
    }

    /// The counters of the frame flushes.
    pub fn flushes(&self) -> FlushStats {
        self.flushes
    }

    /// The counters of the recording cache.
    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats
    }

    /// The GPU address of guest memory. This is `nvnMemoryPoolGetBufferAddress` and its relatives.
    pub fn gpu_address(&self, host: u64, len: u64) -> Result<GpuAddr> {
        self.space.gpu_address(host, len)
    }

    /// The GPU address of a texture. A record in command memory names a texture by this address.
    pub fn texture_address(&self, id: TexId) -> Result<u64> {
        Ok(self.table.get(id)?.address.get())
    }

    /// Creates a texture in guest memory. This is `nvnTextureInitialize`.
    pub fn create_texture(&mut self, desc: TextureDesc, host_address: u64) -> Result<TexId> {
        let size = desc.layout()?.size;
        let address = self.space.gpu_address(host_address, size)?;
        self.table.create(desc, address)
    }

    /// Destroys a texture. This is `nvnTextureFinalize`.
    pub fn finalize_texture(&mut self, id: TexId) -> Result<()> {
        self.table.finalize(id)?;
        self.descriptors.drop_texture(id);
        Ok(())
    }

    /// Creates a texture pool. This is `nvnTexturePoolInitialize`.
    pub fn add_texture_pool(&mut self, base: u32, count: u32, host_memory: u64) -> Result<()> {
        let bytes = u64::from(count) * crate::tic::HEADER_BYTES as u64;
        let memory = self.space.gpu_address(host_memory, bytes)?;
        self.descriptors.add_texture_pool(base, count, memory)
    }

    /// Creates a sampler pool. This is `nvnSamplerPoolInitialize`.
    pub fn add_sampler_pool(&mut self, base: u32, count: u32, host_memory: u64) -> Result<()> {
        let bytes = u64::from(count) * crate::tic::HEADER_BYTES as u64;
        let memory = self.space.gpu_address(host_memory, bytes)?;
        self.descriptors.add_sampler_pool(base, count, memory)
    }

    /// Registers a texture. This is `nvnTexturePoolRegisterTexture`.
    /// A recording that bound the old texture of this id is decoded again at its next submit.
    pub fn register_texture(
        &mut self,
        id: u32,
        texture: TexId,
        view: TextureView,
    ) -> Result<TextureSlot> {
        self.descriptors
            .register_texture(&self.table, id, texture, view)
    }

    /// Registers a sampler. This is `nvnSamplerPoolRegisterSampler`.
    pub fn register_sampler(&mut self, id: u32, sampler: Tsc) -> Result<()> {
        self.descriptors.register_sampler(id, sampler)
    }

    fn buffer(&mut self, cb: usize) -> &mut CommandBuffer {
        if self.buffers.len() <= cb {
            self.buffers.resize_with(cb + 1, CommandBuffer::new);
        }
        &mut self.buffers[cb]
    }

    /// `nvnCommandBufferAddCommandMemory`. The block is at a host address.
    pub fn add_command_memory(&mut self, cb: usize, host: u64, len: u64) -> Result<()> {
        self.space.gpu_address(host, len)?;
        self.buffer(cb).add_command_memory(host, len)
    }

    /// `nvnCommandBufferBeginRecording`.
    pub fn begin_recording(&mut self, cb: usize) -> Result<()> {
        self.buffer(cb).begin_recording()
    }

    /// Writes a record. [`NvnError::OutOfCommandMemory`] tells the host to call the memory callback
    /// of the game, to add memory and to write the record again.
    pub fn write(&mut self, cb: usize, rec: Record) -> Result<()> {
        self.buffer(cb);
        self.buffers[cb].write(&mut self.mem, rec)
    }

    /// `nvnCommandBufferEndRecording`. The summaries of the recordings that lived in this memory are dropped.
    pub fn end_recording(&mut self, cb: usize) -> Result<CommandHandle> {
        self.buffer(cb);
        let r = self.buffers[cb].end_recording(&mut self.mem)?;
        for s in &r.spans {
            self.cache.invalidate(s.start, s.end);
        }
        Ok(r.handle)
    }

    /// `nvnCommandBufferGetCommandMemoryUsed`.
    pub fn command_memory_used(&mut self, cb: usize) -> u64 {
        self.buffer(cb).used()
    }

    /// `nvnCommandBufferGetCommandMemoryFree`.
    pub fn command_memory_free(&mut self, cb: usize) -> u64 {
        self.buffer(cb).free()
    }

    /// `nvnQueueSubmitCommands` for one recording. The queue starts with the default state.
    /// The summary comes from the cache when it holds. The effects go to the frame.
    pub fn submit(&mut self, handle: CommandHandle) -> Result<()> {
        let cx = Ctx {
            mem: &self.mem,
            table: &self.table,
            desc: &self.descriptors,
        };
        let i = summary(&mut self.cache, &cx, handle.0, &State::default(), 0)?;
        let s = self
            .cache
            .entry(handle.0, i)
            .ok_or(NvnError::BadRecord("summary is missing"))?;
        self.frame.append(&s.stream);
        Ok(())
    }

    /// The frame ends. This runs the pass analysis and returns its result.
    pub fn end_frame(&mut self, reason: FlushReason) -> Result<&PassPlan> {
        match reason {
            FlushReason::Present => self.flushes.present += 1,
            FlushReason::CpuWait => self.flushes.cpu_wait += 1,
        }
        self.analyser
            .analyse(&self.frame, &self.table, &mut self.plan)?;
        Ok(&self.plan)
    }

    /// The frame that the last `end_frame` analysed.
    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// The result of the last `end_frame`.
    pub fn plan(&self) -> &PassPlan {
        &self.plan
    }

    /// Starts the next frame.
    pub fn begin_frame(&mut self) {
        self.frame.clear();
    }

    /// Tells that the guest wrote host memory. The textures that overlap are marked `GuestNewer` and returned.
    /// The recordings in that memory lose their summaries.
    pub fn guest_wrote(&mut self, host: u64, len: u64) -> Result<&[TexId]> {
        let start = self.space.gpu_address(host, len)?;
        self.cache.invalidate(host, host.saturating_add(len));
        self.hit.clear();
        self.table.guest_wrote(start, len, &mut self.hit);
        Ok(&self.hit)
    }
}
