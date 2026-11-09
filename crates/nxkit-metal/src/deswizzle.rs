//! Block-linear kernels between guest memory and textures.
//!
//! [`Deswizzler`] reads blocks at their block-linear address and writes them to a texture. [`Swizzler`] is the
//! inverse and writes the pixels of a texture into guest memory, for a game that reads a render target. Both work
//! on a box of one level and one slice, so a sub-rectangle copy-back is the same call with a smaller region.
//!
//! One thread handles one block of 1, 2, 4, 8 or 16 bytes. The kernels see the texture as an unsigned integer image
//! of that block size, which `encode_region` makes with a view when the texture has another format. The layout
//! (levels, layers, block heights) is in [`crate::blocklinear`]. The MSL is compiled at run time.

use std::sync::Arc;

use crate::blocklinear::{KernelParams, LevelLayout};
use crate::compute::{ComputeCommand, ComputePass};
use crate::databuf::DataBuffer;
use crate::device::Context;
use crate::error::{MetalError, Result};
use crate::format::Format;
use crate::guest::GuestMemory;
use crate::pipeline::{ComputePipeline, PipelineCache, ShaderLibrary};
use crate::scratch::Scratch;
use crate::table::ArgumentTable;
use crate::texture::{Texture, TextureKind, TextureRegion, ViewDesc};

use crate::blocklinear::{GobOrder, SurfaceLayout};

const KEY: u64 = 0x6e78_6b69_745f_6473; // "nxkit_ds"

/// The MSL source of the kernels: `deswizzle2d`, `deswizzle3d`, `swizzle2d` and `swizzle3d`.
pub const DESWIZZLE_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Params {
    ulong base;
    uint rows;
    uint row_bytes;
    uint bytes_per_block;
    uint block_height;
    uint block_depth;
    uint order;
    uint ox, oy, oz;
    uint ew, eh, ed;
};

static inline uint gob_offset(uint x, uint y, uint order) {
    if (order == 0) {
        return (x % 64 / 32) * 256 + (y % 8 / 2) * 64 + (x % 32 / 16) * 32 + (y % 2) * 16 + x % 16;
    }
    return (x % 64 / 32) * 256 + (y % 8 / 4) * 128 + (x % 32 / 16) * 64 + (y % 4 / 2) * 32 + (y % 2) * 16 + x % 16;
}

static inline device uchar* block_ptr(constant Params& p, uint3 b) {
    uint xb = b.x * p.bytes_per_block;
    uint bh = p.block_height;
    uint bd = p.block_depth;
    ulong gobs_x = (p.row_bytes + 63) / 64;
    ulong blocks_y = (p.rows + 8 * bh - 1) / (8 * bh);
    ulong block = 512ul * bh * bd;
    ulong at = (ulong)(b.z / bd) * gobs_x * blocks_y * block
             + (ulong)(b.y / (8 * bh)) * gobs_x * block
             + (ulong)(xb / 64) * block
             + (ulong)((b.z % bd) * bh + (b.y % (8 * bh)) / 8) * 512
             + gob_offset(xb, b.y, p.order);
    return (device uchar*)(p.base + at);
}

static inline uint4 load_block(device const uchar* ptr, uint bytes) {
    uint4 v = uint4(0);
    switch (bytes) {
        case 1: v.x = *ptr; break;
        case 2: v.x = *(device const ushort*)ptr; break;
        case 4: v.x = *(device const uint*)ptr; break;
        case 8: v.xy = *(device const uint2*)ptr; break;
        default: v = *(device const uint4*)ptr; break;
    }
    return v;
}

static inline void store_block(device uchar* ptr, uint bytes, uint4 v) {
    switch (bytes) {
        case 1: *ptr = (uchar)v.x; break;
        case 2: *(device ushort*)ptr = (ushort)v.x; break;
        case 4: *(device uint*)ptr = v.x; break;
        case 8: *(device uint2*)ptr = v.xy; break;
        default: *(device uint4*)ptr = v; break;
    }
}

kernel void deswizzle2d(constant Params& p [[buffer(0)]],
                        texture2d<uint, access::write> dst [[texture(0)]],
                        uint3 gid [[thread_position_in_grid]]) {
    if (gid.x >= p.ew || gid.y >= p.eh || gid.z >= p.ed) { return; }
    uint3 b = uint3(gid.x + p.ox, gid.y + p.oy, gid.z + p.oz);
    dst.write(load_block(block_ptr(p, b), p.bytes_per_block), uint2(b.x, b.y));
}

kernel void deswizzle3d(constant Params& p [[buffer(0)]],
                        texture3d<uint, access::write> dst [[texture(0)]],
                        uint3 gid [[thread_position_in_grid]]) {
    if (gid.x >= p.ew || gid.y >= p.eh || gid.z >= p.ed) { return; }
    uint3 b = uint3(gid.x + p.ox, gid.y + p.oy, gid.z + p.oz);
    dst.write(load_block(block_ptr(p, b), p.bytes_per_block), b);
}

kernel void deswizzle_buffer(constant Params& p [[buffer(0)]], device uchar* dst [[buffer(1)]],
                            uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= p.ew || gid.y >= p.eh) { return; }
    uint3 b = uint3(gid.x, gid.y, 0);
    store_block(dst + (ulong(gid.y) * p.ew + gid.x) * p.bytes_per_block,
                p.bytes_per_block, load_block(block_ptr(p, b), p.bytes_per_block));
}

kernel void swizzle2d(constant Params& p [[buffer(0)]],
                      texture2d<uint, access::read> src [[texture(0)]],
                      uint3 gid [[thread_position_in_grid]]) {
    if (gid.x >= p.ew || gid.y >= p.eh || gid.z >= p.ed) { return; }
    uint3 b = uint3(gid.x + p.ox, gid.y + p.oy, gid.z + p.oz);
    store_block(block_ptr(p, b), p.bytes_per_block, src.read(uint2(b.x, b.y)));
}

kernel void swizzle3d(constant Params& p [[buffer(0)]],
                      texture3d<uint, access::read> src [[texture(0)]],
                      uint3 gid [[thread_position_in_grid]]) {
    if (gid.x >= p.ew || gid.y >= p.eh || gid.z >= p.ed) { return; }
    uint3 b = uint3(gid.x + p.ox, gid.y + p.oy, gid.z + p.oz);
    store_block(block_ptr(p, b), p.bytes_per_block, src.read(b));
}
"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    ToTexture,
    FromTexture,
}

struct Kernels {
    d2: ComputePipeline,
    d3: ComputePipeline,
    table: ArgumentTable,
    direction: Direction,
    buffer: Option<ComputePipeline>,
}

impl Kernels {
    fn new(ctx: &Context, cache: &PipelineCache, direction: Direction) -> Result<Self> {
        let lib = ShaderLibrary::from_source(ctx, "nxkit-block-linear", DESWIZZLE_MSL)?;
        let (n2, n3, k) = match direction {
            Direction::ToTexture => ("deswizzle2d", "deswizzle3d", KEY),
            Direction::FromTexture => ("swizzle2d", "swizzle3d", KEY + 2),
        };
        Ok(Self {
            d2: cache.compute(k, &lib, n2)?,
            d3: cache.compute(k + 1, &lib, n3)?,
            table: ArgumentTable::new(ctx, 2, 1)?,
            direction,
            buffer: if direction == Direction::ToTexture {
                Some(cache.compute(KEY + 4, &lib, "deswizzle_buffer")?)
            } else {
                None
            },
        })
    }

    fn dispatch(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        job: &KernelParams,
        tex: &Texture,
        three_d: bool,
    ) -> Result<()> {
        let l = &job.level;
        let storage = u64::from(l.row_bytes.div_ceil(64))
            * u64::from(l.height.div_ceil(8 * l.block_height))
            * u64::from(l.depth.div_ceil(l.block_depth))
            * 512
            * u64::from(l.block_height * l.block_depth);
        let addr = guest.gpu_address_of_offset(job.base, storage)?;
        let p = scratch.push(&KernelParams { base: addr, ..*job }.to_bytes())?;
        let pipe = if three_d { &self.d3 } else { &self.d2 };
        pass.encode(&ComputeCommand::SetPipeline(pipe.clone()))?;
        pass.encode(&ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: p,
        })?;
        pass.encode(&ComputeCommand::SetTexture {
            index: 0,
            texture: tex.clone(),
        })?;
        let width = pipe.thread_execution_width().max(1) as u32;
        let height = (pipe.max_threads_per_threadgroup() as u32 / width).clamp(1, 8);
        pass.encode(&ComputeCommand::DispatchThreads {
            grid: job.extent,
            threads: [width, height, 1],
        })?;
        pass.retain(guest.clone());
        pass.retain(tex.clone());
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn region(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        guest_offset: u64,
        surface: &SurfaceLayout,
        tex: &Texture,
        r: &TextureRegion,
    ) -> Result<()> {
        let d = tex.desc();
        let level = surface.level(r.level)?;
        let want = Format::uint_for_block_bytes(surface.bytes_per_block);
        if d.format.bytes_per_pixel() != surface.bytes_per_block || want.is_none() {
            return Err(MetalError::Invalid(
                "the texture format does not match bytes per block",
            ));
        }
        let [w, h, dep] = d.level_extent(r.level);
        if (w, h, dep) != (level.width, level.height, level.depth)
            || d.mip_levels != surface.mip_levels
            || d.slice_count() != surface.layers
        {
            return Err(MetalError::Invalid(
                "the texture does not match the surface layout",
            ));
        }
        r.check(&d)?;
        let write = self.direction == Direction::ToTexture;
        if (write && !d.shader_write) || (!write && !d.shader_read) {
            return Err(MetalError::Invalid(
                "the texture is not usable by the kernel",
            ));
        }
        let three_d = d.kind == TextureKind::D3;
        let stride = surface.layer_stride()?;
        let base = guest_offset + u64::from(r.slice) * stride + level.offset;
        let uint = want.unwrap_or(Format::R32Uint);
        let plain = d.format == uint && d.kind == TextureKind::D2 && d.mip_levels == 1;
        let target = if plain {
            tex.clone()
        } else {
            tex.view(&ViewDesc {
                format: uint,
                kind: if three_d {
                    TextureKind::D3
                } else {
                    TextureKind::D2
                },
                first_level: r.level,
                level_count: 1,
                first_slice: r.slice,
                slice_count: 1,
            })?
        };
        let job = KernelParams {
            base,
            level,
            order: surface.order,
            bytes_per_block: surface.bytes_per_block,
            origin: r.origin,
            extent: r.size,
        };
        self.dispatch(pass, scratch, guest, &job, &target, three_d)
    }
}

/// The geometry of one block-linear level of a 2D texture, as one flat surface.
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

    fn surface(&self) -> Result<SurfaceLayout> {
        let s = SurfaceLayout::single(
            self.bytes_per_block,
            self.width,
            self.height,
            self.block_height,
            self.order,
        );
        s.validate()?;
        if u64::from(self.row_pitch()) < u64::from(self.width) * u64::from(self.bytes_per_block) {
            return Err(MetalError::Invalid("pitch is smaller than a row"));
        }
        Ok(s)
    }

    fn level(&self) -> Result<LevelLayout> {
        let mut l = self.surface()?.level(0)?;
        l.row_bytes = self.row_pitch();
        Ok(l)
    }

    /// The bytes of the block-linear storage, with the padding of whole GOBs and blocks.
    pub fn storage_bytes(&self) -> u64 {
        let gobs_x = u64::from(self.row_pitch()).div_ceil(64);
        let blocks_y = u64::from(self.height).div_ceil(8 * u64::from(self.block_height));
        gobs_x * blocks_y * 512 * u64::from(self.block_height)
    }
}

/// Block-linear guest memory to a texture.
pub struct Deswizzler {
    k: Kernels,
}

// SAFETY: the kernels are immutable pipeline states and one argument table. The caller encodes through one
// deswizzler on one thread at a time, as the docs of `table` say.
unsafe impl Send for Deswizzler {}
// SAFETY: see `Send`. A shared reference only reads the table and the pipelines.
unsafe impl Sync for Deswizzler {}

impl Deswizzler {
    /// Compiles the kernels through the pipeline cache.
    pub fn new(ctx: &Context, cache: &PipelineCache) -> Result<Self> {
        Ok(Self {
            k: Kernels::new(ctx, cache, Direction::ToTexture)?,
        })
    }

    /// The table that a compute pass must be started with to use the `encode` calls.
    pub fn table(&self) -> &ArgumentTable {
        &self.k.table
    }

    /// Deswizzles a whole 2D level into a tightly packed range of a buffer that starts at `dst_offset`. Compressed blocks use this path before a
    /// buffer-to-texture copy, because Metal cannot copy an integer texture directly into a compressed texture.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_to_buffer(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        guest_offset: u64,
        (dst, dst_offset): (&Arc<DataBuffer>, u64),
        params: &DeswizzleParams,
    ) -> Result<()> {
        let level = params.level()?;
        let size =
            u64::from(params.width) * u64::from(params.height) * u64::from(params.bytes_per_block);
        if dst_offset
            .checked_add(size)
            .is_none_or(|end| end > dst.len())
        {
            return Err(MetalError::Invalid("the deswizzle buffer is too small"));
        }
        let base = guest.gpu_address_of_offset(guest_offset, params.storage_bytes())?;
        let job = KernelParams {
            base,
            level,
            order: params.order,
            bytes_per_block: params.bytes_per_block,
            origin: [0; 3],
            extent: [params.width, params.height, 1],
        };
        let args = scratch.push(&job.to_bytes())?;
        let pipeline = self
            .k
            .buffer
            .as_ref()
            .ok_or(MetalError::Invalid("no buffer deswizzle kernel"))?;
        pass.encode(&ComputeCommand::SetPipeline(pipeline.clone()))?;
        pass.encode(&ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: args,
        })?;
        pass.encode(&ComputeCommand::SetBuffer {
            index: 1,
            gpu_addr: dst.gpu_address() + dst_offset,
        })?;
        pass.encode(&ComputeCommand::DispatchThreads {
            grid: job.extent,
            threads: [32, 4, 1],
        })?;
        pass.retain(guest.clone());
        pass.retain(dst.clone());
        Ok(())
    }

    /// Encodes the deswizzle of a box of one level and slice into `dst`.
    ///
    /// `guest_offset` is the offset of the surface (level 0 of layer 0) from the start of the guest memory.
    /// `scratch` holds the kernel parameters. The texture has the size, levels and slices of `surface`.
    /// The texture needs `shader_write`, and `allow_views` unless it is a one-level 2D texture of the unsigned
    /// integer format of the block size.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_region(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        guest_offset: u64,
        surface: &SurfaceLayout,
        dst: &Texture,
        region: &TextureRegion,
    ) -> Result<()> {
        self.k
            .region(pass, scratch, guest, guest_offset, surface, dst, region)
    }

    /// Encodes the deswizzle of one whole 2D level into `dst` (the texture may be larger).
    pub fn encode(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        guest_offset: u64,
        dst: &Texture,
        params: &DeswizzleParams,
    ) -> Result<()> {
        let level = params.level()?;
        let d = dst.desc();
        if Format::uint_for_block_bytes(params.bytes_per_block) != Some(d.format)
            || d.kind != TextureKind::D2
            || d.mip_levels != 1
        {
            return Err(MetalError::Invalid(
                "the texture format does not match bytes per block",
            ));
        }
        if !d.shader_write || d.width < params.width || d.height < params.height {
            return Err(MetalError::Invalid(
                "the texture is too small or not writable",
            ));
        }
        let job = KernelParams {
            base: guest_offset,
            level,
            order: params.order,
            bytes_per_block: params.bytes_per_block,
            origin: [0; 3],
            extent: [params.width, params.height, 1],
        };
        self.k.dispatch(pass, scratch, guest, &job, dst, false)
    }
}

/// A texture to block-linear guest memory, the copy-back of a render target.
pub struct Swizzler {
    k: Kernels,
}

// SAFETY: as for `Deswizzler`.
unsafe impl Send for Swizzler {}
// SAFETY: as for `Deswizzler`.
unsafe impl Sync for Swizzler {}

impl Swizzler {
    /// Compiles the kernels through the pipeline cache.
    pub fn new(ctx: &Context, cache: &PipelineCache) -> Result<Self> {
        Ok(Self {
            k: Kernels::new(ctx, cache, Direction::FromTexture)?,
        })
    }

    /// The table that a compute pass must be started with to use [`Swizzler::encode_region`].
    pub fn table(&self) -> &ArgumentTable {
        &self.k.table
    }

    /// Encodes the copy of a box of one level and slice of `src` to the block-linear surface in guest memory.
    ///
    /// The bytes outside the box stay as they are. `src` must have finished rendering in an earlier pass
    /// (`ComputeCommand::WaitForEarlierPasses`) and needs `shader_read`, and `allow_views` unless it is a one-level
    /// 2D texture of the unsigned integer format of the block size.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_region(
        &self,
        pass: &mut ComputePass<'_>,
        scratch: &Scratch,
        guest: &Arc<GuestMemory>,
        guest_offset: u64,
        surface: &SurfaceLayout,
        src: &Texture,
        region: &TextureRegion,
    ) -> Result<()> {
        self.k
            .region(pass, scratch, guest, guest_offset, surface, src, region)
    }
}
