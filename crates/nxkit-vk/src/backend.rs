//! What the front end needs from the GPU backend.
//!
//! `nxkit-vk-gpu` implements [`Backend`] on Metal. The front end ([`crate::front::Vk`]) calls it through one
//! `Arc<dyn Backend>`. The trait uses only types of this crate, so the front end does not know Metal. It is coarse:
//!
//! - Objects appear and go: memory storage, textures (images, image views, texel buffer views, swapchain images),
//!   samplers, pipelines, query pools. The id of each is the Vulkan handle, so ids never collide or repeat. The front end
//!   calls `create_*` outside its lock, once per object, and `destroy_*` when the game destroys it. The game must not
//!   destroy an object that pending work uses (Vulkan). The backend's command buffers keep what they encode alive.
//! - Memory storage is made on the first buffer bind or map of a `VkDeviceMemory` (memory that only backs images
//!   never gets one). [`Backend::create_memory`] gives the host address of the contents, which the game maps.
//! - A submit ([`Backend::submit`]) hands over the recorded commands of every command buffer, the secondary command
//!   buffers they execute and the contents of every descriptor set they bind, all as owned snapshots: the backend
//!   needs no access to the front end's state and the front end's lock is not held.
//! - Submits are numbered 1, 2, 3 in queue order ([`Submission::serial`]). The backend completes them in order and
//!   reports the highest completed serial ([`Backend::completed`]). Fences, events and query results are done when
//!   their serial is. The backend must complete a serial even when encoding it failed.
//! - The swapchain: [`Backend::acquire`] paces the game and waits until the image's previous present has been shown.
//!   [`Backend::present`] shows an image once the given serial has completed.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::cmd::Cmd;
use crate::desc::{
    BufferViewDesc, ComputePipelineDesc, Descriptor, GraphicsPipelineDesc, ImageDesc, MemoryId,
    PipelineId, QueryPoolId, QueryType, SamplerDesc, SamplerId, SetLayoutDesc, ShaderKey,
    TextureId, ViewDesc,
};
use crate::device::DeviceCaps;
use crate::format::PixelFormat;

/// A failure that the backend reports. The text is for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError(pub String);

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Vulkan backend: {}", self.0)
    }
}

impl std::error::Error for BackendError {}

/// The result type of the backend.
pub type BackendResult<T> = std::result::Result<T, BackendError>;

/// The recorded commands of one command buffer at a submit.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandBufferSnapshot {
    /// The handle of the command buffer.
    pub handle: u64,
    /// Its commands.
    pub cmds: Arc<[Cmd]>,
}

/// The contents of one descriptor set at a submit.
#[derive(Clone, Debug, PartialEq)]
pub struct SetContents {
    /// The layout of the set.
    pub layout: Arc<SetLayoutDesc>,
    /// One descriptor per element, in the order of [`SetLayoutDesc::bindings`] and their `base`.
    pub descriptors: Arc<Vec<Descriptor>>,
}

/// One `vkQueueSubmit` (all its `VkSubmitInfo` in order), or an empty one that only signals a fence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Submission {
    /// The number of the submit. The backend completes it after every earlier one.
    pub serial: u64,
    /// The primary command buffers, in submit order.
    pub command_buffers: Vec<CommandBufferSnapshot>,
    /// The secondary command buffers that [`Cmd::ExecuteCommands`] names, by handle.
    pub secondaries: HashMap<u64, Arc<[Cmd]>>,
    /// The descriptor sets that [`Cmd::BindDescriptorSets`] names, by handle. A set that the game freed is missing,
    /// and the backend binds nothing for it.
    pub sets: HashMap<u64, SetContents>,
}

/// One present of a swapchain image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Present {
    /// The swapchain handle.
    pub swapchain: u64,
    /// The image index.
    pub index: u32,
    /// The texture of the image.
    pub image: TextureId,
    /// The extent of the swapchain.
    pub extent: (u32, u32),
    /// The `VkFormat` of the swapchain.
    pub format: u32,
    /// The serial of the last submit before the present: the image is shown once it has completed.
    pub after: u64,
}

/// What the front end needs from the GPU backend. See the module docs for the contract.
pub trait Backend: Send + Sync {
    /// What the GPU decides about the device that the game sees.
    fn caps(&self) -> DeviceCaps;

    /// The row alignment of a linear texture of a pixel format (Metal's `minimumLinearTextureAlignmentForPixelFormat`).
    fn linear_alignment(&self, _pixel: PixelFormat) -> u64 {
        256
    }

    /// Makes the storage of a `VkDeviceMemory` and gives the host address of its contents.
    fn create_memory(&self, id: MemoryId, size: u64, memory_type: u32) -> BackendResult<u64>;

    /// Frees the storage of a `VkDeviceMemory`.
    fn destroy_memory(&self, id: MemoryId);

    /// Makes the texture of an image: at `vkCreateImage`, or at `vkBindImageMemory` for a linear image
    /// ([`ImageDesc::linear`]).
    fn create_image(&self, id: TextureId, desc: &ImageDesc) -> BackendResult<()>;

    /// Makes the texture of an image view of `image`.
    fn create_view(&self, id: TextureId, image: TextureId, desc: &ViewDesc) -> BackendResult<()>;

    /// Makes the texture of a texel buffer view.
    fn create_buffer_view(&self, id: TextureId, desc: &BufferViewDesc) -> BackendResult<()>;

    /// Frees a texture (image, view, buffer view or swapchain image).
    fn destroy_texture(&self, id: TextureId);

    /// Makes a sampler.
    fn create_sampler(&self, id: SamplerId, desc: &SamplerDesc) -> BackendResult<()>;

    /// Frees a sampler.
    fn destroy_sampler(&self, id: SamplerId);

    /// Tells the backend about a new shader module and its SPIR-V (for a capture or a report of a missing shader).
    fn shader_module(&self, _key: ShaderKey, _code: &[u8]) {}

    /// Builds a graphics pipeline. A pipeline that the backend refuses is still handed to the game. Its draws are
    /// skipped.
    fn create_graphics_pipeline(
        &self,
        id: PipelineId,
        desc: &GraphicsPipelineDesc,
    ) -> BackendResult<()>;

    /// Builds a compute pipeline. As for graphics, a refused pipeline's dispatches are skipped.
    fn create_compute_pipeline(
        &self,
        id: PipelineId,
        desc: &ComputePipelineDesc,
    ) -> BackendResult<()>;

    /// Frees a pipeline.
    fn destroy_pipeline(&self, id: PipelineId);

    /// Makes a query pool.
    fn create_query_pool(&self, id: QueryPoolId, ty: QueryType, count: u32) -> BackendResult<()>;

    /// Frees a query pool.
    fn destroy_query_pool(&self, id: QueryPoolId);

    /// The value of a query. The front end asks only once the submit that wrote it has completed.
    fn query_value(&self, id: QueryPoolId, index: u32) -> u64;

    /// Replays a submit into the GPU. The backend completes [`Submission::serial`] when the GPU is done.
    fn submit(&self, submission: &Submission) -> BackendResult<()>;

    /// The highest completed serial.
    fn completed(&self) -> u64;

    /// Waits until `serial` has completed or `timeout_ns` has passed. Gives whether it completed.
    fn wait(&self, serial: u64, timeout_ns: u64) -> bool;

    /// Paces an acquire and waits until the previous present of `image` has been shown. Gives false on a timeout.
    fn acquire(&self, image: TextureId, timeout_ns: u64) -> bool;

    /// Shows a swapchain image once [`Present::after`] has completed.
    fn present(&self, present: &Present) -> BackendResult<()>;
}
