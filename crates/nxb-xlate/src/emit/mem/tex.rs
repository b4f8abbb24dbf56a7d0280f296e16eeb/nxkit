//! `InstEmitTexture.cs`: shared helpers, TEX and the texture sample emission.

use super::hlp::REGISTER_ZERO_INDEX;
use crate::accessor::SetBinding;
use crate::emit::emitter::EmitterContext;
use crate::ir::inst::Inst;
use crate::ir::ops::{Opd, regtype, texflags};
use crate::maxwell::enums::{Lod, TexDim, TexsTarget, TldsTarget};
use crate::translation::props::{TextureFlags, dimensions};
use crate::types::SamplerType;

pub(super) const MASK_LUT: [[i32; 8]; 2] = [
    [
        0b0001, 0b0010, 0b0100, 0b1000, 0b0011, 0b1001, 0b1010, 0b1100,
    ],
    [
        0b0111, 0b1011, 0b1101, 0b1110, 0b1111, 0b0000, 0b0000, 0b0000,
    ],
];

/// `TextureOperation.DefaultCbufSlot`
pub(crate) const DEFAULT_CBUF_SLOT: i32 = -1;
/// `TextureFormat.Unknown`
pub(crate) const FORMAT_UNKNOWN: usize = 0;

/// The `Ra()` and `Rb()` local functions: the next source register of each of the two register runs.
pub(super) struct Regs {
    pub(super) a: i32,
    pub(super) b: i32,
}

pub(super) fn next_reg(ctx: &mut EmitterContext<'_, '_>, idx: &mut i32) -> Opd {
    if *idx > REGISTER_ZERO_INDEX {
        return ctx.ir.const_(0);
    }
    let r = ctx.ir.register(*idx, regtype::GPR);
    *idx += 1;
    ctx.copy(r)
}

impl Regs {
    pub(super) fn ra(&mut self, ctx: &mut EmitterContext<'_, '_>) -> Opd {
        next_reg(ctx, &mut self.a)
    }
    pub(super) fn rb(&mut self, ctx: &mut EmitterContext<'_, '_>) -> Opd {
        next_reg(ctx, &mut self.b)
    }
}

pub(super) fn dim_is_array(d: TexDim) -> bool {
    d == TexDim::Array1d || d == TexDim::Array2d || d == TexDim::Array3d || d == TexDim::ArrayCube
}

pub(crate) fn is_1d(ty: SamplerType) -> bool {
    ty.0 & SamplerType::MASK == SamplerType::TEXTURE_1D.0
}

pub(crate) fn convert_sampler_type_dim(d: TexDim) -> Result<SamplerType, String> {
    let arr = SamplerType::ARRAY;
    Ok(match d {
        TexDim::_1d => SamplerType::TEXTURE_1D,
        TexDim::Array1d => SamplerType::TEXTURE_1D.with(arr),
        TexDim::_2d => SamplerType::TEXTURE_2D,
        TexDim::Array2d => SamplerType::TEXTURE_2D.with(arr),
        TexDim::_3d => SamplerType::TEXTURE_3D,
        TexDim::Array3d => SamplerType::TEXTURE_3D.with(arr),
        TexDim::Cube => SamplerType::TEXTURE_CUBE,
        TexDim::ArrayCube => SamplerType::TEXTURE_CUBE.with(arr),
        _ => return Err(format!("Invalid texture dimensions \"{}\".", d.0)),
    })
}

pub(super) fn convert_sampler_type_texs(t: TexsTarget) -> SamplerType {
    let (arr, sh) = (SamplerType::ARRAY, SamplerType::SHADOW);
    match t {
        TexsTarget::Texture1DLodZero => SamplerType::TEXTURE_1D,
        TexsTarget::Texture2D | TexsTarget::Texture2DLodZero | TexsTarget::Texture2DLodLevel => {
            SamplerType::TEXTURE_2D
        }
        TexsTarget::Texture2DDepthCompare
        | TexsTarget::Texture2DLodLevelDepthCompare
        | TexsTarget::Texture2DLodZeroDepthCompare => SamplerType::TEXTURE_2D.with(sh),
        TexsTarget::Texture2DArray | TexsTarget::Texture2DArrayLodZero => {
            SamplerType::TEXTURE_2D.with(arr)
        }
        TexsTarget::Texture2DArrayLodZeroDepthCompare => SamplerType::TEXTURE_2D.with(arr | sh),
        TexsTarget::Texture3D | TexsTarget::Texture3DLodZero => SamplerType::TEXTURE_3D,
        TexsTarget::TextureCube | TexsTarget::TextureCubeLodLevel => SamplerType::TEXTURE_CUBE,
        _ => SamplerType::NONE,
    }
}

pub(super) fn convert_sampler_type_tlds(t: TldsTarget) -> SamplerType {
    match t {
        TldsTarget::Texture1DLodZero | TldsTarget::Texture1DLodLevel => SamplerType::TEXTURE_1D,
        TldsTarget::Texture2DLodZero
        | TldsTarget::Texture2DLodZeroOffset
        | TldsTarget::Texture2DLodLevel
        | TldsTarget::Texture2DLodLevelOffset => SamplerType::TEXTURE_2D,
        TldsTarget::Texture2DLodZeroMultisample => {
            SamplerType::TEXTURE_2D.with(SamplerType::MULTISAMPLE)
        }
        TldsTarget::Texture3DLodZero => SamplerType::TEXTURE_3D,
        TldsTarget::Texture2DArrayLodZero => SamplerType::TEXTURE_2D.with(SamplerType::ARRAY),
        _ => SamplerType::NONE,
    }
}

pub(super) fn convert_texture_flags_texs(t: TexsTarget) -> u32 {
    match t {
        TexsTarget::Texture1DLodZero
        | TexsTarget::Texture2DLodZero
        | TexsTarget::Texture2DLodLevel
        | TexsTarget::Texture2DLodLevelDepthCompare
        | TexsTarget::Texture2DLodZeroDepthCompare
        | TexsTarget::Texture2DArrayLodZero
        | TexsTarget::Texture2DArrayLodZeroDepthCompare
        | TexsTarget::Texture3DLodZero
        | TexsTarget::TextureCubeLodLevel => texflags::LOD_LEVEL,
        _ => texflags::NONE,
    }
}

pub(super) fn convert_texture_flags_tlds(t: TldsTarget) -> u32 {
    match t {
        TldsTarget::Texture1DLodZero
        | TldsTarget::Texture1DLodLevel
        | TldsTarget::Texture2DLodZero
        | TldsTarget::Texture2DLodLevel
        | TldsTarget::Texture2DLodZeroMultisample
        | TldsTarget::Texture3DLodZero
        | TldsTarget::Texture2DArrayLodZero => texflags::LOD_LEVEL,
        TldsTarget::Texture2DLodZeroOffset | TldsTarget::Texture2DLodLevelOffset => {
            texflags::LOD_LEVEL | texflags::OFFSET
        }
        _ => texflags::NONE,
    }
}

pub(super) fn tex_query_name(q: i32) -> String {
    match q {
        1 => "TexHeaderDimension".into(),
        2 => "TexHeaderTextureType".into(),
        5 => "TexHeaderSamplerPos".into(),
        16 => "TexSamplerFilter".into(),
        18 => "TexSamplerLod".into(),
        20 => "TexSamplerWrap".into(),
        22 => "TexSamplerBorderColor".into(),
        n => n.to_string(),
    }
}

/// The destination registers of a component mask: stops at `RZ` (the run ends there).
pub(super) fn run_dests(ctx: &mut EmitterContext<'_, '_>, rd: i32, mask: i32) -> Vec<Option<Opd>> {
    let n = (mask as u32).count_ones() as i32;
    let mut dests = Vec::new();
    for i in 0..n {
        if rd + i >= REGISTER_ZERO_INDEX {
            break;
        }
        dests.push(Some(ctx.ir.register(rd + i, regtype::GPR)));
    }
    dests
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_tex(
    ctx: &mut EmitterContext<'_, '_>,
    flags: u32,
    dimensions_: TexDim,
    lod_mode: i32,
    imm: i32,
    component_mask: i32,
    ra_index: i32,
    rb_index: i32,
    rd_index: i32,
    is_multisample: bool,
    has_depth_compare: bool,
    has_offset: bool,
) -> Result<(), String> {
    if rd_index == REGISTER_ZERO_INDEX {
        return Ok(());
    }
    let mut r = Regs {
        a: ra_index,
        b: rb_index,
    };
    let mut flags = flags;
    let mut ty = convert_sampler_type_dim(dimensions_)?;
    let is_array = ty.0 & SamplerType::ARRAY != 0;
    let is_bindless = flags & texflags::BINDLESS != 0;
    let array_index = if is_array { Some(r.ra(ctx)) } else { None };
    let mut sources: Vec<Opd> = Vec::new();
    if is_bindless {
        sources.push(r.rb(ctx));
    }
    let has_lod = lod_mode > Lod::Lz.0;
    let mut wrap_1d = false;
    if ty == SamplerType::TEXTURE_1D
        && (flags & !texflags::BINDLESS) == texflags::INT_COORDS
        && !(has_lod || has_depth_compare || has_offset || is_array || is_multisample)
    {
        // For bindless, the texture type is unknown: a 1D sampler type is assumed to be a texture buffer.
        let is_type_buffer =
            is_bindless || ctx.acc.query_sampler_type(imm, -1) == SamplerType::TEXTURE_BUFFER;
        if is_type_buffer {
            ty = SamplerType::TEXTURE_BUFFER;
        } else {
            wrap_1d = true;
        }
    }
    let coords_count = dimensions(ty)?;
    for _ in 0..coords_count {
        sources.push(r.ra(ctx));
    }
    let mut is_1d_to_2d = false;
    if is_1d(ty) {
        if wrap_1d {
            let x = *sources.last().ok_or("TEX: no coordinate")?;
            let m = ctx.ir.const_(0x3FFF);
            let lo = ctx.bitwise_and(x, m);
            if let Some(l) = sources.last_mut() {
                *l = lo;
            }
            let c14 = ctx.ir.const_(14);
            let hi = ctx.shift_right_u32(x, c14);
            sources.push(hi);
            ctx.acc.record_texel_fetch_1d(imm);
        } else {
            sources.push(ctx.ir.const_f(0.0));
        }
        ty = SamplerType::TEXTURE_2D.with(ty.0 & SamplerType::ARRAY);
        is_1d_to_2d = true;
    }
    if is_array && let Some(a) = array_index {
        sources.push(a);
    }
    let lod_value = if has_lod {
        r.rb(ctx)
    } else {
        ctx.ir.const_f(0.0)
    };
    let packed_offs = if has_offset { Some(r.rb(ctx)) } else { None };
    if has_depth_compare {
        sources.push(r.rb(ctx));
        ty = ty.with(SamplerType::SHADOW);
    }
    if (lod_mode == Lod::Lz.0 || lod_mode == Lod::Ll.0 || lod_mode == Lod::Lla.0)
        && !is_multisample
        && ty != SamplerType::TEXTURE_BUFFER
    {
        sources.push(lod_value);
        flags |= texflags::LOD_LEVEL;
    }
    if has_offset {
        let packed = packed_offs.ok_or("TEX: offset")?;
        for index in 0..coords_count {
            let o = ctx.ir.const_(index * 4);
            let w = ctx.ir.const_(4);
            sources.push(ctx.bitfield_extract_s32(packed, o, w));
        }
        if is_1d_to_2d {
            sources.push(ctx.ir.const_(0));
        }
        flags |= texflags::OFFSET;
    }
    if lod_mode == Lod::Lb.0 || lod_mode == Lod::Lba.0 {
        sources.push(lod_value);
        flags |= texflags::LOD_BIAS;
    }
    if is_multisample {
        sources.push(r.rb(ctx));
        ty = ty.with(SamplerType::MULTISAMPLE);
    }
    let dests = run_dests(ctx, rd_index, component_mask);
    let handle = if !is_bindless { imm } else { 0 };
    emit_texture_sample(ctx, ty, flags, handle, component_mask, &dests, &sources)
}

pub(super) fn emit_texture_sample(
    ctx: &mut EmitterContext<'_, '_>,
    ty: SamplerType,
    flags: u32,
    handle: i32,
    component_mask: i32,
    dests: &[Option<Opd>],
    sources: &[Opd],
) -> Result<(), String> {
    let sb = if flags & texflags::BINDLESS != 0 {
        SetBinding { set: 0, binding: 0 }
    } else {
        ctx.rm.get_texture_or_image_binding(
            ctx.acc,
            Inst::TextureSample,
            ty,
            FORMAT_UNKNOWN,
            TextureFlags(flags),
            DEFAULT_CBUF_SLOT,
            handle,
            1,
            false,
        )?
    };
    ctx.texture_sample(ty, flags, sb, component_mask, dests, sources);
    Ok(())
}
