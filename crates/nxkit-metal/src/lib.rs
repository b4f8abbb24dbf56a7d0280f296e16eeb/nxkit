//! The Metal 4 backend of the nxkit host.
//!
//! This crate is the one place that talks to Metal. It holds the `unsafe` calls, in a few small modules, each with a
//! `SAFETY` comment. The rest of the workspace sees a safe API that does not name a Metal type.
//!
//! - [`Context`]: the device, the Metal 4 command queue, the compiler and the residency set.
//! - [`GuestMemory`]: the guest memory as one no-copy `MTLBuffer`, with the address map `AddressMap`.
//! - [`ShaderLibrary`], [`PipelineCache`]: pipelines through `MTL4Compiler`, keyed by a hash, with an archive. A
//!   [`MeshPipelineDesc`] gives a mesh pipeline (object, mesh and fragment stage).
//! - [`FrameRing`], [`Frame`]: two command allocators, one per frame in flight, with timing.
//! - [`CommandBuffer`], [`RenderPass`], [`ComputePass`]: data-driven encoding of passes.
//! - [`IndirectCommandBuffer`]: render commands encoded once and run in a pass. The module docs list what an ICB holds.
//! - [`ComputeIndirectCommandBuffer`], [`ExecutionRange`]: dispatches encoded once, and the layout of an indirect
//!   execution range that the GPU reads from a [`DataBuffer`].
//! - [`DataBuffer`]: a persistent shared buffer for argument buffers of resource IDs and addresses.
//! - [`Sampler`], [`Texture::gpu_resource_id`]: the resource IDs that bindless shaders read.
//! - [`BatchRing`], [`Batch`]: a frame in several command buffers, encoded on several threads, committed in order.
//! - [`DisplayLink`]: `CAMetalDisplayLink` pacing at any whole rate through `FrameHooks::display_tick`.
//! - [`display`]: the refresh rate of a display mode and a switch to another one for the run.
//! - [`Pacer`], [`TickScheduler`], [`TickSink`]: what each tick shows, by a [`PresentPlan`] from [`choose_present_plan`].
//! - [`PacingStats`]: present time, missed ticks and the interval histogram (R16).
//! - [`Presenter`]: a `CAMetalLayer` drawable path, with `Frame::present_at` for a tick.
//! - [`SpatialUpscaler`], [`FrameInterpolator`]: MetalFX spatial scaling and frame interpolation.
//! - [`RenderScale`], [`choose_render_scale`]: the scale the host renders at.
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
pub mod display;
mod encode;
mod error;
mod format;
mod frame;
mod guest;
mod icb;
mod interpolate;
mod memstats;
mod metrics;
mod pacing;
mod pipeline;
mod policy;
mod present;
mod render;
mod resample;
mod residency;
mod sampler;
mod scale;
mod schedule;
mod scratch;
mod state;
mod stats;
mod table;
mod texture;
pub mod thread;
mod timestamp;
mod upscale;

pub use address::AddressMap;
pub use barrier::{Dependency, Scope, Stages, Use, Visibility};
pub use batch::{
    BATCH_SCRATCH_BYTES, BATCH_SCRATCH_MAX_BYTES, BATCHES_IN_FLIGHT, Batch, BatchRing,
    SubmittedBatch,
};
pub use blocklinear::{GobOrder, LevelLayout, SurfaceLayout};
pub use compute::{ComputeCommand, ComputePass};
pub use compute_icb::{
    ComputeIcbCommand, ComputeIcbDesc, ComputeIndirectCommandBuffer, ExecutionRange,
};
pub use databuf::DataBuffer;
pub use deswizzle::{DESWIZZLE_MSL, DeswizzleParams, Deswizzler, Swizzler};
pub use device::{Context, autorelease_pool};
pub use encode::CommandBuffer;
pub use error::{MetalError, Result};
pub use format::{
    BcKind, Format, FormatClass, IndexType, LoadAction, Primitive, SampleType, StoreAction,
};
pub use frame::{FRAMES_IN_FLIGHT, Frame, FrameRing, SCRATCH_BYTES, Submitted};
pub use guest::{GuestMemory, PAGE_SIZE};
pub use icb::{IcbBuffer, IcbCommand, IcbDesc, IndirectCommandBuffer, Inherit};
pub use interpolate::{Camera, FrameInterpolator, InterpolateDesc, InterpolateInputs};
pub use memstats::{MemStats, mem_stats};
pub use metrics::{CommandBufferStats, FrameHooks, Metrics, MetricsSnapshot};
pub use pacing::{DisplayLink, DisplayTick, FrameInterval, LinkRate, Pacer, RateRange, TickSink};
pub use pipeline::{
    ArchiveConfig, ComputePipeline, MeshPipelineDesc, MissWhy, PipelineCache, RenderPipeline,
    RenderPipelineDesc, ShaderLibrary, Source,
};
pub use policy::{FrameSource, GameRate, PolicyInput, PresentPlan, choose_present_plan};
pub use present::{
    Drawable, PRESENT_FORMAT, Presenter, media_time, set_timed_present, timed_present,
};
pub use render::{
    ColorAttachment, CullMode, DepthAttachment, RenderCommand, RenderPass, RenderPassDesc, Scissor,
    StencilAttachment, Viewport,
};
pub use resample::{RESAMPLE_MSL, Resampler};
pub use residency::ResidentGuard;
pub use sampler::{Border, Filter, MipFilter, Reduction, Sampler, SamplerDesc, Wrap};
pub use scale::{
    MAX_SCALE, METALFX_RATIO, MIN_SCALE, RenderScale, SCALE_STEP, ScaleChoice, ScaleInput,
    UpscaleMode, choose_render_scale,
};
pub use schedule::{Shown, TickAction, TickScheduler};
pub use scratch::{SCRATCH_ALIGN, Scratch};
pub use state::{
    BlendFactor, BlendOp, BlendState, ColorTarget, CompareFn, DepthBias, DepthClip,
    DepthStencilDesc, DepthStencilState, FillMode, StencilFace, StencilOp, StepFunction,
    TopologyClass, VertexAttribute, VertexDescriptor, VertexFormat, VertexLayout, Winding,
};
pub use stats::{
    INTERVAL_BOUNDS_US, INTERVAL_BUCKETS, INTERVAL_LABELS, PacingSnapshot, PacingStats,
    interval_bucket,
};
pub use table::ArgumentTable;
pub use texture::{Storage, Texture, TextureDesc, TextureKind, TextureRegion, ViewDesc};
pub use timestamp::TimestampHeap;
pub use upscale::{SpatialUpscaler, UpscaleDesc};
