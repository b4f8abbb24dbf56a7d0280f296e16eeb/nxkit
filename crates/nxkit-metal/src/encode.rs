//! A narrow, safe encoding API over `MTL4CommandBuffer` and its encoders.
//!
//! The API is data driven. A caller builds [`RenderCommand`] and [`ComputeCommand`] values and feeds them to a
//! pass with `encode`, so the frame encoder is a loop over recorded commands. The pass checks what Metal would
//! otherwise trust, for example that a pipeline is bound before a draw.
//!
//! The GPU addresses in commands point into the guest buffer or into a scratch buffer. Both are resident.

use std::any::Any;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTL4CommandBuffer, MTL4CommandEncoder, MTL4ComputeCommandEncoder, MTL4RenderCommandEncoder,
    MTL4RenderPassDescriptor, MTL4VisibilityOptions, MTLClearColor, MTLCullMode, MTLOrigin,
    MTLRenderStages, MTLScissorRect, MTLSize, MTLStages, MTLViewport,
};

use crate::error::{MetalError, Result, create};
use crate::format::{IndexType, LoadAction, Primitive, StoreAction};
use crate::guest::GuestMemory;
use crate::pipeline::{ComputePipeline, RenderPipeline};
use crate::table::{ArgumentTable, Keep};
use crate::texture::{Storage, Texture};

pub(crate) type RawCommandBuffer = Retained<ProtocolObject<dyn MTL4CommandBuffer>>;
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

/// The attachments of a render pass.
#[derive(Clone, Default)]
pub struct RenderPassDesc {
    /// The colour attachments, in slot order.
    pub colors: Vec<ColorAttachment>,
    /// The depth attachment.
    pub depth: Option<DepthAttachment>,
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
    /// Sets the viewport.
    SetViewport(Viewport),
    /// Sets the scissor rectangle.
    SetScissor(Scissor),
    /// Sets the cull mode.
    SetCullMode(CullMode),
    /// Orders this pass after the work of earlier passes of the command buffer.
    WaitForEarlierPasses,
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

/// A command for a compute pass.
#[derive(Clone)]
pub enum ComputeCommand {
    /// Binds a pipeline.
    SetPipeline(ComputePipeline),
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
    /// Runs whole threadgroups.
    DispatchThreadgroups {
        /// The threadgroups per grid.
        groups: [u32; 3],
        /// The threads per threadgroup.
        threads: [u32; 3],
    },
    /// Runs `grid` threads, with partial threadgroups at the edge.
    DispatchThreads {
        /// The threads per grid.
        grid: [u32; 3],
        /// The threads per threadgroup.
        threads: [u32; 3],
    },
    /// Orders this pass after the work of earlier passes of the command buffer.
    WaitForEarlierPasses,
    /// Orders the later commands of this pass after the earlier ones.
    Barrier,
    /// Copies a texture to guest memory. This is the copy-back for a game that reads a render target.
    CopyTextureToGuest {
        /// The texture. It must have finished rendering in an earlier pass.
        texture: Texture,
        /// The guest memory.
        guest: Arc<GuestMemory>,
        /// The destination, as an offset from the start of the guest memory.
        offset: u64,
        /// The bytes between two rows.
        bytes_per_row: u32,
    },
}

/// A Metal 4 command buffer that is being encoded.
pub struct CommandBuffer {
    raw: RawCommandBuffer,
    retained: Vec<Keep>,
    pub(crate) encode_passes: u32,
}

impl CommandBuffer {
    pub(crate) fn new(raw: RawCommandBuffer) -> Self {
        Self {
            raw,
            retained: Vec::new(),
            encode_passes: 0,
        }
    }

    pub(crate) fn raw(&self) -> &RawCommandBuffer {
        &self.raw
    }

    pub(crate) fn take_retained(&mut self) -> Vec<Keep> {
        std::mem::take(&mut self.retained)
    }

    /// Keeps a value alive until the GPU finished the command buffer.
    pub fn retain(&mut self, v: impl Any) {
        self.retained.push(Keep::new(v));
    }

    /// Starts a render pass. `table` is the argument table of its draws.
    pub fn begin_render_pass(
        &mut self,
        desc: &RenderPassDesc,
        table: &ArgumentTable,
    ) -> Result<RenderPass<'_>> {
        let first = desc
            .colors
            .first()
            .map(|c| &c.texture)
            .or(desc.depth.as_ref().map(|d| &d.texture))
            .ok_or(MetalError::Invalid("a render pass needs an attachment"))?;
        let rp = MTL4RenderPassDescriptor::new();
        let slots = rp.colorAttachments();
        for (i, c) in desc.colors.iter().enumerate() {
            check_attachment(&c.texture, c.load, c.store, c.memoryless)?;
            // SAFETY: the index is a plain slot. Metal checks the attachment limit when the encoder starts.
            let a = unsafe { slots.objectAtIndexedSubscript(i) };
            a.setTexture(Some(c.texture.raw()));
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
            let a = rp.depthAttachment();
            a.setTexture(Some(d.texture.raw()));
            a.setLoadAction(d.load.to_mtl());
            a.setStoreAction(d.store.to_mtl());
            a.setClearDepth(d.clear);
            self.retain(d.texture.clone());
        }
        let td = first.desc();
        rp.setRenderTargetWidth(td.width as usize);
        rp.setRenderTargetHeight(td.height as usize);
        let enc = self
            .raw
            .renderCommandEncoderWithDescriptor(&rp)
            .ok_or_else(|| {
                create(
                    "render encoder",
                    "renderCommandEncoderWithDescriptor returned nil",
                )
            })?;
        enc.setArgumentTable_atStages(
            Some(&table.raw),
            MTLRenderStages::Vertex | MTLRenderStages::Fragment,
        );
        self.retain(table.clone());
        self.encode_passes += 1;
        Ok(RenderPass {
            enc,
            cb: self,
            table: table.clone(),
            has_pipeline: false,
            ended: false,
        })
    }

    /// Starts a compute pass. `table` is the argument table of its dispatches.
    pub fn begin_compute_pass(&mut self, table: &ArgumentTable) -> Result<ComputePass<'_>> {
        let enc = self
            .raw
            .computeCommandEncoder()
            .ok_or_else(|| create("compute encoder", "computeCommandEncoder returned nil"))?;
        enc.setArgumentTable(Some(&table.raw));
        self.retain(table.clone());
        self.encode_passes += 1;
        Ok(ComputePass {
            enc,
            cb: self,
            table: table.clone(),
            has_pipeline: false,
            ended: false,
        })
    }
}

/// A render pass. End it with [`RenderPass::end`]. Dropping it ends it too.
pub struct RenderPass<'a> {
    enc: Retained<ProtocolObject<dyn MTL4RenderCommandEncoder>>,
    cb: &'a mut CommandBuffer,
    table: ArgumentTable,
    has_pipeline: bool,
    ended: bool,
}

impl RenderPass<'_> {
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
            RenderCommand::WaitForEarlierPasses => self
                .enc
                .barrierAfterQueueStages_beforeStages_visibilityOptions(
                    MTLStages::All,
                    MTLStages::Vertex | MTLStages::Fragment,
                    MTL4VisibilityOptions::Device,
                ),
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

/// A compute pass. End it with [`ComputePass::end`]. Dropping it ends it too.
pub struct ComputePass<'a> {
    enc: Retained<ProtocolObject<dyn MTL4ComputeCommandEncoder>>,
    cb: &'a mut CommandBuffer,
    table: ArgumentTable,
    has_pipeline: bool,
    ended: bool,
}

fn size3(v: [u32; 3]) -> MTLSize {
    MTLSize {
        width: v[0] as usize,
        height: v[1] as usize,
        depth: v[2] as usize,
    }
}

impl ComputePass<'_> {
    /// Keeps a value alive until the GPU finished the command buffer.
    pub fn retain(&mut self, v: impl Any) {
        self.cb.retain(v);
    }

    /// Encodes one command.
    pub fn encode(&mut self, cmd: &ComputeCommand) -> Result<()> {
        match cmd {
            ComputeCommand::SetPipeline(p) => {
                self.enc.setComputePipelineState(&p.raw);
                self.cb.retain(p.clone());
                self.has_pipeline = true;
            }
            ComputeCommand::SetBuffer { index, gpu_addr } => {
                self.table.set_buffer(*index, *gpu_addr)?
            }
            ComputeCommand::SetTexture { index, texture } => {
                self.table.set_texture(*index, texture)?
            }
            ComputeCommand::DispatchThreadgroups { groups, threads } => {
                self.need_pipeline()?;
                self.enc
                    .dispatchThreadgroups_threadsPerThreadgroup(size3(*groups), size3(*threads));
            }
            ComputeCommand::DispatchThreads { grid, threads } => {
                self.need_pipeline()?;
                self.enc
                    .dispatchThreads_threadsPerThreadgroup(size3(*grid), size3(*threads));
            }
            ComputeCommand::WaitForEarlierPasses => self
                .enc
                .barrierAfterQueueStages_beforeStages_visibilityOptions(
                    MTLStages::All,
                    MTLStages::Dispatch | MTLStages::Blit,
                    MTL4VisibilityOptions::Device,
                ),
            ComputeCommand::Barrier => self
                .enc
                .barrierAfterEncoderStages_beforeEncoderStages_visibilityOptions(
                    MTLStages::Dispatch | MTLStages::Blit,
                    MTLStages::Dispatch | MTLStages::Blit,
                    MTL4VisibilityOptions::Device,
                ),
            ComputeCommand::CopyTextureToGuest {
                texture,
                guest,
                offset,
                bytes_per_row,
            } => self.copy_to_guest(texture, guest, *offset, *bytes_per_row)?,
        }
        Ok(())
    }

    fn copy_to_guest(
        &mut self,
        tex: &Texture,
        guest: &Arc<GuestMemory>,
        offset: u64,
        bpr: u32,
    ) -> Result<()> {
        let d = tex.desc();
        let row = u64::from(d.width) * u64::from(d.format.bytes_per_pixel());
        if u64::from(bpr) < row || u64::from(bpr) % u64::from(d.format.bytes_per_pixel()) != 0 {
            return Err(MetalError::Invalid(
                "bytes per row is too small or not a whole number of pixels",
            ));
        }
        let bytes = u64::from(bpr) * u64::from(d.height);
        guest.gpu_address_of_offset(offset, bytes)?;
        // SAFETY: the destination range is inside the guest buffer (checked above), the source is the whole
        // level 0 of a texture that outlives the pass (`retain` below).
        unsafe {
            self.enc.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                tex.raw(),
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
                MTLSize { width: d.width as usize, height: d.height as usize, depth: 1 },
                guest.buffer(),
                offset as usize,
                bpr as usize,
                bytes as usize,
            );
        }
        self.cb.retain(tex.clone());
        self.cb.retain(guest.clone());
        Ok(())
    }

    fn need_pipeline(&self) -> Result<()> {
        if self.has_pipeline {
            Ok(())
        } else {
            Err(MetalError::Invalid("dispatch before a pipeline is set"))
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

impl Drop for ComputePass<'_> {
    fn drop(&mut self) {
        self.finish();
    }
}
