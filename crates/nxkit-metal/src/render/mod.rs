//! Render passes: the attachments, the commands and the pass that encodes them.

use std::sync::Arc;

use crate::barrier::Dependency;
use crate::databuf::DataBuffer;
use crate::error::{MetalError, Result};
use crate::format::{IndexType, LoadAction, Primitive, StoreAction};
use crate::icb::IndirectCommandBuffer;
use crate::pipeline::RenderPipeline;
use crate::state::{DepthBias, DepthClip, DepthStencilState, FillMode, Winding};
use crate::texture::{Storage, Texture};

pub(crate) use pass::PassDescriptor;
pub use pass::RenderPass;

mod pass;

/// A viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// The left edge.
    pub x: f64,
    /// The top edge.
    pub y: f64,
    /// The width.
    pub width: f64,
    /// The height.
    pub height: f64,
    /// The near depth.
    pub z_near: f64,
    /// The far depth.
    pub z_far: f64,
}

/// A scissor rectangle in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scissor {
    /// The left edge.
    pub x: u32,
    /// The top edge.
    pub y: u32,
    /// The width.
    pub width: u32,
    /// The height.
    pub height: u32,
}

/// Which faces are not drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CullMode {
    /// Nothing is culled.
    None,
    /// Front faces are culled.
    Front,
    /// Back faces are culled.
    Back,
}

/// One colour attachment of a render pass.
#[derive(Clone)]
pub struct ColorAttachment {
    /// The texture.
    pub texture: Texture,
    /// The depth plane when rendering a slice of a 3D texture; zero for other texture kinds.
    pub depth_plane: u32,
    /// The action at the start.
    pub load: LoadAction,
    /// The action at the end.
    pub store: StoreAction,
    /// The clear colour, used with `LoadAction::Clear`.
    pub clear: [f64; 4],
    /// True when the texture is memoryless. It must match the texture and the store action must drop the content.
    pub memoryless: bool,
}

/// The depth attachment of a render pass.
#[derive(Clone)]
pub struct DepthAttachment {
    /// The texture.
    pub texture: Texture,
    /// The action at the start.
    pub load: LoadAction,
    /// The action at the end.
    pub store: StoreAction,
    /// The clear depth, used with `LoadAction::Clear`.
    pub clear: f64,
    /// True when the texture is memoryless.
    pub memoryless: bool,
}

/// The stencil attachment of a render pass. The texture is usually the depth texture of a combined format.
#[derive(Clone)]
pub struct StencilAttachment {
    /// The texture.
    pub texture: Texture,
    /// The action at the start.
    pub load: LoadAction,
    /// The action at the end.
    pub store: StoreAction,
    /// The clear value, used with `LoadAction::Clear`.
    pub clear: u32,
    /// True when the texture is memoryless.
    pub memoryless: bool,
}

/// The attachments of a render pass.
#[derive(Clone, Default)]
pub struct RenderPassDesc {
    /// The colour attachments by slot. `None` leaves a slot empty, as NVN allows.
    pub colors: Vec<Option<ColorAttachment>>,
    /// The depth attachment.
    pub depth: Option<DepthAttachment>,
    /// The stencil attachment.
    pub stencil: Option<StencilAttachment>,
}

fn check_attachment(
    tex: &Texture,
    load: LoadAction,
    store: StoreAction,
    memoryless: bool,
) -> Result<()> {
    let d = tex.desc();
    if !d.render_target {
        return Err(MetalError::Invalid(
            "the attachment texture is not a render target",
        ));
    }
    if memoryless != (d.storage == Storage::Memoryless) {
        return Err(MetalError::Invalid(
            "the memoryless flag does not match the texture",
        ));
    }
    if memoryless && (store != StoreAction::DontCare || load == LoadAction::Load) {
        return Err(MetalError::Invalid(
            "a memoryless attachment cannot load or store",
        ));
    }
    Ok(())
}

/// A command for a render pass.
#[derive(Clone)]
pub enum RenderCommand {
    /// Binds a pipeline.
    SetPipeline(RenderPipeline),
    /// Binds a GPU address to a buffer slot of the pass's argument table.
    SetBuffer {
        /// The slot.
        index: u32,
        /// The GPU address.
        gpu_addr: u64,
    },
    /// Binds a texture to a texture slot of the pass's argument table.
    SetTexture {
        /// The slot.
        index: u32,
        /// The texture.
        texture: Texture,
    },
    /// Binds a GPU address to a buffer slot of the fragment argument table of a split pass
    /// ([`CommandBuffer::begin_render_pass_split`](crate::encode::CommandBuffer::begin_render_pass_split)).
    SetFragmentBuffer {
        /// The slot.
        index: u32,
        /// The GPU address.
        gpu_addr: u64,
    },
    /// Binds a GPU address to a buffer slot of the object stage table ([`RenderPass::set_object_table`]). The mesh
    /// stage shares the table of the vertex stage, so [`RenderCommand::SetBuffer`] binds its buffers.
    SetObjectBuffer {
        /// The slot.
        index: u32,
        /// The GPU address.
        gpu_addr: u64,
    },
    /// Sets the viewport.
    SetViewport(Viewport),
    /// Sets the scissor rectangle.
    SetScissor(Scissor),
    /// Sets the cull mode.
    SetCullMode(CullMode),
    /// Binds a depth and stencil state.
    SetDepthStencil(DepthStencilState),
    /// Sets which winding order is a front face.
    SetWinding(Winding),
    /// Sets how triangles are filled.
    SetFillMode(FillMode),
    /// Sets the depth bias.
    SetDepthBias(DepthBias),
    /// Sets whether fragments outside the depth range are clipped or clamped.
    SetDepthClip(DepthClip),
    /// Sets the stencil reference values of front and back faces.
    SetStencilRef {
        /// The value for front faces.
        front: u32,
        /// The value for back faces.
        back: u32,
    },
    /// Sets the constant blend colour.
    SetBlendColor([f32; 4]),
    /// Sets the depth bounds test range.
    SetDepthBounds {
        /// The lower bound.
        min: f32,
        /// The upper bound.
        max: f32,
    },
    /// Counts the samples of the draws after it that pass the depth and stencil tests into the 8 bytes at `offset` of
    /// the visibility result buffer of the pass (`CommandBuffer::begin_render_pass_counted`), or stops counting.
    SetVisibility {
        /// True to count, false to stop.
        counting: bool,
        /// The byte offset, a multiple of 8. Ignored when `counting` is false.
        offset: u64,
    },
    /// Orders this pass after the work of earlier passes of the command buffer.
    WaitForEarlierPasses,
    /// A barrier with explicit stages. The stages must be `VERTEX` or `FRAGMENT` on the side of this pass.
    Depend(Dependency),
    /// Runs `count` commands of an ICB from `first`. The ICB, its pipelines and buffers stay alive until the GPU is done.
    ExecuteIcb {
        /// The ICB.
        icb: IndirectCommandBuffer,
        /// The first command.
        first: u32,
        /// The number of commands.
        count: u32,
    },
    /// Runs the range of an ICB that the GPU reads from a buffer when the command executes.
    ExecuteIcbIndirect {
        /// The ICB.
        icb: IndirectCommandBuffer,
        /// The buffer that holds an [`ExecutionRange`](crate::ExecutionRange) at `range_offset`.
        range: Arc<DataBuffer>,
        /// The byte offset of the range in `range`.
        range_offset: u64,
    },
    /// Draws primitives.
    Draw {
        /// The topology.
        primitive: Primitive,
        /// The first vertex.
        first_vertex: u32,
        /// The number of vertices.
        vertex_count: u32,
        /// The number of instances.
        instance_count: u32,
        /// The first instance.
        base_instance: u32,
    },
    /// Draws primitives with the arguments that the GPU reads from `indirect_addr` when the draw runs: an
    /// `MTLDrawPrimitivesIndirectArguments` (vertex count, instance count, first vertex, first instance).
    DrawIndirect {
        /// The topology.
        primitive: Primitive,
        /// The GPU address of the arguments, 4 byte aligned.
        indirect_addr: u64,
    },
    /// Draws indexed primitives with the arguments that the GPU reads from `indirect_addr` when the draw runs: an
    /// `MTLDrawIndexedPrimitivesIndirectArguments` (index count, instance count, first index, base vertex, first
    /// instance).
    DrawIndexedIndirect {
        /// The topology.
        primitive: Primitive,
        /// The width of an index.
        index_type: IndexType,
        /// The GPU address of index 0.
        index_addr: u64,
        /// The bytes available at `index_addr`. Metal reads an index past them as 0.
        index_bytes: u64,
        /// The GPU address of the arguments, 4 byte aligned.
        indirect_addr: u64,
    },
    /// Runs the mesh pipeline over a grid of threadgroups: the object stage, when the pipeline has one, or else the
    /// mesh stage.
    DrawMeshThreadgroups {
        /// The threadgroups of the grid.
        groups: [u32; 3],
        /// The threads of an object threadgroup. Ignored without an object stage.
        object_threads: u32,
        /// The threads of a mesh threadgroup.
        mesh_threads: u32,
    },
    /// Runs the mesh pipeline over the grid that the GPU reads from `indirect_addr` when the draw runs: an
    /// `MTLDispatchThreadgroupsIndirectArguments` (three counts).
    DrawMeshThreadgroupsIndirect {
        /// The GPU address of the grid, 4 byte aligned.
        indirect_addr: u64,
        /// The threads of an object threadgroup. Ignored without an object stage.
        object_threads: u32,
        /// The threads of a mesh threadgroup.
        mesh_threads: u32,
    },
    /// Draws primitives with indices from a GPU address.
    DrawIndexed {
        /// The topology.
        primitive: Primitive,
        /// The width of an index.
        index_type: IndexType,
        /// The GPU address of the first index.
        index_addr: u64,
        /// The bytes available at `index_addr`.
        index_bytes: u64,
        /// The number of indices.
        index_count: u32,
        /// The number of instances.
        instance_count: u32,
        /// The value added to every index.
        base_vertex: i32,
        /// The first instance.
        base_instance: u32,
    },
}
