//! The objects of the front end and the one state that holds them all, behind the front lock.

use std::collections::HashSet;
use std::sync::Arc;

use crate::cmd::{BindPoint, Cmd};
use crate::desc::{
    BufferRef, Descriptor, FramebufferDesc, ImageDesc, ImageRef, MemoryId, PipelineLayoutDesc,
    QueryType, RenderPassDesc, SamplerDesc, SetLayoutDesc, ShaderKey, TextureId, ViewDesc,
};
use crate::handle::{Kind, Registry};
use crate::info::{BufferInfo, TemplateEntry};
use crate::result::Log;

/// A `VkDeviceMemory`.
#[derive(Clone, Debug)]
pub(crate) struct Memory {
    pub(crate) size: u64,
    pub(crate) type_index: u32,
    /// The host address of the backend storage, once it exists.
    pub(crate) host: Option<u64>,
}

/// A `VkBuffer`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Buffer {
    pub(crate) info: BufferInfo,
    /// The memory handle, 0 until it is bound.
    pub(crate) memory: u64,
    pub(crate) offset: u64,
}

impl Buffer {
    /// The buffer from byte `off`, resolved to its memory.
    pub(crate) fn at(&self, off: u64) -> BufferRef {
        BufferRef {
            memory: MemoryId(self.memory),
            offset: self.offset.saturating_add(off),
            size: self.info.size.saturating_sub(off),
        }
    }
}

/// A `VkImage`.
#[derive(Clone, Debug)]
pub(crate) struct Image {
    pub(crate) desc: ImageDesc,
    /// The row pitch of a linear image whose texture lies over its memory, or 0 otherwise.
    pub(crate) pitch: u64,
    /// The bytes of the image (for its memory requirements and subresource layout).
    pub(crate) size: u64,
    /// Whether the backend made its texture.
    pub(crate) created: bool,
}

impl Image {
    pub(crate) fn as_ref(&self, handle: u64) -> ImageRef {
        ImageRef {
            texture: TextureId(handle),
            format: self.desc.format,
            samples: self.desc.samples,
        }
    }
}

/// A `VkImageView`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ImageView {
    pub(crate) image: u64,
    pub(crate) desc: ViewDesc,
    pub(crate) samples: u32,
}

/// A descriptor set.
#[derive(Clone, Debug)]
pub(crate) struct DescriptorSet {
    pub(crate) pool: u64,
    pub(crate) layout: Arc<SetLayoutDesc>,
    pub(crate) descriptors: Arc<Vec<Descriptor>>,
}

/// A pipeline.
#[derive(Clone, Debug)]
pub(crate) struct Pipeline {
    pub(crate) bind: BindPoint,
}

/// The recording state of a command buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CbState {
    Initial,
    Recording,
    Executable,
}

/// A `VkCommandBuffer`.
#[derive(Clone, Debug)]
pub(crate) struct CommandBuffer {
    pub(crate) pool: u64,
    pub(crate) secondary: bool,
    pub(crate) state: CbState,
    pub(crate) recording: Vec<Cmd>,
    pub(crate) frozen: Arc<[Cmd]>,
    pub(crate) in_pass: bool,
}

/// The state of a fence or an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    /// Not signalled, nothing pending.
    Unsignaled,
    /// Signalled.
    Signaled,
    /// Signalled once this serial has completed.
    Pending(u64),
}

/// A `VkQueryPool`.
#[derive(Clone, Debug)]
pub(crate) struct QueryPool {
    pub(crate) ty: QueryType,
    /// The serial of the submit that last wrote each query.
    pub(crate) tickets: Vec<Option<u64>>,
}

/// A `VkSwapchainKHR`.
#[derive(Clone, Debug)]
pub(crate) struct Swapchain {
    pub(crate) images: Vec<u64>,
    pub(crate) extent: (u32, u32),
    pub(crate) format: u32,
    pub(crate) next: u32,
}

/// Everything behind the front lock.
#[derive(Debug)]
pub(crate) struct State {
    pub(crate) instance: Option<u64>,
    pub(crate) physical: Option<u64>,
    pub(crate) device: Option<u64>,
    pub(crate) queue: Option<u64>,
    pub(crate) tokens: Registry<()>,
    pub(crate) surfaces: Registry<u64>,
    pub(crate) memories: Registry<Memory>,
    pub(crate) buffers: Registry<Buffer>,
    pub(crate) buffer_views: Registry<u64>,
    pub(crate) images: Registry<Image>,
    pub(crate) views: Registry<ImageView>,
    pub(crate) samplers: Registry<SamplerDesc>,
    pub(crate) set_layouts: Registry<Arc<SetLayoutDesc>>,
    pub(crate) pools: Registry<HashSet<u64>>,
    pub(crate) sets: Registry<DescriptorSet>,
    pub(crate) templates: Registry<Vec<TemplateEntry>>,
    pub(crate) pipeline_layouts: Registry<Arc<PipelineLayoutDesc>>,
    pub(crate) render_passes: Registry<Arc<RenderPassDesc>>,
    pub(crate) framebuffers: Registry<Arc<FramebufferDesc>>,
    pub(crate) modules: Registry<ShaderKey>,
    pub(crate) caches: Registry<()>,
    pub(crate) pipelines: Registry<Pipeline>,
    pub(crate) command_pools: Registry<HashSet<u64>>,
    pub(crate) command_buffers: Registry<CommandBuffer>,
    pub(crate) fences: Registry<Signal>,
    pub(crate) semaphores: Registry<()>,
    pub(crate) events: Registry<Signal>,
    pub(crate) query_pools: Registry<QueryPool>,
    pub(crate) swapchains: Registry<Swapchain>,
}

impl State {
    pub(crate) fn new() -> Self {
        Self {
            instance: None,
            physical: None,
            device: None,
            queue: None,
            tokens: Registry::new(Kind::DebugCallback),
            surfaces: Registry::new(Kind::Surface),
            memories: Registry::new(Kind::Memory),
            buffers: Registry::new(Kind::Buffer),
            buffer_views: Registry::new(Kind::BufferView),
            images: Registry::new(Kind::Image),
            views: Registry::new(Kind::ImageView),
            samplers: Registry::new(Kind::Sampler),
            set_layouts: Registry::new(Kind::SetLayout),
            pools: Registry::new(Kind::DescriptorPool),
            sets: Registry::new(Kind::DescriptorSet),
            templates: Registry::new(Kind::UpdateTemplate),
            pipeline_layouts: Registry::new(Kind::PipelineLayout),
            render_passes: Registry::new(Kind::RenderPass),
            framebuffers: Registry::new(Kind::Framebuffer),
            modules: Registry::new(Kind::ShaderModule),
            caches: Registry::new(Kind::PipelineCache),
            pipelines: Registry::new(Kind::Pipeline),
            command_pools: Registry::new(Kind::CommandPool),
            command_buffers: Registry::new(Kind::CommandBuffer),
            fences: Registry::new(Kind::Fence),
            semaphores: Registry::new(Kind::Semaphore),
            events: Registry::new(Kind::Event),
            query_pools: Registry::new(Kind::QueryPool),
            swapchains: Registry::new(Kind::Swapchain),
        }
    }

    /// A buffer from byte `off`, or an unbound reference for an unknown handle.
    pub(crate) fn buffer_ref(&self, log: &Log, buffer: u64, off: u64) -> BufferRef {
        match self.buffers.get(buffer) {
            Some(b) => b.at(off),
            None => {
                log.once(
                    "unknown buffer",
                    format_args!("[RED] a command names unknown buffer {buffer:#x}"),
                );
                BufferRef::default()
            }
        }
    }

    /// An image as a command names it, or an empty reference for an unknown handle.
    pub(crate) fn image_ref(&self, log: &Log, image: u64) -> ImageRef {
        match self.images.get(image) {
            Some(i) => i.as_ref(image),
            None => {
                log.once(
                    "unknown image",
                    format_args!("[RED] a command names unknown image {image:#x}"),
                );
                ImageRef::default()
            }
        }
    }
}
