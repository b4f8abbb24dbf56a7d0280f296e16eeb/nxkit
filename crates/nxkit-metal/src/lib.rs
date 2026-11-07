//! The Metal 4 backend of the nxkit host.
//!
//! This crate is the one place that talks to Metal. It holds the `unsafe` calls, in a few small modules, each with a
//! `SAFETY` comment. The rest of the workspace sees a safe API that does not name a Metal type.
//!
//! - [`Context`]: the device, the Metal 4 command queue, the compiler and the residency set.
//! - [`GuestMemory`]: the guest memory as one no-copy `MTLBuffer`, with the address map `AddressMap`.
//! - [`ShaderLibrary`], [`PipelineCache`]: pipelines through `MTL4Compiler`, keyed by a hash, with an archive.
//! - [`FrameRing`], [`Frame`]: three command allocators, one per frame in flight, with timing.
//! - [`CommandBuffer`], [`RenderPass`], [`ComputePass`]: data-driven encoding of passes.
//! - [`IndirectCommandBuffer`]: render commands encoded once and run in a pass. The module docs list what an ICB holds.
//! - [`ComputeIndirectCommandBuffer`], [`ExecutionRange`]: dispatches encoded once, and the layout of an indirect
//!   execution range that the GPU reads from a [`DataBuffer`].
//! - [`DataBuffer`]: a persistent shared buffer for argument buffers of resource IDs and addresses.
//! - [`Sampler`], [`Texture::gpu_resource_id`]: the resource IDs that bindless shaders read.
//! - [`BatchRing`], [`Batch`]: a frame in several command buffers, encoded on several threads, committed in order.
//! - [`DisplayLink`]: `CAMetalDisplayLink` pacing at 60 or 120 Hz through `FrameHooks::display_tick`.
//! - [`Presenter`]: a `CAMetalLayer` drawable path.
//! - [`Deswizzler`]: block-linear deswizzle from guest memory into a texture.
//!
//! The frame format comes from `nxkit-nvn`. This crate does not depend on it.
//!
//! The crate builds on macOS only. On other systems it is empty, so the workspace still builds there.

#![cfg(target_os = "macos")]

mod address;
mod barrier;
mod batch;
mod blocklinear;
mod compute;
mod compute_icb;
mod databuf;
mod deswizzle;
mod device;
mod encode;
mod error;
mod format;
mod frame;
mod guest;
mod icb;
mod metrics;
mod pacing;
mod pipeline;
mod present;
mod render;
mod residency;
mod sampler;
mod scratch;
mod state;
mod table;
mod texture;

pub use address::AddressMap;
pub use barrier::{Dependency, Scope, Stages, Use, Visibility};
pub use batch::{BATCH_SCRATCH_BYTES, BATCHES_IN_FLIGHT, Batch, BatchRing, SubmittedBatch};
pub use blocklinear::{GobOrder, LevelLayout, SurfaceLayout};
pub use compute::{ComputeCommand, ComputePass};
pub use compute_icb::{
    ComputeIcbCommand, ComputeIcbDesc, ComputeIndirectCommandBuffer, ExecutionRange,
};
pub use databuf::DataBuffer;
pub use deswizzle::{DESWIZZLE_MSL, DeswizzleParams, Deswizzler, Swizzler};
pub use device::Context;
pub use encode::CommandBuffer;
pub use error::{MetalError, Result};
pub use format::{BcKind, Format, FormatClass, IndexType, LoadAction, Primitive, StoreAction};
pub use frame::{FRAMES_IN_FLIGHT, Frame, FrameRing, SCRATCH_BYTES, Submitted};
pub use guest::{GuestMemory, PAGE_SIZE};
pub use icb::{IcbBuffer, IcbCommand, IcbDesc, IndirectCommandBuffer, Inherit};
pub use metrics::{CommandBufferStats, FrameHooks, Metrics, MetricsSnapshot};
pub use pacing::{DisplayLink, DisplayTick, FrameInterval};
pub use pipeline::{
    ArchiveConfig, ComputePipeline, PipelineCache, RenderPipeline, RenderPipelineDesc,
    ShaderLibrary,
};
pub use present::{Drawable, PRESENT_FORMAT, Presenter};
pub use render::{
    ColorAttachment, CullMode, DepthAttachment, RenderCommand, RenderPass, RenderPassDesc, Scissor,
    StencilAttachment, Viewport,
};
pub use residency::ResidentGuard;
pub use sampler::{Border, Filter, MipFilter, Reduction, Sampler, SamplerDesc, Wrap};
pub use scratch::{SCRATCH_ALIGN, Scratch};
pub use state::{
    BlendFactor, BlendOp, BlendState, ColorTarget, CompareFn, DepthBias, DepthClip,
    DepthStencilDesc, DepthStencilState, FillMode, StencilFace, StencilOp, StepFunction,
    TopologyClass, VertexAttribute, VertexDescriptor, VertexFormat, VertexLayout, Winding,
};
pub use table::ArgumentTable;
pub use texture::{Storage, Texture, TextureDesc, TextureKind, TextureRegion, ViewDesc};
