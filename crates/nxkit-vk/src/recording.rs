//! [`Recording`]: a [`Backend`] for tests that logs every call and completes every submit at once (or holds them).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::backend::{Backend, BackendError, BackendResult, Present, Submission};
use crate::desc::{
    BufferViewDesc, ComputePipelineDesc, GraphicsPipelineDesc, ImageDesc, MemoryId, PipelineId,
    QueryPoolId, QueryType, SamplerDesc, SamplerId, ShaderKey, TextureId, ViewDesc,
};
use crate::device::DeviceCaps;

/// One call to the backend.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    /// [`Backend::create_memory`].
    CreateMemory(MemoryId, u64, u32),
    /// [`Backend::destroy_memory`].
    DestroyMemory(MemoryId),
    /// [`Backend::create_image`].
    CreateImage(TextureId, Box<ImageDesc>),
    /// [`Backend::create_view`].
    CreateView(TextureId, TextureId, ViewDesc),
    /// [`Backend::create_buffer_view`].
    CreateBufferView(TextureId, BufferViewDesc),
    /// [`Backend::destroy_texture`].
    DestroyTexture(TextureId),
    /// [`Backend::create_sampler`].
    CreateSampler(SamplerId, SamplerDesc),
    /// [`Backend::destroy_sampler`].
    DestroySampler(SamplerId),
    /// [`Backend::shader_module`]: the key and the code length.
    ShaderModule(ShaderKey, usize),
    /// [`Backend::create_graphics_pipeline`].
    CreateGraphicsPipeline(PipelineId, Box<GraphicsPipelineDesc>),
    /// [`Backend::create_compute_pipeline`].
    CreateComputePipeline(PipelineId, Box<ComputePipelineDesc>),
    /// [`Backend::destroy_pipeline`].
    DestroyPipeline(PipelineId),
    /// [`Backend::create_query_pool`].
    CreateQueryPool(QueryPoolId, QueryType, u32),
    /// [`Backend::destroy_query_pool`].
    DestroyQueryPool(QueryPoolId),
    /// [`Backend::submit`].
    Submit(Box<Submission>),
    /// [`Backend::acquire`].
    Acquire(TextureId),
    /// [`Backend::present`].
    Present(Present),
}

/// A backend for tests. It gives memory storage at made-up host addresses, completes each submit when it is made
/// (unless [`Recording::hold`] is on) and keeps every call in order.
#[derive(Debug)]
pub struct Recording {
    calls: Mutex<Vec<Call>>,
    caps: DeviceCaps,
    next_host: AtomicU64,
    submitted: AtomicU64,
    completed: AtomicU64,
    hold: AtomicBool,
    fail_images: AtomicBool,
    queries: Mutex<HashMap<(QueryPoolId, u32), u64>>,
}

impl Default for Recording {
    fn default() -> Self {
        Self::new(DeviceCaps::default())
    }
}

impl Recording {
    /// A recording backend with the given caps.
    pub fn new(caps: DeviceCaps) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            caps,
            next_host: AtomicU64::new(0x7000_0000_0000),
            submitted: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            hold: AtomicBool::new(false),
            fail_images: AtomicBool::new(false),
            queries: Mutex::new(HashMap::new()),
        }
    }

    fn log(&self, c: Call) {
        self.lock().push(c);
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Call>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The calls so far.
    pub fn calls(&self) -> Vec<Call> {
        self.lock().clone()
    }

    /// The calls so far, and forgets them.
    pub fn take(&self) -> Vec<Call> {
        std::mem::take(&mut *self.lock())
    }

    /// The submissions so far.
    pub fn submissions(&self) -> Vec<Submission> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Submit(s) => Some(*s),
                _ => None,
            })
            .collect()
    }

    /// While on, submits stay pending until [`Recording::complete_all`].
    pub fn hold(&self, on: bool) {
        self.hold.store(on, Ordering::SeqCst);
    }

    /// Completes every submit so far.
    pub fn complete_all(&self) {
        self.completed
            .store(self.submitted.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    /// While on, [`Backend::create_image`] fails.
    pub fn fail_images(&self, on: bool) {
        self.fail_images.store(on, Ordering::SeqCst);
    }

    /// Sets the value that a query will report.
    pub fn set_query(&self, pool: QueryPoolId, index: u32, value: u64) {
        self.queries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((pool, index), value);
    }
}

impl Backend for Recording {
    fn caps(&self) -> DeviceCaps {
        self.caps
    }

    fn create_memory(&self, id: MemoryId, size: u64, memory_type: u32) -> BackendResult<u64> {
        self.log(Call::CreateMemory(id, size, memory_type));
        let len = size.max(16).next_multiple_of(0x1000);
        Ok(self.next_host.fetch_add(len, Ordering::SeqCst))
    }

    fn destroy_memory(&self, id: MemoryId) {
        self.log(Call::DestroyMemory(id));
    }

    fn create_image(&self, id: TextureId, desc: &ImageDesc) -> BackendResult<()> {
        if self.fail_images.load(Ordering::SeqCst) {
            return Err(BackendError("image refused".into()));
        }
        self.log(Call::CreateImage(id, Box::new(desc.clone())));
        Ok(())
    }

    fn create_view(&self, id: TextureId, image: TextureId, desc: &ViewDesc) -> BackendResult<()> {
        self.log(Call::CreateView(id, image, *desc));
        Ok(())
    }

    fn create_buffer_view(&self, id: TextureId, desc: &BufferViewDesc) -> BackendResult<()> {
        self.log(Call::CreateBufferView(id, *desc));
        Ok(())
    }

    fn destroy_texture(&self, id: TextureId) {
        self.log(Call::DestroyTexture(id));
    }

    fn create_sampler(&self, id: SamplerId, desc: &SamplerDesc) -> BackendResult<()> {
        self.log(Call::CreateSampler(id, *desc));
        Ok(())
    }

    fn destroy_sampler(&self, id: SamplerId) {
        self.log(Call::DestroySampler(id));
    }

    fn shader_module(&self, key: ShaderKey, code: &[u8]) {
        self.log(Call::ShaderModule(key, code.len()));
    }

    fn create_graphics_pipeline(
        &self,
        id: PipelineId,
        desc: &GraphicsPipelineDesc,
    ) -> BackendResult<()> {
        self.log(Call::CreateGraphicsPipeline(id, Box::new(desc.clone())));
        Ok(())
    }

    fn create_compute_pipeline(
        &self,
        id: PipelineId,
        desc: &ComputePipelineDesc,
    ) -> BackendResult<()> {
        self.log(Call::CreateComputePipeline(id, Box::new(desc.clone())));
        Ok(())
    }

    fn destroy_pipeline(&self, id: PipelineId) {
        self.log(Call::DestroyPipeline(id));
    }

    fn create_query_pool(&self, id: QueryPoolId, ty: QueryType, count: u32) -> BackendResult<()> {
        self.log(Call::CreateQueryPool(id, ty, count));
        Ok(())
    }

    fn destroy_query_pool(&self, id: QueryPoolId) {
        self.log(Call::DestroyQueryPool(id));
    }

    fn query_value(&self, id: QueryPoolId, index: u32) -> u64 {
        self.queries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(id, index))
            .copied()
            .unwrap_or(0)
    }

    fn submit(&self, submission: &Submission) -> BackendResult<()> {
        self.log(Call::Submit(Box::new(submission.clone())));
        self.submitted
            .fetch_max(submission.serial, Ordering::SeqCst);
        if !self.hold.load(Ordering::SeqCst) {
            self.complete_all();
        }
        Ok(())
    }

    fn completed(&self) -> u64 {
        self.completed.load(Ordering::SeqCst)
    }

    fn wait(&self, serial: u64, _timeout_ns: u64) -> bool {
        self.completed() >= serial
    }

    fn acquire(&self, image: TextureId, _timeout_ns: u64) -> bool {
        self.log(Call::Acquire(image));
        true
    }

    fn present(&self, present: &Present) -> BackendResult<()> {
        self.log(Call::Present(*present));
        Ok(())
    }
}
