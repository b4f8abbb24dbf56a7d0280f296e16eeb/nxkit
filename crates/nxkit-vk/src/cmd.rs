//! The recorded commands of a command buffer: one typed [`Cmd`] per `vkCmd*` call, no byte arena.
//!
//! What a command names is resolved when it is recorded: buffers to their memory and offset ([`BufferRef`]), images
//! to their texture ([`ImageRef`]), pipelines to their backend id, a render pass begin to its attachments. Vulkan wants
//! those objects alive until the work is done. Two things are resolved at submit instead, as the reference layer reads
//! them at replay: descriptor sets ([`Cmd::BindDescriptorSets`] names them by handle, the submission carries their
//! contents) and secondary command buffers ([`Cmd::ExecuteCommands`]).
//!
//! A command that the front end refuses at record time (a bad range, an unsupported feature) is not recorded and is
//! logged once, as in the reference layer.

use std::sync::Arc;

use crate::desc::{
    BufferRef, FramebufferDesc, ImageRef, PipelineId, QueryPoolId, Rect2D, RenderPassDesc,
    SubresourceRange, Viewport,
};

/// `VkImageSubresourceLayers`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SubresourceLayers {
    /// `aspectMask`.
    pub aspect: u32,
    /// `mipLevel`.
    pub level: u32,
    /// `baseArrayLayer`.
    pub base_layer: u32,
    /// `layerCount`.
    pub layers: u32,
}

/// `VkBufferCopy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BufferCopy {
    /// `srcOffset`, from the start of the source buffer.
    pub src_offset: u64,
    /// `dstOffset`, from the start of the destination buffer.
    pub dst_offset: u64,
    /// `size`.
    pub size: u64,
}

/// `VkBufferImageCopy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BufferImageCopy {
    /// `bufferOffset`, from the start of the buffer.
    pub buffer_offset: u64,
    /// `bufferRowLength` in texels (0 = tightly packed).
    pub row_length: u32,
    /// `bufferImageHeight` in texels (0 = tightly packed).
    pub image_height: u32,
    /// `imageSubresource`.
    pub sub: SubresourceLayers,
    /// `imageOffset`.
    pub offset: [i32; 3],
    /// `imageExtent`.
    pub extent: [u32; 3],
}

/// `VkImageCopy` and `VkImageResolve` (the same layout).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ImageCopy {
    /// `srcSubresource`.
    pub src_sub: SubresourceLayers,
    /// `srcOffset`.
    pub src_offset: [i32; 3],
    /// `dstSubresource`.
    pub dst_sub: SubresourceLayers,
    /// `dstOffset`.
    pub dst_offset: [i32; 3],
    /// `extent`.
    pub extent: [u32; 3],
}

/// `VkImageBlit`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ImageBlit {
    /// `srcSubresource`.
    pub src_sub: SubresourceLayers,
    /// `srcOffsets`: two corners.
    pub src_offsets: [[i32; 3]; 2],
    /// `dstSubresource`.
    pub dst_sub: SubresourceLayers,
    /// `dstOffsets`.
    pub dst_offsets: [[i32; 3]; 2],
}

/// A `VkClearValue` as its 16 raw bytes: four colour words, or depth (`f32` bits) and stencil in the first two. The
/// attachment's format says which ([`crate::format::FormatInfo::clear_color`]).
pub type ClearValue = [u32; 4];

/// `VkClearAttachment`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ClearAttachment {
    /// `aspectMask`.
    pub aspect: u32,
    /// `colorAttachment`: the index among the colour attachments of the subpass.
    pub color_attachment: u32,
    /// `clearValue`.
    pub value: ClearValue,
}

/// `VkClearRect`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ClearRect {
    /// `rect`.
    pub rect: Rect2D,
    /// `baseArrayLayer`.
    pub base_layer: u32,
    /// `layerCount`.
    pub layers: u32,
}

/// The two bind points.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindPoint {
    /// `VK_PIPELINE_BIND_POINT_GRAPHICS`.
    Graphics,
    /// `VK_PIPELINE_BIND_POINT_COMPUTE`.
    Compute,
}

/// `VkIndexType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IndexType {
    /// `VK_INDEX_TYPE_UINT16`.
    U16,
    /// `VK_INDEX_TYPE_UINT32`.
    U32,
    /// `VK_INDEX_TYPE_UINT8`.
    U8,
}

/// Which stencil value a [`Cmd::SetStencil`] sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StencilValue {
    /// `vkCmdSetStencilCompareMask`.
    CompareMask,
    /// `vkCmdSetStencilWriteMask`.
    WriteMask,
    /// `vkCmdSetStencilReference`.
    Reference,
}

/// A begin of a render pass, resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderPassBegin {
    /// The render pass.
    pub pass: Arc<RenderPassDesc>,
    /// The framebuffer.
    pub framebuffer: Arc<FramebufferDesc>,
    /// `renderArea`.
    pub area: Rect2D,
    /// The clear values, one per attachment (at most 16).
    pub clear: Vec<ClearValue>,
}

/// One recorded command.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    /// `vkCmdBeginRenderPass`.
    BeginRenderPass(Arc<RenderPassBegin>),
    /// `vkCmdEndRenderPass`.
    EndRenderPass,
    /// `vkCmdBindPipeline`.
    BindPipeline {
        /// The bind point.
        bind: BindPoint,
        /// The pipeline.
        pipeline: PipelineId,
    },
    /// `vkCmdBindDescriptorSets`.
    BindDescriptorSets {
        /// The bind point.
        bind: BindPoint,
        /// `firstSet`.
        first: u32,
        /// The set handles (at most 8); their contents are in the submission.
        sets: Vec<u64>,
        /// The dynamic offsets, in the order of the sets and their dynamic bindings.
        dynamic_offsets: Vec<u32>,
    },
    /// `vkCmdBindVertexBuffers`.
    BindVertexBuffers {
        /// `firstBinding`.
        first: u32,
        /// The buffers from their offsets.
        buffers: Vec<BufferRef>,
    },
    /// `vkCmdBindIndexBuffer`.
    BindIndexBuffer {
        /// The buffer from its offset.
        buffer: BufferRef,
        /// The index type.
        index_type: IndexType,
    },
    /// `vkCmdSetViewport`.
    SetViewport {
        /// `firstViewport`.
        first: u32,
        /// The viewports.
        viewports: Vec<Viewport>,
    },
    /// `vkCmdSetScissor`.
    SetScissor {
        /// `firstScissor`.
        first: u32,
        /// The rectangles.
        rects: Vec<Rect2D>,
    },
    /// `vkCmdSetLineWidth` (Metal draws lines one pixel wide; kept for completeness).
    SetLineWidth(f32),
    /// `vkCmdSetDepthBias`: constant factor, clamp, slope factor.
    SetDepthBias([f32; 3]),
    /// `vkCmdSetBlendConstants`.
    SetBlendConstants([f32; 4]),
    /// `vkCmdSetDepthBounds`: min and max (not implemented by the backend).
    SetDepthBounds([f32; 2]),
    /// `vkCmdSetStencilCompareMask`, `WriteMask`, `Reference`.
    SetStencil {
        /// The value that is set.
        what: StencilValue,
        /// `faceMask` (1 front, 2 back).
        faces: u32,
        /// The value.
        value: u32,
    },
    /// `vkCmdPushConstants`: the bytes are copied.
    PushConstants {
        /// `stageFlags`.
        stages: u32,
        /// `offset`.
        offset: u32,
        /// The bytes.
        data: Vec<u8>,
    },
    /// `vkCmdDraw`.
    Draw {
        /// `vertexCount`.
        vertices: u32,
        /// `instanceCount`.
        instances: u32,
        /// `firstVertex`.
        first_vertex: u32,
        /// `firstInstance`.
        first_instance: u32,
    },
    /// `vkCmdDrawIndexed`.
    DrawIndexed {
        /// `indexCount`.
        indices: u32,
        /// `instanceCount`.
        instances: u32,
        /// `firstIndex`.
        first_index: u32,
        /// `vertexOffset`.
        vertex_offset: i32,
        /// `firstInstance`.
        first_instance: u32,
    },
    /// `vkCmdDrawIndirect` and `vkCmdDrawIndexedIndirect`.
    DrawIndirect {
        /// The argument buffer from its offset.
        buffer: BufferRef,
        /// `drawCount`.
        count: u32,
        /// `stride`.
        stride: u32,
        /// Whether the draws are indexed.
        indexed: bool,
    },
    /// `vkCmdDispatch` (and `vkCmdDispatchBase` with a zero base).
    Dispatch([u32; 3]),
    /// `vkCmdDispatchIndirect`.
    DispatchIndirect(BufferRef),
    /// `vkCmdCopyBuffer`.
    CopyBuffer {
        /// The source buffer (from offset 0 of the buffer).
        src: BufferRef,
        /// The destination buffer.
        dst: BufferRef,
        /// The regions, relative to the buffers.
        regions: Vec<BufferCopy>,
    },
    /// `vkCmdCopyBufferToImage`.
    CopyBufferToImage {
        /// The source buffer.
        src: BufferRef,
        /// The destination image.
        dst: ImageRef,
        /// The regions.
        regions: Vec<BufferImageCopy>,
    },
    /// `vkCmdCopyImageToBuffer`.
    CopyImageToBuffer {
        /// The source image.
        src: ImageRef,
        /// The destination buffer.
        dst: BufferRef,
        /// The regions.
        regions: Vec<BufferImageCopy>,
    },
    /// `vkCmdCopyImage`.
    CopyImage {
        /// The source image.
        src: ImageRef,
        /// The destination image.
        dst: ImageRef,
        /// The regions.
        regions: Vec<ImageCopy>,
    },
    /// `vkCmdBlitImage`.
    BlitImage {
        /// The source image.
        src: ImageRef,
        /// The destination image.
        dst: ImageRef,
        /// The regions.
        regions: Vec<ImageBlit>,
        /// `filter` (0 nearest, 1 linear).
        filter: u32,
    },
    /// `vkCmdResolveImage`.
    ResolveImage {
        /// The multisample source image.
        src: ImageRef,
        /// The destination image.
        dst: ImageRef,
        /// The regions.
        regions: Vec<ImageCopy>,
    },
    /// `vkCmdFillBuffer`.
    FillBuffer {
        /// The buffer range (`VK_WHOLE_SIZE` resolved).
        dst: BufferRef,
        /// The word that is repeated.
        data: u32,
    },
    /// `vkCmdUpdateBuffer`: the bytes are copied.
    UpdateBuffer {
        /// The buffer range.
        dst: BufferRef,
        /// The bytes.
        data: Vec<u8>,
    },
    /// `vkCmdClearColorImage`.
    ClearColorImage {
        /// The image.
        image: ImageRef,
        /// The colour, raw.
        color: ClearValue,
        /// The ranges, `REMAINING` resolved.
        ranges: Vec<SubresourceRange>,
    },
    /// `vkCmdClearDepthStencilImage`.
    ClearDepthStencilImage {
        /// The image.
        image: ImageRef,
        /// The depth.
        depth: f32,
        /// The stencil.
        stencil: u32,
        /// The ranges, `REMAINING` resolved.
        ranges: Vec<SubresourceRange>,
    },
    /// `vkCmdClearAttachments`.
    ClearAttachments {
        /// The attachments.
        attachments: Vec<ClearAttachment>,
        /// The rectangles.
        rects: Vec<ClearRect>,
    },
    /// `vkCmdExecuteCommands`: secondary command buffers, replayed in place; their commands are in the submission.
    ExecuteCommands(Vec<u64>),
    /// `vkCmdResetQueryPool`.
    ResetQueryPool {
        /// The pool.
        pool: QueryPoolId,
        /// `firstQuery`.
        first: u32,
        /// `queryCount`.
        count: u32,
    },
    /// `vkCmdBeginQuery`.
    BeginQuery {
        /// The pool.
        pool: QueryPoolId,
        /// The query.
        query: u32,
        /// `flags` (`VK_QUERY_CONTROL_PRECISE_BIT`).
        flags: u32,
    },
    /// `vkCmdEndQuery`.
    EndQuery {
        /// The pool.
        pool: QueryPoolId,
        /// The query.
        query: u32,
    },
    /// `vkCmdWriteTimestamp`: the completion time of the submit.
    WriteTimestamp {
        /// The pool.
        pool: QueryPoolId,
        /// The query.
        query: u32,
    },
    /// `vkCmdCopyQueryPoolResults`: written when the submit completes.
    CopyQueryPoolResults {
        /// The pool.
        pool: QueryPoolId,
        /// `firstQuery`.
        first: u32,
        /// `queryCount`.
        count: u32,
        /// The destination from `dstOffset`.
        dst: BufferRef,
        /// `stride`.
        stride: u64,
        /// `VkQueryResultFlags`.
        flags: u32,
    },
    /// `vkCmdSetEvent`: the event is set when the submit has completed.
    SetEvent(u64),
    /// `vkCmdResetEvent`.
    ResetEvent(u64),
    /// `vkCmdPipelineBarrier` with at least one barrier: kept as a cut point where the backend may end an encoder.
    /// One serial queue runs every command in order, and Metal tracks hazards, so nothing waits here.
    PipelineBarrier,
}

impl Cmd {
    /// Whether the command draws.
    pub fn is_draw(&self) -> bool {
        matches!(
            self,
            Self::Draw { .. } | Self::DrawIndexed { .. } | Self::DrawIndirect { .. }
        )
    }

    /// The name of the Vulkan function that recorded the command, for logs.
    pub fn name(&self) -> &'static str {
        match self {
            Self::BeginRenderPass(_) => "vkCmdBeginRenderPass",
            Self::EndRenderPass => "vkCmdEndRenderPass",
            Self::BindPipeline { .. } => "vkCmdBindPipeline",
            Self::BindDescriptorSets { .. } => "vkCmdBindDescriptorSets",
            Self::BindVertexBuffers { .. } => "vkCmdBindVertexBuffers",
            Self::BindIndexBuffer { .. } => "vkCmdBindIndexBuffer",
            Self::SetViewport { .. } => "vkCmdSetViewport",
            Self::SetScissor { .. } => "vkCmdSetScissor",
            Self::SetLineWidth(_) => "vkCmdSetLineWidth",
            Self::SetDepthBias(_) => "vkCmdSetDepthBias",
            Self::SetBlendConstants(_) => "vkCmdSetBlendConstants",
            Self::SetDepthBounds(_) => "vkCmdSetDepthBounds",
            Self::SetStencil { .. } => "vkCmdSetStencil*",
            Self::PushConstants { .. } => "vkCmdPushConstants",
            Self::Draw { .. } => "vkCmdDraw",
            Self::DrawIndexed { .. } => "vkCmdDrawIndexed",
            Self::DrawIndirect { .. } => "vkCmdDraw*Indirect",
            Self::Dispatch(_) => "vkCmdDispatch",
            Self::DispatchIndirect(_) => "vkCmdDispatchIndirect",
            Self::CopyBuffer { .. } => "vkCmdCopyBuffer",
            Self::CopyBufferToImage { .. } => "vkCmdCopyBufferToImage",
            Self::CopyImageToBuffer { .. } => "vkCmdCopyImageToBuffer",
            Self::CopyImage { .. } => "vkCmdCopyImage",
            Self::BlitImage { .. } => "vkCmdBlitImage",
            Self::ResolveImage { .. } => "vkCmdResolveImage",
            Self::FillBuffer { .. } => "vkCmdFillBuffer",
            Self::UpdateBuffer { .. } => "vkCmdUpdateBuffer",
            Self::ClearColorImage { .. } => "vkCmdClearColorImage",
            Self::ClearDepthStencilImage { .. } => "vkCmdClearDepthStencilImage",
            Self::ClearAttachments { .. } => "vkCmdClearAttachments",
            Self::ExecuteCommands(_) => "vkCmdExecuteCommands",
            Self::ResetQueryPool { .. } => "vkCmdResetQueryPool",
            Self::BeginQuery { .. } => "vkCmdBeginQuery",
            Self::EndQuery { .. } => "vkCmdEndQuery",
            Self::WriteTimestamp { .. } => "vkCmdWriteTimestamp",
            Self::CopyQueryPoolResults { .. } => "vkCmdCopyQueryPoolResults",
            Self::SetEvent(_) => "vkCmdSetEvent",
            Self::ResetEvent(_) => "vkCmdResetEvent",
            Self::PipelineBarrier => "vkCmdPipelineBarrier",
        }
    }
}
