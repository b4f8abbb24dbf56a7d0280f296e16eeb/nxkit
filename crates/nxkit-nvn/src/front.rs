//! The front end: the one object that the NVN entry points of the host call.
//!
//! The front end owns the resource table, the descriptor pools, the command buffers, the cache of recordings and the frame.
//! It turns guest addresses into GPU addresses through a [`GpuSpace`] and rejects addresses outside the mapping.
//! A command buffer writes its records into the command memory of the game, through [`GuestMem`].
//! A submit gives the summary of the recording under the state of the queue and appends its effects to the frame.
//! The frame is held until a present or until the CPU waits for the GPU. [`FrontEnd::end_frame`] counts the reason,
//! runs the pass analysis and keeps the frame for the backend. The next [`FrontEnd::begin_frame`] drops the frame
//! and keeps its memory.

use std::collections::HashMap;

use crate::analyse::Analyser;
use crate::cache::{CacheStats, RecCache};
use crate::cmdbuf::{CommandBuffer, CommandHandle};
use crate::decode::{Ctx, State, summary};
use crate::descriptors::{Descriptors, TextureSlot};
use crate::error::{NvnError, Result};
use crate::frame::{Cmd, Frame};
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

/// The memory callback of a game (`nvnCommandBufferSetMemoryCallback`). The host implements it
/// and keeps the callback data of the game inside.
pub trait MemoryCallback: std::fmt::Debug + Send {
    /// The command buffer `cb` needs `needed` more bytes. The result is a block as host address and length.
    /// None means that the game gives no memory.
    fn command_memory(&mut self, cb: usize, needed: u32) -> Option<(u64, u64)>;
}

/// How often the front end asks the callback for one record.
pub const MAX_CALLBACK_TRIES: u32 = 4;

#[derive(Debug)]
struct Callback {
    hook: Box<dyn MemoryCallback>,
    enabled: bool,
}

/// The NVN front end.
#[derive(Debug)]
pub struct FrontEnd<S: GpuSpace, M: GuestMem> {
    space: S,
    mem: M,
    table: ResourceTable,
    descriptors: Descriptors,
    buffers: Vec<CommandBuffer>,
    callbacks: HashMap<usize, Callback>,
    queues: HashMap<usize, State>,
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
            callbacks: HashMap::new(),
            queues: HashMap::new(),
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
        self.queues.values_mut().for_each(|q| q.forget(id));
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

    /// `nvnCommandBufferFinalize`. The command buffer `cb` loses its memory blocks, its recording state and its
    /// callback, so the number can name another command buffer later.
    pub fn release_command_buffer(&mut self, cb: usize) {
        if let Some(b) = self.buffers.get_mut(cb) {
            *b = CommandBuffer::new();
        }
        self.callbacks.remove(&cb);
    }

    /// `nvnCommandBufferBeginRecording`.
    pub fn begin_recording(&mut self, cb: usize) -> Result<()> {
        self.buffer(cb).begin_recording()
    }

    /// `nvnCommandBufferSetMemoryCallback`. None removes the callback. A new callback is enabled.
    pub fn set_memory_callback(&mut self, cb: usize, hook: Option<Box<dyn MemoryCallback>>) {
        match hook {
            Some(hook) => {
                self.callbacks.insert(
                    cb,
                    Callback {
                        hook,
                        enabled: true,
                    },
                );
            }
            None => {
                self.callbacks.remove(&cb);
            }
        }
    }

    /// `nvnCommandBufferSetCommandMemoryCallbackEnabled`.
    pub fn set_memory_callback_enabled(&mut self, cb: usize, enabled: bool) {
        if let Some(c) = self.callbacks.get_mut(&cb) {
            c.enabled = enabled;
        }
    }

    /// Writes a record. When the command memory is full, the front end calls the memory callback of the game,
    /// adds the block that it gives and writes the record again, up to [`MAX_CALLBACK_TRIES`] times.
    /// Without an enabled callback, or when it gives no usable block, the error is [`NvnError::OutOfCommandMemory`].
    pub fn write(&mut self, cb: usize, rec: Record) -> Result<()> {
        self.buffer(cb);
        for _ in 0..MAX_CALLBACK_TRIES {
            let need = match self.buffers[cb].write(&mut self.mem, rec) {
                Err(NvnError::OutOfCommandMemory { need }) => need,
                other => return other,
            };
            let block = match self.callbacks.get_mut(&cb) {
                Some(c) if c.enabled => c.hook.command_memory(cb, need),
                _ => None,
            };
            let Some((host, len)) = block else {
                return Err(NvnError::OutOfCommandMemory { need });
            };
            self.add_command_memory(cb, host, len)?;
        }
        self.buffers[cb].write(&mut self.mem, rec)
    }

    /// `nvnCommandBufferEndRecording`. The summaries of the recordings that lived in this memory are dropped.
    pub fn end_recording(&mut self, cb: usize) -> Result<CommandHandle> {
        self.buffer(cb);
        let r = self.buffers[cb].end_recording(&mut self.mem)?;
        self.recorded(&r.spans);
        Ok(r.handle)
    }

    /// Drops the summaries of recordings that lived in these memory ranges. A recorder that owns its command buffer
    /// (the C ABI of `nxkit-rt`) calls this when it ends a recording, as [`FrontEnd::end_recording`] does.
    pub fn recorded(&mut self, spans: &[std::ops::Range<u64>]) {
        for s in spans {
            self.cache.invalidate(s.start, s.end);
        }
    }

    /// `nvnCommandBufferGetCommandMemoryUsed`.
    pub fn command_memory_used(&mut self, cb: usize) -> u64 {
        self.buffer(cb).used()
    }

    /// `nvnCommandBufferGetCommandMemoryFree`.
    pub fn command_memory_free(&mut self, cb: usize) -> u64 {
        self.buffer(cb).free()
    }

    /// `nvnQueueSubmitCommands` for one recording on queue 0. See [`FrontEnd::submit_on`].
    pub fn submit(&mut self, handle: CommandHandle) -> Result<()> {
        self.submit_on(0, handle)
    }

    /// `nvnQueueSubmitCommands` for one recording. `queue` numbers the queue. A queue that was not seen before starts
    /// with the default state. The summary comes from the cache when it holds. The effects go to the frame.
    ///
    /// The state that the recording leaves stays with the queue and is the inherited state of its next submit, as on
    /// the GPU. A recording that fails to decode leaves the state of the queue as it was.
    pub fn submit_on(&mut self, queue: usize, handle: CommandHandle) -> Result<()> {
        let cx = Ctx {
            mem: &self.mem,
            table: &self.table,
            desc: &self.descriptors,
        };
        let inherited = self.queues.entry(queue).or_default();
        let i = summary(&mut self.cache, &cx, handle.0, inherited, 0)?;
        let s = self
            .cache
            .entry(handle.0, i)
            .ok_or(NvnError::BadRecord("summary is missing"))?;
        inherited.apply(&s.delta);
        self.frame.append(&s.stream);
        Ok(())
    }

    /// The state that the last submit of a queue left.
    pub fn queue_state(&self, queue: usize) -> Option<&State> {
        self.queues.get(&queue)
    }

    /// Forgets the state of a queue, for `nvnQueueFinalize`. The queue starts again with the default state.
    pub fn drop_queue(&mut self, queue: usize) {
        self.queues.remove(&queue);
    }

    /// The game presents a texture with `nvnQueuePresentTexture`. This puts the present into the frame,
    /// behind the commands that were submitted before it.
    pub fn present(&mut self, texture: TexId) {
        self.frame.cmds.push(Cmd::Present(texture));
    }

    /// True when no command was submitted since the frame began.
    pub fn frame_is_empty(&self) -> bool {
        self.frame.cmds.is_empty()
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
