//! `InstEmitSurface.cs`: SUATOM and SURED (image atomics), shared helpers.

use super::hlp::{REGISTER_ZERO_INDEX, get_src_reg};
use crate::accessor::SetBinding;
use crate::emit::emitter::EmitterContext;
use crate::ir::inst::Inst;
use crate::ir::ops::{Opd, regtype, texflags};
use crate::maxwell::enums::{SuDim, SuSize, SuatomOp, SuatomSize};
use crate::maxwell::insts::*;
use crate::translation::props::{ShaderProperties, TextureFlags, dimensions};
use crate::types::{SamplerType, TEXTURE_FORMATS};

pub(super) const DEFAULT_CBUF_SLOT: i32 = -1;
/// `TranslationFlags.MetalImageAtomics` is set by the job driver.
pub(super) const METAL_ATOMICS: bool = true;

pub(super) fn fmt(name: &str) -> usize {
    TEXTURE_FORMATS.iter().position(|n| *n == name).unwrap_or(0)
}

pub(super) fn convert_sampler_type(d: SuDim) -> SamplerType {
    match d {
        SuDim::_1d => SamplerType::TEXTURE_1D,
        SuDim::_1dBuffer => SamplerType::TEXTURE_BUFFER,
        SuDim::_1dArray => SamplerType::TEXTURE_1D.with(SamplerType::ARRAY),
        SuDim::_2d => SamplerType::TEXTURE_2D,
        SuDim::_2dArray => SamplerType::TEXTURE_2D.with(SamplerType::ARRAY),
        SuDim::_3d => SamplerType::TEXTURE_3D,
        _ => SamplerType::NONE,
    }
}

pub(super) fn atom_size_log2(s: SuatomSize) -> i32 {
    match s {
        SuatomSize::U64 | SuatomSize::S64 | SuatomSize::Sd64 => 3,
        _ => 2,
    }
}

pub(super) fn atom_format(s: SuatomSize) -> usize {
    match s {
        SuatomSize::U32 | SuatomSize::Sd32 => fmt("R32Uint"),
        SuatomSize::S32 => fmt("R32Sint"),
        SuatomSize::U64 | SuatomSize::S64 | SuatomSize::Sd64 => fmt("R32G32Uint"),
        SuatomSize::F32FtzRn => fmt("R32Float"),
        SuatomSize::F16x2FtzRn => fmt("R16G16Float"),
        _ => fmt("R32Uint"),
    }
}

/// `UniformHandleAtomicFormat`: an image whose handle is in the game's own uniform buffer has no descriptor to ask,
/// so the instruction's size names the format (Metal's atomics take only R32Uint and R32Sint).
fn uniform_handle_atomic_format(size: SuatomSize) -> usize {
    if matches!(size, SuatomSize::S32 | SuatomSize::Sd32) {
        fmt("R32Sint")
    } else {
        fmt("R32Uint")
    }
}

pub(super) fn atomic_op_flags(op: SuatomOp) -> u32 {
    match op {
        SuatomOp::Add => texflags::ADD,
        SuatomOp::Min => texflags::MINIMUM,
        SuatomOp::Max => texflags::MAXIMUM,
        SuatomOp::Inc => texflags::INCREMENT,
        SuatomOp::Dec => texflags::DECREMENT,
        SuatomOp::And => texflags::BITWISE_AND,
        SuatomOp::Or => texflags::BITWISE_OR,
        SuatomOp::Xor => texflags::BITWISE_XOR,
        SuatomOp::Exch => texflags::SWAP,
        _ => texflags::ADD,
    }
}

pub(super) fn su_components(s: SuSize) -> i32 {
    match s {
        SuSize::B64 => 2,
        SuSize::B128 | SuSize::UB128 => 4,
        _ => 1,
    }
}

pub(super) fn su_size_log2(s: SuSize) -> i32 {
    match s {
        SuSize::U8 | SuSize::S8 => 0,
        SuSize::U16 | SuSize::S16 => 1,
        SuSize::B64 => 3,
        SuSize::B128 | SuSize::UB128 => 4,
        _ => 2,
    }
}

pub(super) fn su_format(s: SuSize) -> usize {
    match s {
        SuSize::U8 => fmt("R8Uint"),
        SuSize::S8 => fmt("R8Sint"),
        SuSize::U16 => fmt("R16Uint"),
        SuSize::S16 => fmt("R16Sint"),
        SuSize::B64 => fmt("R32G32Uint"),
        SuSize::B128 | SuSize::UB128 => fmt("R32G32B32A32Uint"),
        _ => fmt("R32Uint"),
    }
}

pub(super) fn next_reg(ctx: &mut EmitterContext<'_, '_>, idx: &mut i32) -> Opd {
    if *idx > REGISTER_ZERO_INDEX {
        return ctx.ir.const_(0);
    }
    let r = ctx.ir.register(*idx, regtype::GPR);
    *idx += 1;
    ctx.copy(r)
}

/// The coordinates shared by all surface instructions (`Sample1DAs2D` is on).
pub(super) fn coords(
    ctx: &mut EmitterContext<'_, '_>,
    ty: &mut SamplerType,
    src_a: &mut i32,
    sources: &mut Vec<Opd>,
) -> Result<(), String> {
    let n = dimensions(*ty)?;
    for _ in 0..n {
        let r = next_reg(ctx, src_a);
        sources.push(r);
    }
    if ty.0 & SamplerType::MASK == SamplerType::TEXTURE_1D.0 {
        sources.push(ctx.ir.const_(0));
        *ty = SamplerType((ty.0 & !SamplerType::MASK) | SamplerType::TEXTURE_2D.0);
    }
    if ty.0 & SamplerType::ARRAY != 0 {
        let r = next_reg(ctx, src_a);
        sources.push(r);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn binding(
    ctx: &mut EmitterContext<'_, '_>,
    bindless: bool,
    inst: Inst,
    ty: SamplerType,
    format: usize,
    flags: u32,
    cbuf_slot: i32,
    handle: i32,
) -> Result<SetBinding, String> {
    if bindless {
        return Ok(SetBinding { set: 0, binding: 0 });
    }
    ctx.rm.get_texture_or_image_binding(
        ctx.acc,
        inst,
        ty,
        format,
        TextureFlags(flags),
        cbuf_slot,
        handle,
        1,
        false,
    )
}

macro_rules! suatom {
    ($name:ident, $inst:ident, $bindless:expr, $cas:expr, $tid:expr, $srcc:expr, $op:expr, $pred:expr) => {
        pub(super) fn $name(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
            let op = $inst(ctx.curr_op.raw);
            #[allow(clippy::redundant_closure_call)]
            let (tid, src_c, aop, has_pred) = (($tid)(&op), ($srcc)(&op), ($op)(&op), ($pred)(&op));
            emit_suatom(
                ctx,
                op.dim(),
                op.clamp().0,
                aop,
                op.size(),
                tid,
                op.src_a(),
                op.src_b(),
                src_c,
                op.dest(),
                op.ba(),
                $bindless,
                $cas,
                has_pred,
                DEFAULT_CBUF_SLOT,
            )
        }
    };
}

suatom!(
    suatom_b,
    InstSuatomB,
    true,
    false,
    |_o: &InstSuatomB| 0,
    |o: &InstSuatomB| o.src_c(),
    |o: &InstSuatomB| o.op(),
    |_o: &InstSuatomB| false
);
suatom!(
    suatom,
    InstSuatom,
    false,
    false,
    |o: &InstSuatom| o.tid_b(),
    |_o: &InstSuatom| 0,
    |o: &InstSuatom| o.op(),
    |_o: &InstSuatom| false
);
suatom!(
    suatom_b2,
    InstSuatomB2,
    true,
    false,
    |_o: &InstSuatomB2| 0,
    |o: &InstSuatomB2| o.src_c(),
    |o: &InstSuatomB2| o.op(),
    |o: &InstSuatomB2| o.dest_pred() != 7
);
suatom!(
    suatom_cas_b,
    InstSuatomCasB,
    true,
    true,
    |_o: &InstSuatomCasB| 0,
    |o: &InstSuatomCasB| o.src_c(),
    |_o: &InstSuatomCasB| SuatomOp(0),
    |_o: &InstSuatomCasB| false
);
suatom!(
    suatom_cas,
    InstSuatomCas,
    false,
    true,
    |o: &InstSuatomCas| o.tid_b(),
    |_o: &InstSuatomCas| 0,
    |_o: &InstSuatomCas| SuatomOp(0),
    |_o: &InstSuatomCas| false
);

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_suatom(
    ctx: &mut EmitterContext<'_, '_>,
    dims: SuDim,
    clamp: i32,
    atomic_op: SuatomOp,
    size: SuatomSize,
    imm: i32,
    src_a: i32,
    src_b: i32,
    src_c: i32,
    dest: i32,
    byte_address: bool,
    is_bindless: bool,
    compare_and_swap: bool,
    has_predicate_result: bool,
    cbuf_slot: i32,
) -> Result<(), String> {
    if METAL_ATOMICS
        && !(size == SuatomSize::U32 || size == SuatomSize::S32 || size == SuatomSize::Sd32)
    {
        return Err("Metal image atomics require a 32-bit integer source size.".into());
    }
    let mut ty = convert_sampler_type(dims);
    if ty == SamplerType::NONE {
        ctx.acc.log("Invalid image atomic sampler type.");
        return Ok(());
    }
    let mut a = src_a;
    let mut b = src_b;
    let d = ctx.ir.register(dest, regtype::GPR);
    let mut sources: Vec<Opd> = Vec::new();
    if is_bindless {
        let r = get_src_reg(ctx, src_c);
        sources.push(ctx.copy(r));
    }
    coords(ctx, &mut ty, &mut a, &mut sources)?;
    if byte_address {
        let x = if is_bindless { 1 } else { 0 };
        let c = ctx.ir.const_(atom_size_log2(size));
        let v = *sources.get(x).ok_or("SUATOM: no x coordinate")?;
        sources[x] = ctx.shift_right_s32(v, c);
    }
    let format = if size == SuatomSize::Sd32 || size == SuatomSize::Sd64 {
        if is_bindless {
            0
        } else if cbuf_slot != DEFAULT_CBUF_SLOT {
            uniform_handle_atomic_format(size)
        } else {
            ShaderProperties::get_texture_format_atomic(ctx.acc, imm, -1, METAL_ATOMICS)?
        }
    } else {
        atom_format(size)
    };
    if compare_and_swap {
        let r = next_reg(ctx, &mut b);
        sources.push(r);
    }
    let r = next_reg(ctx, &mut b);
    sources.push(r);
    let mut flags = (if compare_and_swap {
        texflags::CAS
    } else {
        atomic_op_flags(atomic_op)
    }) | ((clamp as u32) << 20);
    if METAL_ATOMICS {
        flags |= texflags::ATOMIC_METAL;
    }
    if dest == REGISTER_ZERO_INDEX && !compare_and_swap && !has_predicate_result {
        flags |= texflags::ATOMIC_DISCARD_RESULT;
    }
    if has_predicate_result {
        flags |= texflags::ATOMIC_PREDICATE_RESULT;
    }
    if is_bindless {
        flags |= texflags::BINDLESS;
    }
    let sb = binding(
        ctx,
        is_bindless,
        Inst::ImageAtomic,
        ty,
        format,
        flags,
        cbuf_slot,
        imm,
    )?;
    let res = ctx.image_atomic(ty, format, flags, sb, &sources);
    ctx.copy_to(d, res);
    Ok(())
}

pub(super) fn sured_b(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstSuredB(ctx.curr_op.raw);
    emit_sured(
        ctx,
        op.dim(),
        op.clamp().0,
        SuatomOp(op.op().0),
        op.size(),
        0,
        op.src_a(),
        op.dest(),
        op.src_c(),
        op.ba(),
        true,
        DEFAULT_CBUF_SLOT,
    )
}

pub(super) fn sured(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstSured(ctx.curr_op.raw);
    emit_sured(
        ctx,
        op.dim(),
        op.clamp().0,
        SuatomOp(op.op().0),
        op.size(),
        op.tid_b(),
        op.src_a(),
        op.dest(),
        0,
        op.ba(),
        false,
        DEFAULT_CBUF_SLOT,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_sured(
    ctx: &mut EmitterContext<'_, '_>,
    dims: SuDim,
    clamp: i32,
    atomic_op: SuatomOp,
    size: SuatomSize,
    imm: i32,
    src_a: i32,
    src_b: i32,
    src_c: i32,
    byte_address: bool,
    is_bindless: bool,
    cbuf_slot: i32,
) -> Result<(), String> {
    if METAL_ATOMICS
        && !(size == SuatomSize::U32 || size == SuatomSize::S32 || size == SuatomSize::Sd32)
    {
        return Err("Metal image atomics require a 32-bit integer source size.".into());
    }
    let mut ty = convert_sampler_type(dims);
    if ty == SamplerType::NONE {
        ctx.acc.log("Invalid image reduction sampler type.");
        return Ok(());
    }
    let mut a = src_a;
    let mut b = src_b;
    let mut sources: Vec<Opd> = Vec::new();
    if is_bindless {
        let r = get_src_reg(ctx, src_c);
        sources.push(ctx.copy(r));
    }
    coords(ctx, &mut ty, &mut a, &mut sources)?;
    if byte_address {
        let x = if is_bindless { 1 } else { 0 };
        let c = ctx.ir.const_(atom_size_log2(size));
        let v = *sources.get(x).ok_or("SURED: no x coordinate")?;
        sources[x] = ctx.shift_right_s32(v, c);
    }
    let format = if size == SuatomSize::Sd32 || size == SuatomSize::Sd64 {
        if is_bindless {
            0
        } else if cbuf_slot != DEFAULT_CBUF_SLOT {
            uniform_handle_atomic_format(size)
        } else {
            ShaderProperties::get_texture_format_atomic(ctx.acc, imm, -1, METAL_ATOMICS)?
        }
    } else {
        atom_format(size)
    };
    let r = next_reg(ctx, &mut b);
    sources.push(r);
    let mut flags =
        atomic_op_flags(atomic_op) | ((clamp as u32) << 20) | texflags::ATOMIC_DISCARD_RESULT;
    if METAL_ATOMICS {
        flags |= texflags::ATOMIC_METAL;
    }
    if is_bindless {
        flags |= texflags::BINDLESS;
    }
    let sb = binding(
        ctx,
        is_bindless,
        Inst::ImageAtomic,
        ty,
        format,
        flags,
        cbuf_slot,
        imm,
    )?;
    ctx.image_atomic(ty, format, flags, sb, &sources);
    Ok(())
}
