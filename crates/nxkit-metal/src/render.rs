//! Render passes: the attachments, the commands and the pass that encodes them.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSRange;
use objc2_metal::{
    MTL4CommandBuffer, MTL4CommandEncoder, MTL4RenderCommandEncoder, MTL4RenderPassDescriptor,
    MTL4VisibilityOptions, MTLClearColor, MTLCullMode, MTLRenderStages, MTLScissorRect, MTLSize,
    MTLStages, MTLViewport,
};

use crate::barrier::{Dependency, Stages};
use crate::compute_icb::check_range_buffer;
use crate::databuf::DataBuffer;
use crate::encode::CommandBuffer;
use crate::error::{MetalError, Result, create};
use crate::format::{IndexType, LoadAction, Primitive, StoreAction};
use crate::icb::IndirectCommandBuffer;
use crate::pipeline::RenderPipeline;
use crate::state::{DepthBias, DepthClip, DepthStencilState, FillMode, Winding};
use crate::table::ArgumentTable;
use crate::texture::{Storage, Texture};

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
    /// ([`CommandBuffer::begin_render_pass_split`]).
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

impl CommandBuffer {
    /// Starts a render pass. `table` is the argument table of its draws, for both stages.
    pub fn begin_render_pass(
        &mut self,
        desc: &RenderPassDesc,
        table: &ArgumentTable,
    ) -> Result<RenderPass<'_>> {
        self.begin_render_pass_with(desc, table, None)
    }

    /// Starts a render pass with one argument table per stage. A translated shader reads its argument buffer at
    /// `[[buffer(0)]]` in both stages, and the two stages have different buffers.
    pub fn begin_render_pass_split(
        &mut self,
        desc: &RenderPassDesc,
        vertex_table: &ArgumentTable,
        fragment_table: &ArgumentTable,
    ) -> Result<RenderPass<'_>> {
        self.begin_render_pass_with(desc, vertex_table, Some(fragment_table))
    }

    fn begin_render_pass_with(
        &mut self,
        desc: &RenderPassDesc,
        table: &ArgumentTable,
        fragment_table: Option<&ArgumentTable>,
    ) -> Result<RenderPass<'_>> {
        let first = desc
            .colors
            .iter()
            .flatten()
            .next()
            .map(|c| &c.texture)
            .or(desc.depth.as_ref().map(|d| &d.texture))
            .ok_or(MetalError::Invalid("a render pass needs an attachment"))?;
        let mut pd = self.pass_desc.take().unwrap_or_else(PassDescriptor::new);
        let result = self.fill_pass(&mut pd, desc, first);
        let rp = pd.raw.clone();
        self.pass_desc = Some(pd);
        result?;
        let enc = self
            .raw
            .renderCommandEncoderWithDescriptor(&rp)
            .ok_or_else(|| {
                create(
                    "render encoder",
                    "renderCommandEncoderWithDescriptor returned nil",
                )
            })?;
        match fragment_table {
            Some(ft) => {
                // The mesh stage takes the place of the vertex stage, so it shares its table.
                enc.setArgumentTable_atStages(
                    Some(&table.raw),
                    MTLRenderStages::Vertex | MTLRenderStages::Mesh,
                );
                enc.setArgumentTable_atStages(Some(&ft.raw), MTLRenderStages::Fragment);
                self.retain(ft.clone());
            }
            None => enc.setArgumentTable_atStages(
                Some(&table.raw),
                MTLRenderStages::Vertex | MTLRenderStages::Mesh | MTLRenderStages::Fragment,
            ),
        }
        self.retain(table.clone());
        self.encode_passes += 1;
        Ok(RenderPass {
            enc,
            cb: self,
            table: table.clone(),
            fragment_table: fragment_table.cloned(),
            object_table: None,
            has_pipeline: false,
            ended: false,
        })
    }

    /// Writes `desc` into the reusable descriptor `pd`. The attachments of the previous pass are emptied first.
    fn fill_pass(
        &mut self,
        pd: &mut PassDescriptor,
        desc: &RenderPassDesc,
        first: &Texture,
    ) -> Result<()> {
        pd.reset();
        let rp = &pd.raw;
        let slots = rp.colorAttachments();
        for (i, c) in desc.colors.iter().enumerate() {
            let Some(c) = c else { continue };
            check_attachment(&c.texture, c.load, c.store, c.memoryless)?;
            pd.colors = pd.colors.max(i + 1);
            // SAFETY: the index is a plain slot. Metal checks the attachment limit when the encoder starts.
            let a = unsafe { slots.objectAtIndexedSubscript(i) };
            a.setTexture(Some(c.texture.raw()));
            a.setDepthPlane(c.depth_plane as usize);
            a.setLoadAction(c.load.to_mtl());
            a.setStoreAction(c.store.to_mtl());
            a.setClearColor(MTLClearColor {
                red: c.clear[0],
                green: c.clear[1],
                blue: c.clear[2],
                alpha: c.clear[3],
            });
            self.retain(c.texture.clone());
        }
        if let Some(d) = &desc.depth {
            check_attachment(&d.texture, d.load, d.store, d.memoryless)?;
            pd.depth = true;
            let a = rp.depthAttachment();
            a.setTexture(Some(d.texture.raw()));
            a.setLoadAction(d.load.to_mtl());
            a.setStoreAction(d.store.to_mtl());
            a.setClearDepth(d.clear);
            self.retain(d.texture.clone());
        }
        if let Some(d) = &desc.stencil {
            check_attachment(&d.texture, d.load, d.store, d.memoryless)?;
            if !d.texture.desc().format.has_stencil() {
                return Err(MetalError::Invalid(
                    "the stencil texture has no stencil aspect",
                ));
            }
            pd.stencil = true;
            let a = rp.stencilAttachment();
            a.setTexture(Some(d.texture.raw()));
            a.setLoadAction(d.load.to_mtl());
            a.setStoreAction(d.store.to_mtl());
            a.setClearStencil(d.clear);
            self.retain(d.texture.clone());
        }
        // The render area is the smallest attachment, as in NVN.
        let td = first.desc();
        let (mut w, mut h) = (td.width, td.height);
        for t in desc
            .colors
            .iter()
            .flatten()
            .map(|c| &c.texture)
            .chain(desc.depth.as_ref().map(|d| &d.texture))
        {
            let d = t.desc();
            w = w.min(d.width);
            h = h.min(d.height);
        }
        rp.setRenderTargetWidth(w as usize);
        rp.setRenderTargetHeight(h as usize);
        Ok(())
    }
}

/// A render pass descriptor that is filled again for every pass of a command buffer. Metal copies a descriptor when
/// an encoder starts, so one object serves all passes.
pub(crate) struct PassDescriptor {
    raw: Retained<MTL4RenderPassDescriptor>,
    /// The colour slots below this index may hold a texture.
    colors: usize,
    depth: bool,
    stencil: bool,
}

impl PassDescriptor {
    fn new() -> Self {
        Self {
            raw: MTL4RenderPassDescriptor::new(),
            colors: 0,
            depth: false,
            stencil: false,
        }
    }

    /// Empties the attachments that the previous pass set.
    fn reset(&mut self) {
        let slots = self.raw.colorAttachments();
        for i in 0..self.colors {
            // SAFETY: the index is a slot that an earlier pass set, so it is inside the attachment limit.
            let a = unsafe { slots.objectAtIndexedSubscript(i) };
            a.setTexture(None);
        }
        if self.depth {
            self.raw.depthAttachment().setTexture(None);
        }
        if self.stencil {
            self.raw.stencilAttachment().setTexture(None);
        }
        (self.colors, self.depth, self.stencil) = (0, false, false);
    }
}

/// A render pass. End it with [`RenderPass::end`]. Dropping it ends it too.
pub struct RenderPass<'a> {
    enc: Retained<ProtocolObject<dyn MTL4RenderCommandEncoder>>,
    cb: &'a mut CommandBuffer,
    table: ArgumentTable,
    fragment_table: Option<ArgumentTable>,
    object_table: Option<ArgumentTable>,
    has_pipeline: bool,
    ended: bool,
}

/// Checks that the GPU address of indirect arguments has the 4 byte alignment that Metal requires.
fn check_indirect(addr: u64) -> Result<()> {
    if addr == 0 || !addr.is_multiple_of(4) {
        return Err(MetalError::Invalid(
            "the indirect arguments are not at a 4 byte aligned address",
        ));
    }
    Ok(())
}

fn size3(v: [u32; 3]) -> MTLSize {
    MTLSize {
        width: v[0] as usize,
        height: v[1] as usize,
        depth: v[2] as usize,
    }
}

fn threads(n: u32) -> MTLSize {
    size3([n.max(1), 1, 1])
}

impl RenderPass<'_> {
    /// Binds `table` at the object stage, for the task shaders of mesh draws. The pass keeps the table until it ends.
    /// A later call replaces it.
    pub fn set_object_table(&mut self, table: &ArgumentTable) {
        self.enc
            .setArgumentTable_atStages(Some(&table.raw), MTLRenderStages::Object);
        self.cb.retain(table.clone());
        self.object_table = Some(table.clone());
    }

    /// True when [`RenderPass::set_object_table`] gave the pass an object stage table.
    pub fn has_object_table(&self) -> bool {
        self.object_table.is_some()
    }

    /// Names the encoder for Instruments and the Metal debugger.
    pub fn set_label(&self, label: &str) {
        use objc2_metal::MTL4CommandEncoder as _;
        self.enc
            .setLabel(Some(&objc2_foundation::NSString::from_str(label)));
    }

    /// Encodes one command.
    pub fn encode(&mut self, cmd: &RenderCommand) -> Result<()> {
        match cmd {
            RenderCommand::SetPipeline(p) => {
                self.enc.setRenderPipelineState(&p.raw);
                self.cb.retain(p.clone());
                self.has_pipeline = true;
            }
            RenderCommand::SetBuffer { index, gpu_addr } => {
                self.table.set_buffer(*index, *gpu_addr)?
            }
            RenderCommand::SetTexture { index, texture } => {
                self.table.set_texture(*index, texture)?
            }
            RenderCommand::SetFragmentBuffer { index, gpu_addr } => self
                .fragment_table
                .as_ref()
                .ok_or(MetalError::Invalid(
                    "the pass has no separate fragment argument table",
                ))?
                .set_buffer(*index, *gpu_addr)?,
            RenderCommand::SetObjectBuffer { index, gpu_addr } => self
                .object_table
                .as_ref()
                .ok_or(MetalError::Invalid("the pass has no object argument table"))?
                .set_buffer(*index, *gpu_addr)?,
            RenderCommand::SetViewport(v) => self.enc.setViewport(MTLViewport {
                originX: v.x,
                originY: v.y,
                width: v.width,
                height: v.height,
                znear: v.z_near,
                zfar: v.z_far,
            }),
            RenderCommand::SetScissor(s) => self.enc.setScissorRect(MTLScissorRect {
                x: s.x as usize,
                y: s.y as usize,
                width: s.width as usize,
                height: s.height as usize,
            }),
            RenderCommand::SetCullMode(m) => self.enc.setCullMode(match m {
                CullMode::None => MTLCullMode::None,
                CullMode::Front => MTLCullMode::Front,
                CullMode::Back => MTLCullMode::Back,
            }),
            RenderCommand::SetDepthStencil(d) => {
                self.enc.setDepthStencilState(Some(&d.raw));
                self.cb.retain(d.clone());
            }
            RenderCommand::SetWinding(w) => self.enc.setFrontFacingWinding(w.to_mtl()),
            RenderCommand::SetFillMode(f) => self.enc.setTriangleFillMode(f.to_mtl()),
            RenderCommand::SetDepthBias(b) => {
                self.enc
                    .setDepthBias_slopeScale_clamp(b.constant, b.slope_scale, b.clamp)
            }
            RenderCommand::SetDepthClip(c) => self.enc.setDepthClipMode(c.to_mtl()),
            RenderCommand::SetStencilRef { front, back } => self
                .enc
                .setStencilFrontReferenceValue_backReferenceValue(*front, *back),
            RenderCommand::SetBlendColor(c) => self
                .enc
                .setBlendColorRed_green_blue_alpha(c[0], c[1], c[2], c[3]),
            RenderCommand::SetDepthBounds { min, max } => {
                self.enc.setDepthTestMinBound_maxBound(*min, *max)
            }
            RenderCommand::Depend(d) => d.encode(
                &*self.enc,
                Stages::VERTEX | Stages::FRAGMENT | Stages::OBJECT | Stages::MESH,
            )?,
            RenderCommand::WaitForEarlierPasses => self
                .enc
                .barrierAfterQueueStages_beforeStages_visibilityOptions(
                    MTLStages::All,
                    MTLStages::Vertex | MTLStages::Object | MTLStages::Mesh | MTLStages::Fragment,
                    MTL4VisibilityOptions::Device,
                ),
            RenderCommand::ExecuteIcb { icb, first, count } => {
                if u64::from(*first) + u64::from(*count) > u64::from(icb.size()) {
                    return Err(MetalError::Invalid("the ICB range is out of range"));
                }
                // SAFETY: the range is inside the ICB (checked above). The ICB is retained until the GPU is done
                // and it is in the residency set.
                unsafe {
                    self.enc.executeCommandsInBuffer_withRange(
                        &icb.raw,
                        NSRange::new(*first as usize, *count as usize),
                    );
                }
                self.cb.retain(icb.clone());
            }
            RenderCommand::ExecuteIcbIndirect {
                icb,
                range,
                range_offset,
            } => {
                check_range_buffer(range, *range_offset)?;
                // SAFETY: the range lies inside the data buffer (checked above). The GPU reads it when the command
                // executes, so the caller keeps it valid until then. Metal does not bounds check the range it
                // reads, and the caller writes one inside the ICB.
                unsafe {
                    self.enc.executeCommandsInBuffer_indirectBuffer(
                        &icb.raw,
                        range.gpu_address() + *range_offset,
                    );
                }
                self.cb.retain(icb.clone());
                self.cb.retain(range.clone());
            }
            RenderCommand::Draw {
                primitive,
                first_vertex,
                vertex_count,
                instance_count,
                base_instance,
            } => {
                self.need_pipeline()?;
                // SAFETY: a pipeline is bound and the argument table is set (`need_pipeline`, `begin_render_pass`).
                // The counts are plain data.
                unsafe {
                    self.enc
                        .drawPrimitives_vertexStart_vertexCount_instanceCount_baseInstance(
                            primitive.to_mtl(),
                            *first_vertex as usize,
                            *vertex_count as usize,
                            *instance_count as usize,
                            *base_instance as usize,
                        );
                }
            }
            RenderCommand::DrawIndexed {
                primitive,
                index_type,
                index_addr,
                index_bytes,
                index_count,
                instance_count,
                base_vertex,
                base_instance,
            } => {
                self.need_pipeline()?;
                if u64::from(*index_count) * index_type.bytes() > *index_bytes {
                    return Err(MetalError::Invalid(
                        "the index range is longer than the index buffer",
                    ));
                }
                // SAFETY: a pipeline is bound (`need_pipeline`) and the index range is inside the length given
                // to Metal (checked above).
                unsafe {
                    self.enc.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferLength_instanceCount_baseVertex_baseInstance(
                        primitive.to_mtl(),
                        *index_count as usize,
                        index_type.to_mtl(),
                        *index_addr,
                        *index_bytes as usize,
                        *instance_count as usize,
                        *base_vertex as isize,
                        *base_instance as usize,
                    );
                }
            }
            RenderCommand::DrawIndirect {
                primitive,
                indirect_addr,
            } => {
                self.need_pipeline()?;
                check_indirect(*indirect_addr)?;
                // The GPU reads the arguments when the draw runs. A wrong address makes the GPU fault, which is not a
                // memory safety matter for the host; the alignment is checked above.
                self.enc
                    .drawPrimitives_indirectBuffer(primitive.to_mtl(), *indirect_addr);
            }
            RenderCommand::DrawIndexedIndirect {
                primitive,
                index_type,
                index_addr,
                index_bytes,
                indirect_addr,
            } => {
                self.need_pipeline()?;
                check_indirect(*indirect_addr)?;
                if !index_addr.is_multiple_of(index_type.bytes()) {
                    return Err(MetalError::Invalid(
                        "the index buffer is not aligned to its index width",
                    ));
                }
                // The length that Metal gets is a whole number of indices.
                let bytes = *index_bytes - *index_bytes % index_type.bytes();
                // SAFETY: a pipeline is bound (`need_pipeline`), the index buffer is aligned to its index width and its
                // length is a multiple of it (checked above). Metal reads an index past `bytes` as 0 and the GPU reads
                // the arguments at the aligned address when the draw runs.
                unsafe {
                    self.enc.drawIndexedPrimitives_indexType_indexBuffer_indexBufferLength_indirectBuffer(
                        primitive.to_mtl(),
                        index_type.to_mtl(),
                        *index_addr,
                        bytes as usize,
                        *indirect_addr,
                    );
                }
            }
            RenderCommand::DrawMeshThreadgroups {
                groups,
                object_threads,
                mesh_threads,
            } => {
                self.need_pipeline()?;
                if groups.contains(&0) {
                    return Ok(());
                }
                self.enc
                    .drawMeshThreadgroups_threadsPerObjectThreadgroup_threadsPerMeshThreadgroup(
                        size3(*groups),
                        threads(*object_threads),
                        threads(*mesh_threads),
                    );
            }
            RenderCommand::DrawMeshThreadgroupsIndirect {
                indirect_addr,
                object_threads,
                mesh_threads,
            } => {
                self.need_pipeline()?;
                check_indirect(*indirect_addr)?;
                self.enc.drawMeshThreadgroupsWithIndirectBuffer_threadsPerObjectThreadgroup_threadsPerMeshThreadgroup(
                    *indirect_addr,
                    threads(*object_threads),
                    threads(*mesh_threads),
                );
            }
        }
        Ok(())
    }

    fn need_pipeline(&self) -> Result<()> {
        if self.has_pipeline {
            Ok(())
        } else {
            Err(MetalError::Invalid("draw before a pipeline is set"))
        }
    }

    /// Ends the pass.
    pub fn end(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        if !self.ended {
            self.enc.endEncoding();
            self.ended = true;
        }
    }
}

impl Drop for RenderPass<'_> {
    fn drop(&mut self) {
        self.finish();
    }
}
