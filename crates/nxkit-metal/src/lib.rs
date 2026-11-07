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
//! - [`Presenter`]: a `CAMetalLayer` drawable path.
//! - [`Deswizzler`]: block-linear deswizzle from guest memory into a texture.
//!
//! The frame format comes from `nxkit-nvn`. This crate does not depend on it.
//!
//! The crate builds on macOS only. On other systems it is empty, so the workspace still builds there.

#![cfg(target_os = "macos")]

mod address;
mod compute;
mod deswizzle;
mod device;
mod encode;
mod error;
mod format;
mod frame;
mod guest;
mod metrics;
mod pipeline;
mod present;
mod render;
mod scratch;
mod state;
mod table;
mod texture;

pub use address::AddressMap;
pub use compute::{ComputeCommand, ComputePass};
pub use deswizzle::{DESWIZZLE_MSL, DeswizzleParams, Deswizzler, GobOrder};
pub use device::Context;
pub use encode::CommandBuffer;
pub use error::{MetalError, Result};
pub use format::{Format, IndexType, LoadAction, Primitive, StoreAction};
pub use frame::{FRAMES_IN_FLIGHT, Frame, FrameRing, SCRATCH_BYTES, Submitted};
pub use guest::{GuestMemory, PAGE_SIZE};
pub use metrics::{CommandBufferStats, FrameHooks, Metrics, MetricsSnapshot};
pub use pipeline::{
    ArchiveConfig, ComputePipeline, PipelineCache, RenderPipeline, RenderPipelineDesc,
    ShaderLibrary,
};
pub use present::{Drawable, PRESENT_FORMAT, Presenter};
pub use render::{
    ColorAttachment, CullMode, DepthAttachment, RenderCommand, RenderPass, RenderPassDesc, Scissor,
    StencilAttachment, Viewport,
};
pub use scratch::{SCRATCH_ALIGN, Scratch};
pub use state::{
    BlendFactor, BlendOp, BlendState, ColorTarget, CompareFn, DepthBias, DepthClip,
    DepthStencilDesc, DepthStencilState, FillMode, StencilFace, StencilOp, StepFunction,
    TopologyClass, VertexAttribute, VertexDescriptor, VertexFormat, VertexLayout, Winding,
};
pub use table::ArgumentTable;
pub use texture::{Storage, Texture, TextureDesc};
