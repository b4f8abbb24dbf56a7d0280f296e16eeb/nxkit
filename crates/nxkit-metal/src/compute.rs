//! Compute passes: the commands and the pass that encodes them.

use std::any::Any;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSRange;
use objc2_metal::{
    MTL4CommandBuffer, MTL4CommandEncoder, MTL4ComputeCommandEncoder, MTL4VisibilityOptions,
    MTLOrigin, MTLSize, MTLStages,
};

use crate::barrier::{Dependency, Stages};
use crate::compute_icb::{ComputeIndirectCommandBuffer, check_range_buffer};
use crate::databuf::DataBuffer;
use crate::encode::CommandBuffer;
use crate::error::{MetalError, Result, create};
use crate::guest::GuestMemory;
use crate::pipeline::ComputePipeline;
use crate::table::ArgumentTable;
use crate::texture::Texture;

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
    /// Runs `count` commands of a compute ICB from `first`.
    ExecuteIcb {
        /// The ICB.
        icb: ComputeIndirectCommandBuffer,
        /// The first command.
        first: u32,
        /// The number of commands.
        count: u32,
    },
    /// Runs the range of a compute ICB that the GPU reads from a buffer when the command executes.
    ExecuteIcbIndirect {
        /// The ICB.
        icb: ComputeIndirectCommandBuffer,
        /// The buffer that holds an [`ExecutionRange`](crate::ExecutionRange) at `range_offset`.
        range: Arc<DataBuffer>,
        /// The byte offset of the range in `range`.
        range_offset: u64,
    },
    /// Orders the later commands of this pass after the earlier ones.
    Barrier,
    /// A barrier with explicit stages. The stages must be `DISPATCH` or `BLIT` on the side of this pass.
    Depend(Dependency),
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

impl CommandBuffer {
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
            ComputeCommand::ExecuteIcb { icb, first, count } => {
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
            ComputeCommand::ExecuteIcbIndirect {
                icb,
                range,
                range_offset,
            } => {
                check_range_buffer(range, *range_offset)?;
                // SAFETY: the range lies inside the data buffer (checked above). The caller writes a range inside
                // the ICB, because Metal does not bounds check it.
                unsafe {
                    self.enc.executeCommandsInBuffer_indirectBuffer(
                        &icb.raw,
                        range.gpu_address() + *range_offset,
                    );
                }
                self.cb.retain(icb.clone());
                self.cb.retain(range.clone());
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
            ComputeCommand::Depend(d) => d.encode(&*self.enc, Stages::DISPATCH | Stages::BLIT)?,
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
