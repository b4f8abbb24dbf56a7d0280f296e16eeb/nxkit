//! Render passes: the attachments, the commands and the pass that encodes them.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSRange;
use objc2_metal::{
    MTL4CommandBuffer, MTL4CommandEncoder, MTL4RenderCommandEncoder, MTL4RenderPassDescriptor,
    MTL4VisibilityOptions, MTLClearColor, MTLCullMode, MTLRenderStages, MTLScissorRect, MTLStages,
    MTLViewport,
};

use crate::barrier::{Dependency, Stages};
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
    /// The colour attachments, in slot order.
    pub colors: Vec<ColorAttachment>,
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

impl CommandBuffer {
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
        if let Some(d) = &desc.stencil {
            check_attachment(&d.texture, d.load, d.store, d.memoryless)?;
            if !d.texture.desc().format.has_stencil() {
                return Err(MetalError::Invalid(
                    "the stencil texture has no stencil aspect",
                ));
            }
            let a = rp.stencilAttachment();
            a.setTexture(Some(d.texture.raw()));
            a.setLoadAction(d.load.to_mtl());
            a.setStoreAction(d.store.to_mtl());
            a.setClearStencil(d.clear);
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
            RenderCommand::Depend(d) => d.encode(&*self.enc, Stages::VERTEX | Stages::FRAGMENT)?,
            RenderCommand::WaitForEarlierPasses => self
                .enc
                .barrierAfterQueueStages_beforeStages_visibilityOptions(
                    MTLStages::All,
                    MTLStages::Vertex | MTLStages::Fragment,
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
