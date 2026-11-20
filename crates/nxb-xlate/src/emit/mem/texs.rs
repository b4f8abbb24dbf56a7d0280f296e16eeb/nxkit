//! `InstEmitTexture.cs`: TEXS, TLDS and TLD4S.

use super::hlp::REGISTER_ZERO_INDEX;
use super::tex::*;
use crate::emit::emitter::EmitterContext;
use crate::ir::ops::{Opd, regtype, texflags};
use crate::maxwell::enums::{TexsTarget, TldsTarget};
use crate::maxwell::insts::*;
use crate::translation::props::dimensions;
use crate::types::SamplerType;
#[derive(Clone, Copy, PartialEq)]
pub(super) enum TexsType {
    Texs,
    Tlds,
    Tld4s,
}

fn add_texture_offset(
    ctx: &mut EmitterContext<'_, '_>,
    r: &mut Regs,
    sources: &mut Vec<Opd>,
    coords_count: i32,
    stride: i32,
    size: i32,
) {
    let packed = r.rb(ctx);
    for index in 0..coords_count {
        let o = ctx.ir.const_(index * stride);
        let s = ctx.ir.const_(size);
        sources.push(ctx.bitfield_extract_s32(packed, o, s));
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_texs(
    ctx: &mut EmitterContext<'_, '_>,
    texs_type: TexsType,
    imm: i32,
    write_mask: i32,
    src_a: i32,
    src_b: i32,
    dest: i32,
    dest2: i32,
    is_f16: bool,
) -> Result<(), String> {
    if dest == REGISTER_ZERO_INDEX && dest2 == REGISTER_ZERO_INDEX {
        return Ok(());
    }
    let mut sources: Vec<Opd> = Vec::new();
    let mut r = Regs { a: src_a, b: src_b };
    let mut ty;
    let mut flags;
    match texs_type {
        TexsType::Texs => {
            let texs_op = InstTexs(ctx.curr_op.raw);
            ty = convert_sampler_type_texs(texs_op.target());
            if ty == SamplerType::NONE {
                ctx.acc.log("Invalid texture sampler type.");
                return Ok(());
            }
            flags = convert_texture_flags_texs(texs_op.target());
            if ty.0 & SamplerType::ARRAY != 0 {
                let array_index = r.ra(ctx);
                sources.push(r.ra(ctx));
                sources.push(r.rb(ctx));
                sources.push(array_index);
                if ty.0 & SamplerType::SHADOW != 0 {
                    sources.push(r.rb(ctx));
                }
                if flags & texflags::LOD_LEVEL != 0 {
                    sources.push(ctx.ir.const_f(0.0));
                }
            } else {
                match texs_op.target() {
                    TexsTarget::Texture1DLodZero => {
                        sources.push(r.ra(ctx));
                        // Sample1DAs2D
                        sources.push(ctx.ir.const_f(0.0));
                        ty = SamplerType((ty.0 & !SamplerType::MASK) | SamplerType::TEXTURE_2D.0);
                        sources.push(ctx.ir.const_f(0.0));
                    }
                    TexsTarget::Texture2D => {
                        sources.push(r.ra(ctx));
                        sources.push(r.rb(ctx));
                    }
                    TexsTarget::Texture2DLodZero => {
                        sources.push(r.ra(ctx));
                        sources.push(r.rb(ctx));
                        sources.push(ctx.ir.const_f(0.0));
                    }
                    TexsTarget::Texture2DLodLevel
                    | TexsTarget::Texture2DDepthCompare
                    | TexsTarget::Texture3D
                    | TexsTarget::TextureCube => {
                        sources.push(r.ra(ctx));
                        sources.push(r.ra(ctx));
                        sources.push(r.rb(ctx));
                    }
                    TexsTarget::Texture2DLodZeroDepthCompare | TexsTarget::Texture3DLodZero => {
                        sources.push(r.ra(ctx));
                        sources.push(r.ra(ctx));
                        sources.push(r.rb(ctx));
                        sources.push(ctx.ir.const_f(0.0));
                    }
                    TexsTarget::Texture2DLodLevelDepthCompare | TexsTarget::TextureCubeLodLevel => {
                        sources.push(r.ra(ctx));
                        sources.push(r.ra(ctx));
                        sources.push(r.rb(ctx));
                        sources.push(r.rb(ctx));
                    }
                    _ => {}
                }
            }
        }
        TexsType::Tlds => {
            let tlds_op = InstTlds(ctx.curr_op.raw);
            let target = tlds_op.target();
            ty = convert_sampler_type_tlds(target);
            if ty == SamplerType::NONE {
                ctx.acc.log("Invalid texel fetch sampler type.");
                return Ok(());
            }
            flags = convert_texture_flags_tlds(target) | texflags::INT_COORDS;
            if target == TldsTarget::Texture1DLodZero
                && ctx.acc.query_sampler_type(tlds_op.tid_b(), -1) == SamplerType::TEXTURE_BUFFER
            {
                ty = SamplerType::TEXTURE_BUFFER;
                flags &= !texflags::LOD_LEVEL;
            }
            match target {
                TldsTarget::Texture1DLodZero => {
                    sources.push(r.ra(ctx));
                    if ty != SamplerType::TEXTURE_BUFFER {
                        // Sample1DAs2D: the read is 2D at (x & 0x3FFF, x >> 14)
                        let x = *sources.last().ok_or("TLDS: no coordinate")?;
                        let m = ctx.ir.const_(0x3FFF);
                        let lo = ctx.bitwise_and(x, m);
                        if let Some(l) = sources.last_mut() {
                            *l = lo;
                        }
                        let c14 = ctx.ir.const_(14);
                        sources.push(ctx.shift_right_u32(x, c14));
                        ctx.acc.record_texel_fetch_1d(tlds_op.tid_b());
                        ty = SamplerType((ty.0 & !SamplerType::MASK) | SamplerType::TEXTURE_2D.0);
                        sources.push(ctx.ir.const_f(0.0));
                    }
                }
                TldsTarget::Texture1DLodLevel => {
                    sources.push(r.ra(ctx));
                    sources.push(ctx.ir.const_f(0.0));
                    ty = SamplerType((ty.0 & !SamplerType::MASK) | SamplerType::TEXTURE_2D.0);
                    sources.push(r.rb(ctx));
                }
                TldsTarget::Texture2DLodZero => {
                    sources.push(r.ra(ctx));
                    sources.push(r.rb(ctx));
                    sources.push(ctx.ir.const_(0));
                }
                TldsTarget::Texture2DLodZeroOffset => {
                    sources.push(r.ra(ctx));
                    sources.push(r.ra(ctx));
                    sources.push(ctx.ir.const_(0));
                }
                TldsTarget::Texture2DLodZeroMultisample
                | TldsTarget::Texture2DLodLevel
                | TldsTarget::Texture2DLodLevelOffset => {
                    sources.push(r.ra(ctx));
                    sources.push(r.ra(ctx));
                    sources.push(r.rb(ctx));
                }
                TldsTarget::Texture3DLodZero => {
                    sources.push(r.ra(ctx));
                    sources.push(r.ra(ctx));
                    sources.push(r.rb(ctx));
                    sources.push(ctx.ir.const_(0));
                }
                TldsTarget::Texture2DArrayLodZero => {
                    sources.push(r.rb(ctx));
                    sources.push(r.rb(ctx));
                    sources.push(r.ra(ctx));
                    sources.push(ctx.ir.const_(0));
                }
                _ => {}
            }
            if flags & texflags::OFFSET != 0 {
                let d = dimensions(ty)?;
                add_texture_offset(ctx, &mut r, &mut sources, d, 4, 4);
            }
        }
        TexsType::Tld4s => {
            let op = InstTld4s(ctx.curr_op.raw);
            if !(op.dc() || op.aoffi()) {
                sources.push(r.ra(ctx));
                sources.push(r.rb(ctx));
            } else {
                sources.push(r.ra(ctx));
                sources.push(r.ra(ctx));
            }
            ty = SamplerType::TEXTURE_2D;
            flags = texflags::GATHER;
            let depth_compare_index = sources.len();
            if op.aoffi() {
                let d = dimensions(ty)?;
                add_texture_offset(ctx, &mut r, &mut sources, d, 8, 6);
                flags |= texflags::OFFSET;
            }
            if op.dc() {
                let v = r.rb(ctx);
                sources.insert(depth_compare_index, v);
                ty = ty.with(SamplerType::SHADOW);
            } else {
                sources.push(ctx.ir.const_(op.tex_comp().0));
            }
        }
    }
    let mut rd0 = [ctx.ir.const_f(0.0), ctx.ir.const_f(0.0)];
    let mut rd1 = [ctx.ir.const_f(0.0), ctx.ir.const_f(0.0)];
    let handle = imm;
    let component_mask = *MASK_LUT[if dest2 == REGISTER_ZERO_INDEX { 0 } else { 1 }]
        .get(write_mask as usize)
        .ok_or("TEXS: write mask")?;
    let components_count = (component_mask as u32).count_ones() as i32;
    let mut dests: Vec<Option<Opd>> = Vec::new();
    for i in 0..components_count {
        let high = i >> 1;
        let low = i & 1;
        if is_f16 {
            let l = ctx.ir.local();
            if high != 0 {
                rd1[low as usize] = l;
            } else {
                rd0[low as usize] = l;
            }
            dests.push(Some(l));
        } else {
            let mut rd_index = if high != 0 { dest2 } else { dest };
            if rd_index < REGISTER_ZERO_INDEX {
                rd_index += low;
            }
            dests.push(Some(ctx.ir.register(rd_index, regtype::GPR)));
        }
    }
    emit_texture_sample(ctx, ty, flags, handle, component_mask, &dests, &sources)?;
    if is_f16 {
        let d0 = ctx.ir.register(dest, regtype::GPR);
        let p0 = ctx.pack_half2x16(rd0[0], rd0[1]);
        ctx.copy_to(d0, p0);
        let d1 = ctx.ir.register(dest2, regtype::GPR);
        let p1 = ctx.pack_half2x16(rd1[0], rd1[1]);
        ctx.copy_to(d1, p1);
    }
    Ok(())
}
