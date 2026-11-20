//! `InstEmitTexture.cs`: TLD4, TMML, TXD and TXQ.

use super::hlp::REGISTER_ZERO_INDEX;
use super::tex::*;
use crate::accessor::SetBinding;
use crate::emit::emitter::EmitterContext;
use crate::ir::inst::Inst;
use crate::ir::ops::{Opd, regtype, texflags};
use crate::maxwell::enums::{TexComp, TexDim, TexOffset, TexQuery};
use crate::translation::props::{TextureFlags, dimensions};
use crate::types::SamplerType;
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_tld4(
    ctx: &mut EmitterContext<'_, '_>,
    dims: TexDim,
    component: TexComp,
    imm: i32,
    component_mask: i32,
    src_a: i32,
    src_b: i32,
    dest: i32,
    offset: TexOffset,
    has_depth_compare: bool,
    is_bindless: bool,
) -> Result<(), String> {
    if dest == REGISTER_ZERO_INDEX {
        return Ok(());
    }
    let mut r = Regs { a: src_a, b: src_b };
    let is_array = dim_is_array(dims);
    let array_index = if is_array { Some(r.ra(ctx)) } else { None };
    let mut sources: Vec<Opd> = Vec::new();
    let mut ty = convert_sampler_type_dim(dims)?;
    let mut flags = texflags::GATHER;
    if is_bindless {
        sources.push(r.rb(ctx));
        flags |= texflags::BINDLESS;
    }
    let coords_count = dimensions(ty)?;
    for _ in 0..coords_count {
        sources.push(r.ra(ctx));
    }
    let is_1d_to_2d = is_1d(ty);
    if is_1d_to_2d {
        sources.push(ctx.ir.const_f(0.0));
        ty = SamplerType::TEXTURE_2D.with(ty.0 & SamplerType::ARRAY);
    }
    if let Some(a) = array_index {
        sources.push(a);
    }
    let has_any_offset = offset == TexOffset::Aoffi || offset == TexOffset::Ptp;
    let packed0 = if has_any_offset {
        Some(r.rb(ctx))
    } else {
        None
    };
    let packed1 = if offset == TexOffset::Ptp {
        Some(r.rb(ctx))
    } else {
        None
    };
    if has_depth_compare {
        sources.push(r.rb(ctx));
        ty = ty.with(SamplerType::SHADOW);
    }
    if has_any_offset {
        let offset_texels_count = if offset == TexOffset::Ptp { 4 } else { 1 };
        for index in 0..coords_count * offset_texels_count {
            let packed = if (index >> 2) & 1 == 0 {
                packed0
            } else {
                packed1
            }
            .ok_or("TLD4: missing packed offset")?;
            let o = ctx.ir.const_((index & 3) * 8);
            let w = ctx.ir.const_(6);
            sources.push(ctx.bitfield_extract_s32(packed, o, w));
        }
        if is_1d_to_2d {
            for _ in 0..offset_texels_count {
                sources.push(ctx.ir.const_(0));
            }
        }
        flags |= if offset == TexOffset::Ptp {
            texflags::OFFSETS
        } else {
            texflags::OFFSET
        };
    }
    if !has_depth_compare {
        sources.push(ctx.ir.const_(component.0));
    }
    let dests = run_dests(ctx, dest, component_mask);
    emit_texture_sample(ctx, ty, flags, imm, component_mask, &dests, &sources)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_tmml(
    ctx: &mut EmitterContext<'_, '_>,
    dims: TexDim,
    imm: i32,
    component_mask: i32,
    src_a: i32,
    src_b: i32,
    dest: i32,
    is_bindless: bool,
) -> Result<(), String> {
    if dest == REGISTER_ZERO_INDEX {
        return Ok(());
    }
    let mut r = Regs { a: src_a, b: src_b };
    let mut flags = texflags::NONE;
    let mut sources: Vec<Opd> = Vec::new();
    if is_bindless {
        sources.push(r.rb(ctx));
        flags |= texflags::BINDLESS;
    }
    let mut ty = convert_sampler_type_dim(dims)?;
    let coords_count = dimensions(ty)?;
    let is_array = dim_is_array(dims);
    let array_index = if is_array { Some(r.ra(ctx)) } else { None };
    for _ in 0..coords_count {
        sources.push(r.ra(ctx));
    }
    if is_1d(ty) {
        sources.push(ctx.ir.const_f(0.0));
        ty = SamplerType::TEXTURE_2D.with(ty.0 & SamplerType::ARRAY);
    }
    if let Some(a) = array_index {
        sources.push(a);
    }
    let sb = if is_bindless {
        SetBinding { set: 0, binding: 0 }
    } else {
        ctx.rm.get_texture_or_image_binding(
            ctx.acc,
            Inst::Lod,
            ty,
            FORMAT_UNKNOWN,
            TextureFlags(flags),
            DEFAULT_CBUF_SLOT,
            imm,
            1,
            false,
        )?
    };
    let mut dest = dest;
    let mut comp_mask = component_mask;
    let mut comp_index = 0;
    while comp_mask != 0 {
        if comp_mask & 1 != 0 {
            let d = if dest >= REGISTER_ZERO_INDEX {
                None
            } else {
                let d = ctx.ir.register(dest, regtype::GPR);
                dest += 1;
                Some(d)
            };
            let Some(d) = d else {
                break;
            };
            if comp_index >= 2 {
                ctx.comment("Unsupported component z or w found");
                let z = ctx.ir.const_(0);
                ctx.copy_to(d, z);
            } else {
                let res = ctx.lod(ty, flags, sb, comp_index ^ 1, &sources);
                let k = ctx.ir.const_f(256.0);
                let res = ctx.fp_multiply(res, k);
                let fixed = ctx.fp32_convert_to_s32(res);
                ctx.copy_to(d, fixed);
            }
        }
        comp_mask >>= 1;
        comp_index += 1;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_txd(
    ctx: &mut EmitterContext<'_, '_>,
    dims: TexDim,
    imm: i32,
    component_mask: i32,
    src_a: i32,
    src_b: i32,
    dest: i32,
    has_offset: bool,
    is_bindless: bool,
) -> Result<(), String> {
    if dest == REGISTER_ZERO_INDEX {
        return Ok(());
    }
    let mut r = Regs { a: src_a, b: src_b };
    let mut flags = texflags::DERIVATIVES;
    let mut sources: Vec<Opd> = Vec::new();
    if is_bindless {
        sources.push(r.ra(ctx));
        flags |= texflags::BINDLESS;
    }
    let mut ty = convert_sampler_type_dim(dims)?;
    let coords_count = dimensions(ty)?;
    for _ in 0..coords_count {
        sources.push(r.ra(ctx));
    }
    let is_1d_to_2d = is_1d(ty);
    if is_1d_to_2d {
        sources.push(ctx.ir.const_f(0.0));
        ty = SamplerType::TEXTURE_2D.with(ty.0 & SamplerType::ARRAY);
    }
    let packed_params = r.ra(ctx);
    if dim_is_array(dims) {
        let m = ctx.ir.const_(0xffff);
        sources.push(ctx.bitwise_and(packed_params, m));
    }
    for _ in 0..2 * coords_count {
        sources.push(r.rb(ctx));
        if is_1d_to_2d {
            sources.push(ctx.ir.const_f(0.0));
        }
    }
    if has_offset {
        for index in 0..coords_count {
            let o = ctx.ir.const_(16 + index * 4);
            let w = ctx.ir.const_(4);
            sources.push(ctx.bitfield_extract_s32(packed_params, o, w));
        }
        if is_1d_to_2d {
            sources.push(ctx.ir.const_(0));
        }
        flags |= texflags::OFFSET;
    }
    let dests = run_dests(ctx, dest, component_mask);
    emit_texture_sample(ctx, ty, flags, imm, component_mask, &dests, &sources)
}

pub(super) fn emit_txq(
    ctx: &mut EmitterContext<'_, '_>,
    query: TexQuery,
    imm: i32,
    component_mask: i32,
    src_a: i32,
    dest: i32,
    is_bindless: bool,
) -> Result<(), String> {
    if dest == REGISTER_ZERO_INDEX {
        return Ok(());
    }
    let mut ra_idx = src_a;
    let mut sources: Vec<Opd> = Vec::new();
    if is_bindless {
        sources.push(next_reg(ctx, &mut ra_idx));
    }
    sources.push(next_reg(ctx, &mut ra_idx));
    let ty = if is_bindless {
        if query == TexQuery::TexHeaderTextureType {
            SamplerType::TEXTURE_2D.with(SamplerType::MULTISAMPLE)
        } else if component_mask & 4 != 0 {
            SamplerType::TEXTURE_3D
        } else {
            SamplerType::TEXTURE_2D
        }
    } else {
        ctx.acc.query_sampler_type(imm, -1)
    };
    let flags = if is_bindless {
        texflags::BINDLESS
    } else {
        texflags::NONE
    };
    let mut dest = dest;
    let mut get_dest = |ctx: &mut EmitterContext<'_, '_>| -> Option<Opd> {
        if dest >= REGISTER_ZERO_INDEX {
            None
        } else {
            let d = ctx.ir.register(dest, regtype::GPR);
            dest += 1;
            Some(d)
        }
    };
    if query == TexQuery::TexHeaderDimension {
        let sb = if is_bindless {
            SetBinding { set: 0, binding: 0 }
        } else {
            ctx.rm.get_texture_or_image_binding(
                ctx.acc,
                Inst::TextureQuerySize,
                ty,
                FORMAT_UNKNOWN,
                TextureFlags(flags),
                DEFAULT_CBUF_SLOT,
                imm,
                1,
                false,
            )?
        };
        let mut comp_mask = component_mask;
        let mut comp_index = 0;
        while comp_mask != 0 {
            if comp_mask & 1 != 0 {
                let Some(d) = get_dest(ctx) else {
                    break;
                };
                let v = ctx.texture_query_size(ty, flags, sb, comp_index, &sources);
                ctx.copy_to(d, v);
            }
            comp_mask >>= 1;
            comp_index += 1;
        }
    } else if query == TexQuery::TexHeaderTextureType {
        let sb = if is_bindless {
            SetBinding { set: 0, binding: 0 }
        } else {
            ctx.rm.get_texture_or_image_binding(
                ctx.acc,
                Inst::TextureQuerySamples,
                ty,
                FORMAT_UNKNOWN,
                TextureFlags(flags),
                DEFAULT_CBUF_SLOT,
                imm,
                1,
                false,
            )?
        };
        if component_mask & 4 != 0 {
            if component_mask & 1 != 0 {
                get_dest(ctx);
            }
            if component_mask & 2 != 0 {
                get_dest(ctx);
            }
            if let Some(d) = get_dest(ctx) {
                let v = ctx.texture_query_samples(ty, flags, sb, &sources);
                ctx.copy_to(d, v);
            }
        }
    } else {
        ctx.acc.log(&format!(
            "Invalid or unsupported query type \"{}\".",
            tex_query_name(query.0)
        ));
    }
    Ok(())
}
