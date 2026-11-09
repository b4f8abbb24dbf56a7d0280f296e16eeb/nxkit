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
use crate::texture::{Texture, TextureRegion};

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
    /// Writes a precise GPU timestamp into entry `index` of the heap, after the work before it.
    Timestamp {
        /// The heap.
        heap: crate::timestamp::TimestampHeap,
        /// The entry.
        index: u32,
    },
    /// A barrier with explicit stages. The stages must be `DISPATCH` or `BLIT` on the side of this pass.
    Depend(Dependency),
    /// Copies a box between compatible textures of the same sample count. Compressed and uncompressed textures
    /// require an intermediate buffer, rather than a direct texture copy.
    CopyTexture {
        /// The source.
        src: Texture,
        /// The destination.
        dst: Texture,
        /// The source level, slice and box.
        src_region: TextureRegion,
        /// The destination level, slice and first pixel.
        dst_level: u32,
        /// The destination slice.
        dst_slice: u32,
        /// The first pixel of the destination box.
        dst_origin: [u32; 3],
    },
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
    /// Copies image data from guest memory into a box of one texture level and slice (or of a 3D texture).
    CopyGuestToTexture {
        /// The guest memory.
        guest: Arc<GuestMemory>,
        /// The source, as an offset from the start of the guest memory.
        offset: u64,
        /// The bytes between two rows of blocks.
        bytes_per_row: u32,
        /// The bytes between two depth slices of a 3D box. Unused when the depth is 1.
        bytes_per_image: u32,
        /// The destination texture.
        texture: Texture,
        /// The destination level, slice and box, measured in texels.
        region: TextureRegion,
    },
    /// Copies a box of one texture level and slice (or of a 3D texture) into guest memory. The arguments are those of
    /// [`ComputeCommand::CopyGuestToTexture`], with the texture as the source.
    CopyTextureRegionToGuest {
        /// The source texture.
        texture: Texture,
        /// The source level, slice and box, measured in texels.
        region: TextureRegion,
        /// The guest memory.
        guest: Arc<GuestMemory>,
        /// The destination, as an offset from the start of the guest memory.
        offset: u64,
        /// The bytes between two rows of blocks.
        bytes_per_row: u32,
        /// The bytes between two depth slices of a 3D box. Unused when the depth is 1.
        bytes_per_image: u32,
    },
    /// Copies tightly packed or padded image data from a buffer into one texture level and slice.
    CopyBufferToTexture {
        /// The source buffer.
        buffer: Arc<DataBuffer>,
        /// The byte offset in the source buffer.
        offset: u64,
        /// The row pitch, in bytes (one row of blocks for compressed formats).
        bytes_per_row: u32,
        /// The destination texture.
        texture: Texture,
        /// The destination level, slice and box, measured in texels.
        region: TextureRegion,
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
    pub(crate) enc: Retained<ProtocolObject<dyn MTL4ComputeCommandEncoder>>,
    pub(crate) cb: &'a mut CommandBuffer,
    table: ArgumentTable,
    has_pipeline: bool,
    ended: bool,
}

pub(crate) fn size3(v: [u32; 3]) -> MTLSize {
    MTLSize {
        width: v[0] as usize,
        height: v[1] as usize,
        depth: v[2] as usize,
    }
}

impl ComputePass<'_> {
    /// Names the encoder for Instruments and the Metal debugger.
    pub fn set_label(&self, label: &str) {
        use objc2_metal::MTL4CommandEncoder as _;
        self.enc
            .setLabel(Some(&objc2_foundation::NSString::from_str(label)));
    }

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
            ComputeCommand::Timestamp { heap, index } => {
                if *index as usize >= heap.len() {
                    return Err(MetalError::Invalid("the timestamp index is out of range"));
                }
                // SAFETY: the index lies inside the heap (checked above), and the heap is retained until the GPU is
                // done with the command buffer.
                unsafe {
                    self.enc.writeTimestampWithGranularity_intoHeap_atIndex(
                        objc2_metal::MTL4TimestampGranularity::Precise,
                        &heap.raw.0,
                        *index as usize,
                    );
                }
                self.cb.retain(heap.clone());
            }
            ComputeCommand::CopyTexture {
                src,
                dst,
                src_region,
                dst_level,
                dst_slice,
                dst_origin,
            } => {
                if src.desc().format.is_compressed() != dst.desc().format.is_compressed() {
                    return Err(MetalError::Invalid(
                        "compressed texture copies require an intermediate buffer",
                    ));
                }
                src_region.check(&src.desc())?;
                let to = TextureRegion {
                    level: *dst_level,
                    slice: *dst_slice,
                    origin: *dst_origin,
                    size: src_region.size,
                };
                // The size of the destination box is not known for a block copy between formats, so only the
                // level, slice and origin are checked here. Metal validates the box.
                if *dst_level >= dst.desc().mip_levels || *dst_slice >= dst.desc().slice_count() {
                    return Err(MetalError::Invalid(
                        "the destination level or slice does not exist",
                    ));
                }
                let o = |v: [u32; 3]| MTLOrigin {
                    x: v[0] as usize,
                    y: v[1] as usize,
                    z: v[2] as usize,
                };
                // SAFETY: the source box is inside the source (checked above) and both textures are retained until
                // the GPU is done. Metal checks the destination box and the format pair.
                unsafe {
                    self.enc.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                        src.raw(),
                        src_region.slice as usize,
                        src_region.level as usize,
                        o(src_region.origin),
                        size3(src_region.size),
                        dst.raw(),
                        to.slice as usize,
                        to.level as usize,
                        o(to.origin),
                    );
                }
                self.cb.retain(src.clone());
                self.cb.retain(dst.clone());
            }
            ComputeCommand::CopyTextureToGuest {
                texture,
                guest,
                offset,
                bytes_per_row,
            } => self.copy_to_guest(texture, guest, *offset, *bytes_per_row)?,
            ComputeCommand::CopyGuestToTexture {
                guest,
                offset,
                bytes_per_row,
                bytes_per_image,
                texture,
                region,
            } => self.guest_texture_box(
                false,
                texture,
                region,
                guest,
                (*offset, *bytes_per_row, *bytes_per_image),
            )?,
            ComputeCommand::CopyTextureRegionToGuest {
                texture,
                region,
                guest,
                offset,
                bytes_per_row,
                bytes_per_image,
            } => self.guest_texture_box(
                true,
                texture,
                region,
                guest,
                (*offset, *bytes_per_row, *bytes_per_image),
            )?,
            ComputeCommand::CopyBufferToTexture {
                buffer,
                offset,
                bytes_per_row,
                texture,
                region,
            } => {
                region.check(&texture.desc())?;
                let format = texture.desc().format;
                let (bw, bh) = format.block_dim();
                let row = u64::from(region.size[0].div_ceil(bw)) * u64::from(format.block_bytes());
                let bpr = u64::from(*bytes_per_row);
                let image_bytes = bpr * u64::from(region.size[1].div_ceil(bh));
                let bytes = image_bytes * u64::from(region.size[2]);
                if bpr < row
                    || bpr % u64::from(format.block_bytes()) != 0
                    || offset % u64::from(format.block_bytes()) != 0
                    || offset
                        .checked_add(bytes)
                        .is_none_or(|end| end > buffer.len())
                {
                    return Err(MetalError::Invalid(
                        "the buffer image range or row pitch is invalid",
                    ));
                }
                // SAFETY: both image ranges are checked above. The buffer and texture are retained below.
                unsafe {
                    self.enc.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                        &buffer.raw, *offset as usize, *bytes_per_row as usize, image_bytes as usize,
                        size3(region.size), texture.raw(), region.slice as usize, region.level as usize,
                        MTLOrigin { x: region.origin[0] as usize, y: region.origin[1] as usize, z: region.origin[2] as usize });
                }
                self.cb.retain(buffer.clone());
                self.cb.retain(texture.clone());
            }
        }
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
