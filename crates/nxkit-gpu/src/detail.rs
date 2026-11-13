//! The per-command detail that a frame does not carry.
//!
//! A `nxkit_nvn::frame::Frame` is an analysis model. It holds flags and texture lists, and no program, no vertex layout,
//! no buffer address, no draw arguments and no copy regions. The recorder side (the backend that a later change wires
//! to the NVN command decoder) fills a [`FrameDetail`] next to the frame. The vectors are indexed by ordinal:
//! the n-th `Cmd::Draw` of the frame uses `draws[n]`, the n-th `Cmd::Copy` uses `copies[n]`, and so on.
//!
//! Enum values use NVN (`NVNblendFunc`, `NVNdepthFunc`, `NVNface` and others). The walker converts the viewport and
//! front-face convention for the captured origin modes. `crate::translate` converts the remaining state.

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
    /// The task stage of NVN2 (the object stage of a Metal mesh pipeline).
    Task,
    /// The mesh stage of NVN2.
    Mesh,
}

/// The shaders of a draw, as the keys of the shader step. A key names `<key>_b.metallib` and `<key>_b.json`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProgramRef {
    /// The key of the vertex shader. The walker shares one allocation per key between all draws. Empty for a mesh
    /// draw.
    pub vertex: Arc<str>,
    /// The key of the fragment shader. A draw without one writes depth only.
    pub fragment: Option<Arc<str>>,
    /// The key of the task shader of a mesh draw, when the program has one.
    pub task: Option<Arc<str>>,
    /// The key of the mesh shader of a mesh draw. `None` for every other draw.
    pub mesh: Option<Arc<str>>,
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
    /// The Metal front face after queue-origin conversion: 0 clockwise, 1 counter-clockwise.
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
    /// `nvnCommandBufferDrawArraysIndirect`: the GPU reads `{count, instances, first, base instance}` at `indirect`.
    ArraysIndirect {
        /// The GPU address of the arguments.
        indirect: u64,
    },
    /// `nvnCommandBufferDrawElementsIndirect`: the GPU reads `{count, instances, first index, base vertex, base
    /// instance}` at `indirect`.
    IndexedIndirect {
        /// `NVNindexType`: 0 byte, 1 short, 2 int.
        index_type: u32,
        /// The GPU address of index 0.
        address: u64,
        /// The GPU address of the arguments.
        indirect: u64,
    },
    /// `nvnCommandBufferMultiDraw{Arrays,Elements}IndirectCount`: up to `max` draws, draw `i` reads its arguments at
    /// `indirect + i * stride` and runs only while `i` is below the `u32` at `count`.
    MultiIndirectCount {
        /// The index type and the GPU address of index 0 of an elements draw, `None` for arrays.
        index: Option<(u32, u64)>,
        /// The GPU address of the arguments of draw 0.
        indirect: u64,
        /// The GPU address of the draw count.
        count: u64,
        /// The most draws.
        max: u32,
        /// The bytes from the arguments of one draw to the next.
        stride: u64,
    },
    /// `nvnCommandBufferDrawMeshTasks`: `count` workgroups from `first`, of the task shader or, without one, of the
    /// mesh shader.
    MeshTasks {
        /// The first workgroup.
        first: u32,
        /// The number of workgroups.
        count: u32,
    },
    /// `nvnCommandBufferDrawMeshTasksIndirect`: the GPU reads `{count, first}` at `indirect`.
    MeshTasksIndirect {
        /// The GPU address of the arguments.
        indirect: u64,
    },
}

impl DrawArgs {
    /// True for a mesh draw.
    pub fn is_mesh(&self) -> bool {
        matches!(
            self,
            Self::MeshTasks { .. } | Self::MeshTasksIndirect { .. }
        )
    }

    /// True when the GPU reads the arguments from memory when the draw runs.
    pub fn is_indirect(&self) -> bool {
        matches!(
            self,
            Self::ArraysIndirect { .. }
                | Self::IndexedIndirect { .. }
                | Self::MultiIndirectCount { .. }
                | Self::MeshTasksIndirect { .. }
        )
    }
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
    /// The viewport as x, y, width and height in unscaled pixels, after origin and swizzle conversion.
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
    /// The graphics push constants (`nvnCommandBufferPushGraphicsConstants`, bank 24), when any were pushed.
    pub push: Option<std::sync::Arc<[u8]>>,
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
    /// The bound storage buffers. `snapshot` is always `None`.
    pub sbufs: Vec<UboDetail>,
    /// The textures.
    pub textures: Vec<TextureBind>,
    /// The storage images (`BindImage`) of the vertex and fragment stages, by `slot` of the `img` binding.
    pub images: Vec<TextureBind>,
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

/// What a `Cmd::Copy` or `Cmd::Downsample` does, as far as the walker could read it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CopyKind {
    /// No box was read: a downsample, an inline update, or a copy of a recording that has no extension record. The
    /// entry only keeps the ordinals aligned.
    #[default]
    Other,
    /// `nvnCommandBufferCopyBufferToTexture`. The destination texture side is in the `dst_*` fields.
    BufferToTexture,
    /// `nvnCommandBufferCopyTextureToBuffer`. The source texture side is in the `src_*` fields.
    TextureToBuffer,
    /// `nvnCommandBufferCopyTextureToTexture`.
    TextureToTexture,
    /// `nvnCommandBufferClearTexture` and `ClearTextureui`. The destination texture side is in the `dst_*` fields and
    /// the colour in [`CopyDetail::clear`].
    Clear,
    /// `nvnCommandBufferCopyBufferToBuffer`. The addresses are in `src_addr` and `dst_addr`, the byte count in
    /// [`CopyDetail::bytes`]. The command is a [`Cmd::BufferCopy`](nxkit_nvn::frame::Cmd::BufferCopy).
    BufferToBuffer,
    /// `nvnCommandBufferClearBuffer`. The address is in `dst_addr`, the byte count in [`CopyDetail::bytes`] and the
    /// 32-bit pattern in [`CopyDetail::value`]. The command is a [`Cmd::BufferCopy`](nxkit_nvn::frame::Cmd::BufferCopy).
    BufferFill,
}

/// The box of a `Cmd::Copy`, one entry per `Cmd::Copy`, `Cmd::BufferCopy` and `Cmd::Downsample`.
///
/// The origins and sizes are the raw `NVNcopyRegion` values. The walker does not know the target of a texture, so the
/// third value is the first layer of the region for array and cube textures (added to the layer of the view) and the
/// z offset or the depth for a 3D texture. The encoder resolves it with the texture it has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyDetail {
    /// What the entry is. [`CopyKind::Other`] carries no box.
    pub kind: CopyKind,
    /// The source level.
    pub src_level: u32,
    /// The first layer of the source view.
    pub src_layer: u32,
    /// The first source texel.
    pub src_origin: [u32; 3],
    /// The destination level.
    pub dst_level: u32,
    /// The first layer of the destination view.
    pub dst_layer: u32,
    /// The first destination texel.
    pub dst_origin: [u32; 3],
    /// The size in texels of the source.
    pub size: [u32; 3],
    /// The size in texels of the destination. A copy between textures stretches the source box onto the destination
    /// box when the sizes differ (see `passes::scale`).
    pub dst_size: [u32; 3],
    /// The copy flags of a [`CopyKind::TextureToTexture`] (`NVNcopyFlags`). Bit 0 asks for linear filtering.
    pub flags: u32,
    /// The colour of a [`CopyKind::Clear`] as the raw words: `f32` bits for kind 0, `u32` values for kind 2.
    pub clear: [u32; 4],
    /// The value type of a [`CopyKind::Clear`]: 0 for `f32` and 2 for `u32`, as in `ExtCode::ClearColor`.
    pub clear_kind: u32,
    /// The channel mask of a [`CopyKind::Clear`]. Bit 0 is red and bit 3 alpha.
    pub channel_mask: u32,
    /// The GPU address of the buffer side of a buffer copy.
    pub buffer: u64,
    /// The bytes between two rows of the buffer (`SetCopyRowStride`). 0 means tightly packed.
    pub row_stride: u64,
    /// The bytes between two layers or depth slices of the buffer (`SetCopyImageStride`). 0 means tightly packed.
    pub image_stride: u64,
    /// The GPU address of what the copy reads (a buffer, or the storage of a texture), 0 when the walker did not read
    /// it. Set for the copies between buffers and textures and for a buffer to buffer copy.
    pub src_addr: u64,
    /// The GPU address of what the copy writes, 0 when the walker did not read it. The address of a texture side is the
    /// plain address, without the texture id the record word carries above it.
    pub dst_addr: u64,
    /// The bytes of a [`CopyKind::BufferToBuffer`] or [`CopyKind::BufferFill`].
    pub bytes: u64,
    /// The 32-bit pattern of a [`CopyKind::BufferFill`].
    pub value: u32,
}

/// A `Cmd::Dispatch`.
#[derive(Clone, Debug, PartialEq)]
pub struct DispatchDetail {
    /// The key of the compute shader.
    pub program: String,
    /// The compute push constants (`nvnCommandBufferPushComputeConstants`, bank 24), when any were pushed.
    pub push: Option<std::sync::Arc<[u8]>>,
    /// The threadgroups. Zero for an indirect dispatch: the walker cannot know them, so they are never encoded.
    pub groups: [u32; 3],
    /// The guest GPU address of the three u32 group counts (NVN layout: x, y, z) of
    /// `nvnCommandBufferDispatchComputeIndirect`, `None` for a direct dispatch.
    pub indirect: Option<u64>,
    /// The uniform buffers.
    pub ubos: Vec<UboDetail>,
    /// The bound storage buffers. `snapshot` is always `None`.
    pub sbufs: Vec<UboDetail>,
    /// The textures.
    pub textures: Vec<TextureBind>,
    /// The storage images (`BindImage`), by `slot` of the `img` binding. `handle` is the texture id of the
    /// descriptor, without a sampler.
    pub images: Vec<TextureBind>,
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
    /// One entry per `Cmd::Draw`. A draw that a walker template produced is shared with the template.
    pub draws: Vec<Arc<DrawDetail>>,
    /// One entry per `Cmd::ClearColor` and `Cmd::ClearDepthStencil`, in command order. May be empty.
    pub clears: Vec<ClearDetail>,
    /// One entry per `Cmd::Copy`, `Cmd::BufferCopy` and `Cmd::Downsample`, in command order. May be empty.
    pub copies: Vec<CopyDetail>,
    /// One entry per `Cmd::Dispatch`. May be empty.
    pub dispatches: Vec<DispatchDetail>,
    /// The runs of draws from reused recordings. Only runs with a handle that `RecCache::icb_candidates` names get
    /// an ICB.
    pub runs: Vec<RecordingRun>,
    /// The timestamp reports (`nvnCommandBufferReportCounter`) and value reports (`nvnCommandBufferReportValue`) of the
    /// frame, in command order.
    pub reports: Vec<CounterReport>,
    /// The storage ranges that this frame reads and writes from a copy of its own (`rename`), taken at the flush. Only
    /// with the overlap on.
    pub(crate) rename: Option<Arc<crate::rename::Capture>>,
    /// The storage bindings of the frame whose use (written or only read) the encoder has to look at; `None` means all of
    /// them. The flush places the others (`rename`).
    pub(crate) rename_check: Option<Vec<(u8, u32, u32)>>,
}

/// The [`CounterReport::kind`] of `nvnCommandBufferReportValue`: the counter is [`CounterReport::value`].
pub const REPORT_VALUE: u32 = u32::MAX;

/// The [`CounterReport::kind`] of an `nvnCommandBufferSignalEvent` that writes [`CounterReport::value`] into the
/// counter of the event. Its [`CounterReport::addr`] is the host address of the 32-bit counter word, not a GPU address.
pub const REPORT_EVENT_WRITE: u32 = u32::MAX - 1;

/// The [`CounterReport::kind`] of an `nvnCommandBufferSignalEvent` that adds [`CounterReport::value`] to the counter of
/// the event (`NVN_EVENT_SIGNAL_MODE_ADD`). Its [`CounterReport::addr`] is the host address of the counter word.
pub const REPORT_EVENT_ADD: u32 = u32::MAX - 2;

/// A counter report that the GPU writes when it reaches the command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CounterReport {
    /// The `NVNcounterType`. 0 is `TIMESTAMP` (when the work before it is done), 0x10 `TIMESTAMP_TOP` (when the
    /// command is reached). [`REPORT_VALUE`] is a `ReportValue`.
    pub kind: u32,
    /// The counter of a [`REPORT_VALUE`], 0 for the others.
    pub value: u32,
    /// The GPU address of the 16-byte `NVNcounterData`: the counter value, then the timestamp in GPU ticks. For an
    /// event signal the host address of the counter word.
    pub addr: u64,
}

impl CounterReport {
    /// True for an event signal ([`REPORT_EVENT_WRITE`], [`REPORT_EVENT_ADD`]): the backend writes it into the counter
    /// of the event when the GPU finished the frame (`backend::events`), not as a counter report.
    pub fn is_event(&self) -> bool {
        matches!(self.kind, REPORT_EVENT_WRITE | REPORT_EVENT_ADD)
    }
}
