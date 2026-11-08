//! The per-command detail that a frame does not carry.
//!
//! A `nxkit_nvn::frame::Frame` is an analysis model. It holds flags and texture lists, and no program, no vertex layout,
//! no buffer address, no draw arguments and no copy regions. The recorder side (the backend that a later change wires
//! to the NVN command decoder) fills a [`FrameDetail`] next to the frame. The vectors are indexed by ordinal:
//! the n-th `Cmd::Draw` of the frame uses `draws[n]`, the n-th `Cmd::Copy` uses `copies[n]`, and so on.
//!
//! Every number is the raw NVN value (`NVNblendFunc`, `NVNdepthFunc`, `NVNface` and others), so the recorder does no
//! translation. [`crate::translate`] does it in one place.

use std::sync::Arc;

/// The shader stage of a binding. The values are those of `NVNshaderStage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// The vertex stage.
    Vertex,
    /// The fragment stage.
    Fragment,
    /// The compute stage.
    Compute,
}

/// The shaders of a draw, as the keys of the shader step. A key names `<key>_b.metallib` and `<key>_b.json`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProgramRef {
    /// The key of the vertex shader. The walker shares one allocation per key between all draws.
    pub vertex: Arc<str>,
    /// The key of the fragment shader. A draw without one writes depth only.
    pub fragment: Option<Arc<str>>,
}

/// The blend state of one colour target (`NVNblendFunc` and `NVNblendEquation` values).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BlendDetail {
    /// Whether blending is on.
    pub enable: bool,
    /// The source factor of RGB.
    pub src_rgb: u32,
    /// The destination factor of RGB.
    pub dst_rgb: u32,
    /// The equation of RGB.
    pub op_rgb: u32,
    /// The source factor of alpha.
    pub src_alpha: u32,
    /// The destination factor of alpha.
    pub dst_alpha: u32,
    /// The equation of alpha.
    pub op_alpha: u32,
}

/// The stencil state of one face (`NVNstencilFunc` and `NVNstencilOp` values).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StencilDetail {
    /// The compare function.
    pub func: u32,
    /// The operation when the stencil test fails.
    pub fail: u32,
    /// The operation when the depth test fails.
    pub depth_fail: u32,
    /// The operation when both tests pass.
    pub pass: u32,
    /// The bits that the compare reads.
    pub read_mask: u8,
    /// The bits that a write changes.
    pub write_mask: u8,
}

/// The depth and stencil state of a draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DepthStencilDetail {
    /// Whether the depth test is on.
    pub depth_test: bool,
    /// Whether depth writes are on.
    pub depth_write: bool,
    /// The depth function (`NVNdepthFunc`).
    pub depth_func: u32,
    /// Whether the stencil test is on.
    pub stencil_test: bool,
    /// The front face.
    pub front: StencilDetail,
    /// The back face.
    pub back: StencilDetail,
}

/// The raster state of a draw.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RasterDetail {
    /// `NVNface`: 0 none, 1 front, 2 back, 3 front and back.
    pub cull: u32,
    /// `NVNfrontFace`: 0 clockwise, 1 counter-clockwise.
    pub front_face: u32,
    /// `NVNpolygonMode`: 0 point, 1 line, 2 fill.
    pub polygon_mode: u32,
    /// The polygon offset as factor, units and clamp, or none.
    pub depth_bias: Option<[f32; 3]>,
    /// Whether depth clamp is on (no clipping against the near and far plane).
    pub depth_clamp: bool,
    /// Whether the coverage comes from the alpha of the fragment.
    pub alpha_to_coverage: bool,
}

/// One vertex attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AttributeDetail {
    /// The attribute index. It is the `location` of the vertex input of the shader.
    pub location: u32,
    /// The `NVNformat` value of the attribute.
    pub format: u32,
    /// The byte offset in the vertex.
    pub offset: u32,
    /// The stream (vertex buffer binding) the attribute reads.
    pub stream: u32,
}

/// One vertex stream with its buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamDetail {
    /// The stream index.
    pub index: u32,
    /// The GPU address of the buffer.
    pub address: u64,
    /// The bytes from the address to the end of the buffer.
    pub size: u64,
    /// The bytes between two vertices. 0 means one value for every vertex.
    pub stride: u32,
    /// The instance divisor. 0 means per vertex.
    pub divisor: u32,
}

/// The arguments of a draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DrawArgs {
    /// `nvnCommandBufferDrawArrays*`.
    Arrays {
        /// The first vertex.
        first: u32,
        /// The vertex count.
        count: u32,
        /// The instance count.
        instances: u32,
        /// The first instance.
        base_instance: u32,
    },
    /// `nvnCommandBufferDrawElements*`.
    Indexed {
        /// `NVNindexType`: 0 byte, 1 short, 2 int.
        index_type: u32,
        /// The GPU address of the first index.
        address: u64,
        /// The index count.
        count: u32,
        /// The instance count.
        instances: u32,
        /// The value added to every index.
        base_vertex: i32,
        /// The first instance.
        base_instance: u32,
    },
}

/// A uniform buffer binding.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UboDetail {
    /// The stage.
    pub stage: Stage,
    /// The slot (`reg` of the reflection).
    pub slot: u32,
    /// The GPU address of the buffer.
    pub address: u64,
    /// The size in bytes.
    pub size: u32,
    /// The bytes at the time of the draw, when the guest rewrote them later in the same frame. The recorder reads
    /// them from guest memory when it sees the draw. `None` means the bytes at the address stay as they are until
    /// the GPU is done.
    pub snapshot: Option<Arc<[u8]>>,
}

/// A texture handle bound to a texture slot of a stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureBind {
    /// The stage.
    pub stage: Stage,
    /// The slot (`reg` of the reflection).
    pub slot: u32,
    /// The handle: `texture_id | sampler_id << 20`.
    pub handle: u64,
}

/// The state that an ICB does not hold. The encoder sets it outside the ICB.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DynamicDetail {
    /// The viewport as x, y, width and height in NVN pixels of the render target.
    pub viewport: [f32; 4],
    /// The depth range as near and far.
    pub depth_range: [f32; 2],
    /// The scissor as x, y, width and height, or none for the whole target.
    pub scissor: Option<[u32; 4]>,
    /// The constant blend colour.
    pub blend_color: [f32; 4],
    /// The stencil reference of the front and the back face.
    pub stencil_ref: [u32; 2],
}

/// Everything about one draw.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawDetail {
    /// The shaders.
    pub program: ProgramRef,
    /// The blend state of each colour target.
    pub blend: [BlendDetail; 8],
    /// The colour write mask of each target: bit 0 red to bit 3 alpha.
    pub write_masks: [u8; 8],
    /// The depth and stencil state.
    pub depth_stencil: DepthStencilDetail,
    /// The raster state.
    pub raster: RasterDetail,
    /// `NVNdrawPrimitive`: 0 points, 1 lines, 3 line strip, 4 triangles, 5 triangle strip.
    pub topology: u32,
    /// The vertex attributes.
    pub attributes: Vec<AttributeDetail>,
    /// The vertex streams.
    pub streams: Vec<StreamDetail>,
    /// The arguments.
    pub args: DrawArgs,
    /// The uniform buffers.
    pub ubos: Vec<UboDetail>,
    /// The textures.
    pub textures: Vec<TextureBind>,
    /// The state outside an ICB.
    pub dynamic: DynamicDetail,
}

/// A clear value. A colour clear uses `color`. A depth and stencil clear uses `depth` and `stencil`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClearDetail {
    /// The colour.
    pub color: [f32; 4],
    /// The depth.
    pub depth: f32,
    /// The stencil value.
    pub stencil: u32,
}

/// The box of a `Cmd::Copy`. A copy without detail copies level 0, layer 0, from the origin, the smaller size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyDetail {
    /// The source level.
    pub src_level: u32,
    /// The source layer.
    pub src_layer: u32,
    /// The first source texel.
    pub src_origin: [u32; 3],
    /// The destination level.
    pub dst_level: u32,
    /// The destination layer.
    pub dst_layer: u32,
    /// The first destination texel.
    pub dst_origin: [u32; 3],
    /// The size in texels of the source.
    pub size: [u32; 3],
}

/// A `Cmd::Dispatch`.
#[derive(Clone, Debug, PartialEq)]
pub struct DispatchDetail {
    /// The key of the compute shader.
    pub program: String,
    /// The threadgroups.
    pub groups: [u32; 3],
    /// The uniform buffers.
    pub ubos: Vec<UboDetail>,
    /// The textures.
    pub textures: Vec<TextureBind>,
}

/// A run of draws that came from one reused recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingRun {
    /// The handle of the recording in `nxkit_nvn::cache::RecCache`.
    pub handle: u64,
    /// The ordinal of its first draw.
    pub first_draw: u32,
    /// The number of its draws.
    pub draws: u32,
}

/// The detail of a frame.
#[derive(Clone, Debug, Default)]
pub struct FrameDetail {
    /// One entry per `Cmd::Draw`.
    pub draws: Vec<DrawDetail>,
    /// One entry per `Cmd::ClearColor` and `Cmd::ClearDepthStencil`, in command order. May be empty.
    pub clears: Vec<ClearDetail>,
    /// One entry per `Cmd::Copy`. May be empty.
    pub copies: Vec<CopyDetail>,
    /// One entry per `Cmd::Dispatch`. May be empty.
    pub dispatches: Vec<DispatchDetail>,
    /// The runs of draws from reused recordings. Only runs with a handle that `RecCache::icb_candidates` names get
    /// an ICB.
    pub runs: Vec<RecordingRun>,
}
