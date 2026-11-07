//! Block-linear deswizzle as a compute kernel: from guest memory into a texture.
//!
//! One thread reads one block (1, 2, 4, 8 or 16 bytes) at its block-linear address and writes it to an unsigned
//! integer texture. A caller that needs another format makes a view of the same size. The MSL is compiled at run
//! time. Only 2D, one layer and one level at a time. The layout is the one that `nxkit-nvn` documents for the
//! Tegra X1 and the Switch 2.

use crate::compute::{ComputeCommand, ComputePass};
use crate::device::Context;
use crate::error::{MetalError, Result};
use crate::format::Format;
use crate::guest::GuestMemory;
use crate::pipeline::{ComputePipeline, PipelineCache, ShaderLibrary};
use crate::scratch::Scratch;
use crate::table::ArgumentTable;

const KEY: u64 = 0x6e78_6b69_745f_6473; // "nxkit_ds"

/// The MSL source of the kernel.
pub const DESWIZZLE_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Params {
    ulong src;
    uint width;
    uint height;
    uint bytes_per_block;
    uint block_height;
    uint pitch;
    uint order;
    uint pad;
};

static inline uint gob_offset(uint x, uint y, uint order) {
    if (order == 0) {
        return (x % 64 / 32) * 256 + (y % 8 / 2) * 64 + (x % 32 / 16) * 32 + (y % 2) * 16 + x % 16;
    }
    return (x % 64 / 32) * 256 + (y % 8 / 4) * 128 + (x % 32 / 16) * 64 + (y % 4 / 2) * 32 + (y % 2) * 16 + x % 16;
}

kernel void deswizzle(constant Params& p [[buffer(0)]],
                      texture2d<uint, access::write> dst [[texture(0)]],
                      uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= p.width || gid.y >= p.height) {
        return;
    }
    uint xb = gid.x * p.bytes_per_block;
    uint bh = p.block_height;
    ulong gobs_x = (p.pitch + 63) / 64;
    ulong block = 512ul * bh;
    ulong at = (ulong)(gid.y / (8 * bh)) * gobs_x * block
             + (ulong)(xb / 64) * block
             + (ulong)((gid.y % (8 * bh)) / 8) * 512
             + gob_offset(xb, gid.y, p.order);
    device const uchar* ptr = (device const uchar*)(p.src + at);
    uint4 v = uint4(0);
    switch (p.bytes_per_block) {
        case 1: v.x = *ptr; break;
        case 2: v.x = *(device const ushort*)ptr; break;
        case 4: v.x = *(device const uint*)ptr; break;
        case 8: v.xy = *(device const uint2*)ptr; break;
        default: v = *(device const uint4*)ptr; break;
    }
    dst.write(v, gid);
}
"#;

/// The order of the 16 byte pieces inside a GOB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GobOrder {
    /// The Tegra X1 order of the Switch.
    #[default]
    Tegra,
    /// The order of the Switch 2.
    Switch2,
}

/// The geometry of one block-linear surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeswizzleParams {
    /// The bytes of one block: 1, 2, 4, 8 or 16.
    pub bytes_per_block: u32,
    /// The width in blocks.
    pub width: u32,
    /// The height in blocks.
    pub height: u32,
    /// The block height in GOBs: 1, 2, 4, 8, 16 or 32. It is not the log2.
    pub block_height: u32,
    /// The bytes of one row as laid out. Zero means `width * bytes_per_block`.
    pub pitch: u32,
    /// The GOB order.
    pub order: GobOrder,
}

impl DeswizzleParams {
    fn row_pitch(&self) -> u32 {
        if self.pitch == 0 {
            self.width * self.bytes_per_block
        } else {
            self.pitch
        }
    }

    fn validate(&self) -> Result<()> {
        if Format::uint_for_block_bytes(self.bytes_per_block).is_none() {
            return Err(MetalError::Invalid(
                "bytes per block must be 1, 2, 4, 8 or 16",
            ));
        }
        if self.width == 0 || self.height == 0 {
            return Err(MetalError::Invalid("the surface is empty"));
        }
        if !(1..=32).contains(&self.block_height) || !self.block_height.is_power_of_two() {
            return Err(MetalError::Invalid(
                "block height must be 1, 2, 4, 8, 16 or 32 GOBs",
            ));
        }
        if u64::from(self.row_pitch()) < u64::from(self.width) * u64::from(self.bytes_per_block) {
            return Err(MetalError::Invalid("pitch is smaller than a row"));
        }
        Ok(())
    }

    /// The bytes of the block-linear storage, with the padding of whole GOBs and blocks.
    pub fn storage_bytes(&self) -> u64 {
        let gobs_x = u64::from(self.row_pitch()).div_ceil(64);
        let blocks_y = u64::from(self.height).div_ceil(8 * u64::from(self.block_height));
        gobs_x * blocks_y * 512 * u64::from(self.block_height)
    }

    fn to_bytes(self, src: u64) -> [u8; 40] {
        let mut b = [0u8; 40];
        b[..8].copy_from_slice(&src.to_le_bytes());
        let words = [
            self.width,
            self.height,
            self.bytes_per_block,
            self.block_height,
            self.row_pitch(),
            u32::from(self.order == GobOrder::Switch2),
            0,
        ];
        for (i, w) in words.iter().enumerate() {
            b[8 + i * 4..12 + i * 4].copy_from_slice(&w.to_le_bytes());
        }
        b
    }
}

/// The kernel and the argument table it needs.
pub struct Deswizzler {
    pipeline: ComputePipeline,
    table: ArgumentTable,
}

impl Deswizzler {
    /// Compiles the kernel through the pipeline cache.
    pub fn new(ctx: &Context, cache: &PipelineCache) -> Result<Self> {
        let lib = ShaderLibrary::from_source(ctx, "nxkit-deswizzle", DESWIZZLE_MSL)?;
        Ok(Self {
            pipeline: cache.compute(KEY, &lib, "deswizzle")?,
            table: ArgumentTable::new(ctx, 1, 1)?,
        })
    }

    /// The table that a compute pass must be started with to use [`Deswizzler::encode`].
    pub fn table(&self) -> &ArgumentTable {
        &self.table
    }

    /// Encodes the deswizzle of one surface of guest memory into `dst`.
    ///
    /// `guest_offset` is the offset of the surface from the start of the guest memory. `scratch` holds the kernel
    /// parameters. The pass must have been made with [`Deswizzler::table`].
    pub fn encode(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &std::sync::Arc<GuestMemory>,
        guest_offset: u64,
        dst: &crate::texture::Texture,
        params: &DeswizzleParams,
    ) -> Result<()> {
        params.validate()?;
        let d = dst.desc();
        if Format::uint_for_block_bytes(params.bytes_per_block) != Some(d.format) {
            return Err(MetalError::Invalid(
                "the texture format does not match bytes per block",
            ));
        }
        if !d.shader_write || d.width < params.width || d.height < params.height {
            return Err(MetalError::Invalid(
                "the texture is too small or not writable",
            ));
        }
        let src = guest.gpu_address_of_offset(guest_offset, params.storage_bytes())?;
        let p = scratch.push(&params.to_bytes(src))?;
        pass.encode(&ComputeCommand::SetPipeline(self.pipeline.clone()))?;
        pass.encode(&ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: p,
        })?;
        pass.encode(&ComputeCommand::SetTexture {
            index: 0,
            texture: dst.clone(),
        })?;
        let width = self.pipeline.thread_execution_width().max(1) as u32;
        let height = (self.pipeline.max_threads_per_threadgroup() as u32 / width).clamp(1, 8);
        pass.encode(&ComputeCommand::DispatchThreads {
            grid: [params.width, params.height, 1],
            threads: [width, height, 1],
        })?;
        pass.retain(guest.clone());
        Ok(())
    }
}
