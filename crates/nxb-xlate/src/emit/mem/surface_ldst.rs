//! `InstEmitSurface.cs`: SULD and SUST.

use super::hlp::REGISTER_ZERO_INDEX;
use crate::emit::emitter::EmitterContext;
use crate::ir::inst::Inst;
use crate::ir::ops::{Opd, regtype, texflags};
use crate::maxwell::enums::{CacheOpLd, CacheOpSt, SuDim, SuSize};
use crate::maxwell::insts::*;
use crate::translation::props::ShaderProperties;
use crate::types::SamplerType;

use super::surface::*;
pub(super) fn suld_d_b(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstSuldDB(ctx.curr_op.raw);
    emit_suld(
        ctx,
        op.cache_op(),
        op.dim(),
        op.size(),
        0,
        0,
        op.src_a(),
        op.dest(),
        op.src_c(),
        false,
        op.ba(),
        true,
    )
}

pub(super) fn suld_d(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstSuldD(ctx.curr_op.raw);
    emit_suld(
        ctx,
        op.cache_op(),
        op.dim(),
        op.size(),
        op.tid_b(),
        0,
        op.src_a(),
        op.dest(),
        0,
        false,
        op.ba(),
        false,
    )
}

pub(super) fn suld_b(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstSuldB(ctx.curr_op.raw);
    emit_suld(
        ctx,
        op.cache_op(),
        op.dim(),
        SuSize(0),
        0,
        op.rgba().0,
        op.src_a(),
        op.dest(),
        op.src_c(),
        true,
        false,
        true,
    )
}

pub(super) fn suld(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstSuld(ctx.curr_op.raw);
    emit_suld(
        ctx,
        op.cache_op(),
        op.dim(),
        SuSize(0),
        op.tid_b(),
        op.rgba().0,
        op.src_a(),
        op.dest(),
        0,
        true,
        false,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_suld(
    ctx: &mut EmitterContext<'_, '_>,
    cache_op: CacheOpLd,
    dims: SuDim,
    size: SuSize,
    imm: i32,
    component_mask: i32,
    src_a: i32,
    src_b: i32,
    src_c: i32,
    use_components: bool,
    byte_address: bool,
    is_bindless: bool,
) -> Result<(), String> {
    if src_b == REGISTER_ZERO_INDEX {
        return Ok(());
    }
    let mut ty = convert_sampler_type(dims);
    if ty == SamplerType::NONE {
        ctx.acc.log("Invalid image store sampler type.");
        return Ok(());
    }
    let mut a = src_a;
    let mut sources: Vec<Opd> = Vec::new();
    if is_bindless {
        let r = ctx.ir.register(src_c, regtype::GPR);
        sources.push(ctx.copy(r));
    }
    coords(ctx, &mut ty, &mut a, &mut sources)?;
    let handle = imm;
    let mut flags = if is_bindless {
        texflags::BINDLESS
    } else {
        texflags::NONE
    };
    if cache_op == CacheOpLd::Cg {
        flags |= texflags::COHERENT;
    }
    let (comp_mask, format, dests);
    if use_components {
        let n = (component_mask as u32).count_ones() as i32;
        let mut d = Vec::new();
        for i in 0..n {
            if src_b + i >= REGISTER_ZERO_INDEX {
                break;
            }
            d.push(Some(ctx.ir.register(src_b + i, regtype::GPR)));
        }
        dests = d;
        format = if is_bindless {
            0
        } else {
            ShaderProperties::get_texture_format(ctx.acc, handle, -1)
        };
        comp_mask = component_mask;
    } else {
        if byte_address {
            let x = if is_bindless { 1 } else { 0 };
            let c = ctx.ir.const_(su_size_log2(size));
            let v = *sources.get(x).ok_or("SULD: no x coordinate")?;
            sources[x] = ctx.shift_right_s32(v, c);
        }
        let components = su_components(size);
        comp_mask = (1 << components) - 1;
        let mut d = Vec::new();
        for i in 0..components {
            if src_b + i >= REGISTER_ZERO_INDEX {
                break;
            }
            d.push(Some(ctx.ir.register(src_b + i, regtype::GPR)));
        }
        dests = d;
        format = su_format(size);
    }
    let sb = binding(
        ctx,
        is_bindless,
        Inst::ImageLoad,
        ty,
        format,
        flags,
        DEFAULT_CBUF_SLOT,
        handle,
    )?;
    ctx.image_load(ty, format, flags, sb, comp_mask, &dests, &sources);
    if !use_components {
        let d0 = dests
            .first()
            .copied()
            .flatten()
            .ok_or("SULD: no destination")?;
        match size {
            SuSize::U8 | SuSize::U16 => {
                let bits = if size == SuSize::U8 { 8 } else { 16 };
                let m = ctx.ir.const_((u32::MAX >> (32 - bits)) as i32);
                let v = ctx.bitwise_and(d0, m);
                ctx.copy_to(d0, v);
            }
            SuSize::S8 | SuSize::S16 => {
                let bits = if size == SuSize::S8 { 8 } else { 16 };
                let z = ctx.ir.const_(0);
                let w = ctx.ir.const_(bits);
                let v = ctx.bitfield_extract_s32(d0, z, w);
                ctx.copy_to(d0, v);
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn sust_d_b(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstSustDB(ctx.curr_op.raw);
    emit_sust(
        ctx,
        op.cache_op(),
        op.dim(),
        op.size(),
        0,
        0,
        op.src_a(),
        op.dest(),
        op.src_c(),
        false,
        op.ba(),
        true,
    )
}

pub(super) fn sust_d(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstSustD(ctx.curr_op.raw);
    emit_sust(
        ctx,
        op.cache_op(),
        op.dim(),
        op.size(),
        op.tid_b(),
        0,
        op.src_a(),
        op.dest(),
        0,
        false,
        op.ba(),
        false,
    )
}

pub(super) fn sust_b(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstSustB(ctx.curr_op.raw);
    emit_sust(
        ctx,
        op.cache_op(),
        op.dim(),
        SuSize(0),
        0,
        op.rgba().0,
        op.src_a(),
        op.dest(),
        op.src_c(),
        true,
        false,
        true,
    )
}

pub(super) fn sust(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstSust(ctx.curr_op.raw);
    emit_sust(
        ctx,
        op.cache_op(),
        op.dim(),
        SuSize(0),
        op.tid_b(),
        op.rgba().0,
        op.src_a(),
        op.dest(),
        0,
        true,
        false,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_sust(
    ctx: &mut EmitterContext<'_, '_>,
    cache_op: CacheOpSt,
    dims: SuDim,
    size: SuSize,
    imm: i32,
    component_mask: i32,
    src_a: i32,
    src_b: i32,
    src_c: i32,
    use_components: bool,
    byte_address: bool,
    is_bindless: bool,
) -> Result<(), String> {
    let mut ty = convert_sampler_type(dims);
    if ty == SamplerType::NONE {
        ctx.acc.log("Invalid image store sampler type.");
        return Ok(());
    }
    let mut a = src_a;
    let mut b = src_b;
    let mut sources: Vec<Opd> = Vec::new();
    if is_bindless {
        let r = ctx.ir.register(src_c, regtype::GPR);
        sources.push(ctx.copy(r));
    }
    coords(ctx, &mut ty, &mut a, &mut sources)?;
    let mut format = 0;
    if use_components {
        let mut comp_mask = component_mask;
        while comp_mask != 0 {
            if comp_mask & 1 != 0 {
                let r = next_reg(ctx, &mut b);
                sources.push(r);
            }
            comp_mask >>= 1;
        }
        if !is_bindless {
            format = ShaderProperties::get_texture_format(ctx.acc, imm, -1);
        }
    } else {
        if byte_address {
            let x = if is_bindless { 1 } else { 0 };
            let c = ctx.ir.const_(su_size_log2(size));
            let v = *sources.get(x).ok_or("SUST: no x coordinate")?;
            sources[x] = ctx.shift_right_s32(v, c);
        }
        for _ in 0..su_components(size) {
            let r = next_reg(ctx, &mut b);
            sources.push(r);
        }
        format = su_format(size);
    }
    let mut flags = if is_bindless {
        texflags::BINDLESS
    } else {
        texflags::NONE
    };
    if cache_op == CacheOpSt::Cg {
        flags |= texflags::COHERENT;
    }
    let sb = binding(
        ctx,
        is_bindless,
        Inst::ImageStore,
        ty,
        format,
        flags,
        DEFAULT_CBUF_SLOT,
        imm,
    )?;
    ctx.image_store(ty, format, flags, sb, &sources);
    Ok(())
}
