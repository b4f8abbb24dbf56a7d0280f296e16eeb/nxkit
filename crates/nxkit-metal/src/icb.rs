//! Indirect command buffers (ICB): render commands encoded on the CPU and executed inside a render pass.
//!
//! # What an ICB can hold on this SDK
//!
//! The facts come from the `objc2-metal` headers at the pinned revision, generated from the Xcode 27.1 SDK.
//! The files are `MTLIndirectCommandBuffer.rs`, `MTLIndirectCommandEncoder.rs` and `MTL4RenderCommandEncoder.rs`.
//!
//! Per command, an ICB can set (`MTLIndirectRenderCommand`):
//! - the render pipeline state,
//! - vertex and fragment buffers, each as an `MTLBuffer` plus an offset (not as a GPU address),
//! - `drawPrimitives`, `drawIndexedPrimitives`, `drawPatches` and `drawIndexedPatches`,
//! - the depth and stencil state, the depth bias, the depth clip mode, the cull mode, the winding and the fill mode,
//! - a barrier (`setBarrier`, `clearBarrier`).
//!
//! Each of the last group and the pipeline and the buffers can be inherited from the encoder instead. The
//! descriptor has one flag for each: `inheritPipelineState`, `inheritBuffers`, `inheritDepthStencilState`,
//! `inheritDepthBias`, `inheritDepthClipMode`, `inheritCullMode`, `inheritFrontFacingWinding` and
//! `inheritTriangleFillMode`. A command cannot set a state that its ICB inherits.
//!
//! An ICB cannot hold:
//! - the viewport and the scissor rectangle,
//! - the stencil reference values and the blend colour,
//! - the depth bounds and the visibility result mode,
//! - textures and samplers set directly. They reach the shader through the argument table of the encoder when
//!   `inheritBuffers` is set, or through resource ids inside buffers,
//! - a mix of draw commands and dispatch commands (`MTLIndirectCommandType`: a dispatch cannot be mixed).
//!
//! Further rules:
//! - A pipeline used in an ICB must be made with `supportIndirectCommandBuffers` enabled
//!   ([`RenderPipelineDesc::support_icb`](crate::RenderPipelineDesc)).
//! - The descriptor fixes the bind counts (`maxVertexBufferBindCount`, `maxFragmentBufferBindCount`).
//! - The ICB, the buffers and the textures its commands use must be resident. This crate puts the ICB and the guest
//!   buffer into the residency set.
//! - A render pass runs commands with `executeCommandsInBuffer:withRange:` or, with the range in a GPU buffer,
//!   `executeCommandsInBuffer:indirectBuffer:`. This module wraps the first.
//! - Compute ICBs hold `setComputePipelineState`, `setKernelBuffer`, `concurrentDispatchThreadgroups`,
//!   `concurrentDispatchThreads` and a barrier. This crate does not wrap them yet.

use std::sync::{Arc, Mutex, PoisonError};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSRange;
use objc2_metal::{
    MTLAllocation, MTLDevice, MTLIndirectCommandBuffer, MTLIndirectCommandBufferDescriptor,
    MTLIndirectCommandType, MTLIndirectRenderCommand, MTLResourceOptions,
};

use crate::device::Context;
use crate::error::{MetalError, Result, create};
use crate::format::{IndexType, Primitive};
use crate::guest::GuestMemory;
use crate::pipeline::RenderPipeline;
use crate::residency::ResidentGuard;
use crate::state::{DepthBias, DepthClip, DepthStencilState, FillMode, Winding};
use crate::table::Keep;

/// Which state the commands of an ICB take from the encoder instead of setting it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inherit {
    /// The pipeline.
    pub pipeline: bool,
    /// The buffers (the argument table of the encoder).
    pub buffers: bool,
    /// The depth and stencil state.
    pub depth_stencil: bool,
    /// The depth bias.
    pub depth_bias: bool,
    /// The depth clip mode.
    pub depth_clip: bool,
    /// The cull mode.
    pub cull: bool,
    /// The winding.
    pub winding: bool,
    /// The fill mode.
    pub fill: bool,
}

impl Inherit {
    /// Every command sets everything itself.
    pub const NOTHING: Inherit = Inherit {
        pipeline: false,
        buffers: false,
        depth_stencil: false,
        depth_bias: false,
        depth_clip: false,
        cull: false,
        winding: false,
        fill: false,
    };
}

/// What an ICB is made for.
#[derive(Clone, Copy, Debug)]
pub struct IcbDesc {
    /// The most commands it can hold.
    pub max_commands: u32,
    /// The state its commands take from the encoder.
    pub inherit: Inherit,
    /// The vertex buffer slots a command can bind.
    pub max_vertex_buffers: u32,
    /// The fragment buffer slots a command can bind.
    pub max_fragment_buffers: u32,
}

/// A buffer binding of an ICB command: a place in guest memory.
#[derive(Clone)]
pub struct IcbBuffer {
    /// The guest memory.
    pub guest: Arc<GuestMemory>,
    /// The byte offset in the guest buffer.
    pub offset: u64,
}

/// One command of an ICB.
#[derive(Clone)]
pub enum IcbCommand {
    /// Sets the pipeline. Not allowed when the ICB inherits it.
    SetPipeline(RenderPipeline),
    /// Binds a vertex buffer. Not allowed when the ICB inherits the buffers.
    SetVertexBuffer {
        /// The slot.
        index: u32,
        /// The buffer.
        buffer: IcbBuffer,
    },
    /// Binds a fragment buffer. Not allowed when the ICB inherits the buffers.
    SetFragmentBuffer {
        /// The slot.
        index: u32,
        /// The buffer.
        buffer: IcbBuffer,
    },
    /// Sets the depth and stencil state.
    SetDepthStencil(DepthStencilState),
    /// Sets the depth bias.
    SetDepthBias(DepthBias),
    /// Sets the depth clip mode.
    SetDepthClip(DepthClip),
    /// Sets the winding of front faces.
    SetWinding(Winding),
    /// Sets the fill mode.
    SetFillMode(FillMode),
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
    /// Draws primitives with indices.
    DrawIndexed {
        /// The topology.
        primitive: Primitive,
        /// The width of an index.
        index_type: IndexType,
        /// The first index.
        indices: IcbBuffer,
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

/// An indirect command buffer of render commands.
#[derive(Clone)]
pub struct IndirectCommandBuffer {
    pub(crate) raw: Retained<ProtocolObject<dyn MTLIndirectCommandBuffer>>,
    size: u32,
    inherit: Inherit,
    max_vertex: u32,
    max_fragment: u32,
    keep: Arc<Mutex<Vec<Keep>>>,
    _resident: Arc<ResidentGuard>,
}

impl Context {
    /// Makes an ICB for render commands. It joins the residency set.
    pub fn new_indirect_command_buffer(&self, desc: &IcbDesc) -> Result<IndirectCommandBuffer> {
        if desc.max_commands == 0 {
            return Err(MetalError::Invalid("an ICB needs at least one command"));
        }
        let d = MTLIndirectCommandBufferDescriptor::new();
        d.setCommandTypes(MTLIndirectCommandType::Draw | MTLIndirectCommandType::DrawIndexed);
        d.setInheritPipelineState(desc.inherit.pipeline);
        d.setInheritBuffers(desc.inherit.buffers);
        d.setInheritDepthStencilState(desc.inherit.depth_stencil);
        d.setInheritDepthBias(desc.inherit.depth_bias);
        d.setInheritDepthClipMode(desc.inherit.depth_clip);
        d.setInheritCullMode(desc.inherit.cull);
        d.setInheritFrontFacingWinding(desc.inherit.winding);
        d.setInheritTriangleFillMode(desc.inherit.fill);
        d.setMaxVertexBufferBindCount(desc.max_vertex_buffers as usize);
        d.setMaxFragmentBufferBindCount(desc.max_fragment_buffers as usize);
        // SAFETY: `max_commands` is the size the ICB is made for and every later index is checked against it.
        let raw = unsafe {
            self.device()
                .newIndirectCommandBufferWithDescriptor_maxCommandCount_options(
                    &d,
                    desc.max_commands as usize,
                    MTLResourceOptions::StorageModePrivate,
                )
        }
        .ok_or_else(|| create("indirect command buffer", "Metal returned none"))?;
        let resident = self.make_resident(ProtocolObject::<dyn MTLAllocation>::from_ref(&*raw));
        Ok(IndirectCommandBuffer {
            raw,
            size: desc.max_commands,
            inherit: desc.inherit,
            max_vertex: desc.max_vertex_buffers,
            max_fragment: desc.max_fragment_buffers,
            keep: Arc::new(Mutex::new(Vec::new())),
            _resident: Arc::new(resident),
        })
    }
}

impl IndirectCommandBuffer {
    /// The number of commands it can hold.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Empties the ICB and drops what it kept alive.
    pub fn reset(&self) {
        // SAFETY: the range is the whole buffer.
        unsafe {
            self.raw.resetWithRange(NSRange::new(0, self.size as usize));
        }
        self.keep().clear();
    }

    /// Encodes the commands of one slot. The slot is overwritten.
    ///
    /// A command that sets state the ICB inherits is refused. The GPU must not execute the ICB while this runs.
    pub fn encode(&self, slot: u32, commands: &[IcbCommand]) -> Result<()> {
        if slot >= self.size {
            return Err(MetalError::Invalid("the ICB slot is out of range"));
        }
        // SAFETY: `slot` is below the size (checked above).
        let cmd = unsafe { self.raw.indirectRenderCommandAtIndex(slot as usize) };
        for c in commands {
            self.encode_one(&cmd, c)?;
        }
        Ok(())
    }

    fn encode_one(
        &self,
        cmd: &ProtocolObject<dyn MTLIndirectRenderCommand>,
        c: &IcbCommand,
    ) -> Result<()> {
        let inh = &self.inherit;
        let refuse = |on: bool, what: &'static str| {
            if on {
                Err(MetalError::Invalid(what))
            } else {
                Ok(())
            }
        };
        match c {
            IcbCommand::SetPipeline(p) => {
                refuse(inh.pipeline, "the ICB inherits the pipeline")?;
                cmd.setRenderPipelineState(&p.raw);
                self.keep().push(Keep::new(p.clone()));
            }
            IcbCommand::SetVertexBuffer { index, buffer } => {
                refuse(inh.buffers, "the ICB inherits the buffers")?;
                if *index >= self.max_vertex {
                    return Err(MetalError::Invalid(
                        "the vertex buffer slot is out of range",
                    ));
                }
                let off = self.guest_offset(buffer)?;
                // SAFETY: the slot is below the bind count of the descriptor and the offset is inside the guest
                // buffer, which the ICB keeps alive and resident.
                unsafe {
                    cmd.setVertexBuffer_offset_atIndex(buffer.guest.buffer(), off, *index as usize)
                };
                self.keep().push(Keep::new(buffer.guest.clone()));
            }
            IcbCommand::SetFragmentBuffer { index, buffer } => {
                refuse(inh.buffers, "the ICB inherits the buffers")?;
                if *index >= self.max_fragment {
                    return Err(MetalError::Invalid(
                        "the fragment buffer slot is out of range",
                    ));
                }
                let off = self.guest_offset(buffer)?;
                // SAFETY: as for the vertex buffer.
                unsafe {
                    cmd.setFragmentBuffer_offset_atIndex(
                        buffer.guest.buffer(),
                        off,
                        *index as usize,
                    )
                };
                self.keep().push(Keep::new(buffer.guest.clone()));
            }
            IcbCommand::SetDepthStencil(d) => {
                refuse(
                    inh.depth_stencil,
                    "the ICB inherits the depth and stencil state",
                )?;
                cmd.setDepthStencilState(Some(&d.raw));
                self.keep().push(Keep::new(d.clone()));
            }
            IcbCommand::SetDepthBias(b) => {
                refuse(inh.depth_bias, "the ICB inherits the depth bias")?;
                cmd.setDepthBias_slopeScale_clamp(b.constant, b.slope_scale, b.clamp);
            }
            IcbCommand::SetDepthClip(m) => {
                refuse(inh.depth_clip, "the ICB inherits the depth clip mode")?;
                cmd.setDepthClipMode(m.to_mtl());
            }
            IcbCommand::SetWinding(w) => {
                refuse(inh.winding, "the ICB inherits the winding")?;
                cmd.setFrontFacingWinding(w.to_mtl());
            }
            IcbCommand::SetFillMode(f) => {
                refuse(inh.fill, "the ICB inherits the fill mode")?;
                cmd.setTriangleFillMode(f.to_mtl());
            }
            IcbCommand::Draw {
                primitive,
                first_vertex,
                vertex_count,
                instance_count,
                base_instance,
            } => {
                // SAFETY: the counts are plain data. The pipeline and buffers are set by earlier commands or
                // inherited, which Metal validates when it runs the command.
                unsafe {
                    cmd.drawPrimitives_vertexStart_vertexCount_instanceCount_baseInstance(
                        primitive.to_mtl(),
                        *first_vertex as usize,
                        *vertex_count as usize,
                        *instance_count as usize,
                        *base_instance as usize,
                    );
                }
            }
            IcbCommand::DrawIndexed {
                primitive,
                index_type,
                indices,
                index_count,
                instance_count,
                base_vertex,
                base_instance,
            } => {
                let off = self.guest_offset(indices)?;
                let bytes = u64::from(*index_count) * index_type.bytes();
                if off as u64 + bytes > indices.guest.len() {
                    return Err(MetalError::Invalid(
                        "the index range is outside the guest buffer",
                    ));
                }
                // SAFETY: the index range is inside the guest buffer (checked above), which the ICB keeps alive.
                unsafe {
                    cmd.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset_instanceCount_baseVertex_baseInstance(
                        primitive.to_mtl(),
                        *index_count as usize,
                        index_type.to_mtl(),
                        indices.guest.buffer(),
                        off,
                        *instance_count as usize,
                        *base_vertex as isize,
                        *base_instance as usize,
                    );
                }
                self.keep().push(Keep::new(indices.guest.clone()));
            }
        }
        Ok(())
    }

    fn guest_offset(&self, b: &IcbBuffer) -> Result<usize> {
        if b.offset >= b.guest.len() {
            return Err(MetalError::Invalid(
                "the buffer offset is outside the guest buffer",
            ));
        }
        Ok(b.offset as usize)
    }

    fn keep(&self) -> std::sync::MutexGuard<'_, Vec<Keep>> {
        self.keep.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
