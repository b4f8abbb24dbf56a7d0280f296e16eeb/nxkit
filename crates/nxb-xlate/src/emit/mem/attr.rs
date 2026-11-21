//! `InstEmitAttribute.cs`: AL2P, ALD, AST, IPA, ISBERD, OUT.

use super::hlp::{REGISTER_ZERO_INDEX, get_dest, get_src_reg};
use crate::attr::{Stage, consts};
use crate::emit::attrmap;
use crate::emit::emitter::EmitterContext;
use crate::ir::inst::{IoVariable, StorageKind};
use crate::ir::ops::{Opd, regtype};
use crate::maxwell::enums::{IpaOp, OutType};
use crate::maxwell::insts::{
    InstAl2p, InstAld, InstAst, InstIpa, InstIsberd, InstOutC, InstOutI, InstOutR,
};
use crate::maxwell::names::Emit;
use crate::translation::props::support_buffer;

const TESS_COORD_X: i32 = 0x2f0;
const TESS_COORD_Y: i32 = 0x2f4;
/// `(int)SupportBufferField.RenderScale`
const FIELD_RENDER_SCALE: i32 = 5;

pub(super) fn al2p(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstAl2p(ctx.curr_op.raw);
    let d = get_dest(ctx, op.dest());
    let a = get_src_reg(ctx, op.src_a());
    let imm = ctx.ir.const_(op.imm11());
    let sum = ctx.i_add(a, imm);
    ctx.copy_to(d, sum);
    Ok(())
}

pub(super) fn ald(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstAld(ctx.curr_op.raw);
    emit_ald(
        ctx,
        op.dest(),
        op.src_a(),
        op.src_b(),
        op.imm11(),
        op.al_size().0,
        op.p(),
        op.o(),
        op.phys(),
    )
}

fn has_primitive_vertex(attr: i32) -> bool {
    attr != consts::PRIMITIVE_ID && attr != TESS_COORD_X && attr != TESS_COORD_Y
}

fn can_load_output(attr: i32) -> bool {
    attr != TESS_COORD_X && attr != TESS_COORD_Y
}

/// `FixedFuncToUserAttribute(translatorContext, attr, isOutput)` (the job accessor supports layer output from vertex and
/// tessellation stages, so the layer never moves to a user attribute).
fn fixed_func_to_user_attribute(
    ctx: &mut EmitterContext<'_, '_>,
    attr: i32,
    is_output: bool,
) -> i32 {
    let fixed_start_attr = 0;
    if attr == consts::FOG_COORD {
        fixed_func_to_user_attribute_base(ctx, attr, consts::FOG_COORD, fixed_start_attr, is_output)
    } else if (consts::FRONT_COLOR_DIFFUSE_R..consts::CLIP_DISTANCE0).contains(&attr) {
        fixed_func_to_user_attribute_base(
            ctx,
            attr,
            consts::FRONT_COLOR_DIFFUSE_R,
            fixed_start_attr + 1,
            is_output,
        )
    } else if (consts::TEX_COORD_BASE..consts::TEX_COORD_END).contains(&attr) {
        fixed_func_to_user_attribute_base(
            ctx,
            attr,
            consts::TEX_COORD_BASE,
            fixed_start_attr + 5,
            is_output,
        )
    } else {
        attr
    }
}

fn fixed_func_to_user_attribute_base(
    ctx: &mut EmitterContext<'_, '_>,
    attr: i32,
    base_attr: i32,
    base_index: i32,
    is_output: bool,
) -> i32 {
    let index = (attr - base_attr) >> 4;
    let user_attr_index = ctx
        .tc
        .program
        .attribute_usage
        .get_free_user_attribute(is_output, base_index + index);
    if (user_attr_index as u32) < consts::MAX_ATTRIBUTES as u32 {
        consts::USER_ATTRIBUTE_BASE + user_attr_index * 16 + (attr & 0xf)
    } else {
        ctx.acc.log(&format!(
            "No enough user attributes for fixed attribute offset 0x{attr:X}."
        ));
        attr
    }
}

/// `TryConvertIdToIndexForVulkan` (the target API is Vulkan).
fn try_convert_id_to_index_for_vulkan(ctx: &mut EmitterContext<'_, '_>, attr: i32) -> Option<Opd> {
    if attr == consts::INSTANCE_ID {
        let a = ctx.load_io(StorageKind::Input, IoVariable::InstanceIndex, None);
        let b = ctx.load_io(StorageKind::Input, IoVariable::BaseInstance, None);
        Some(ctx.i_subtract(a, b))
    } else if attr == consts::VERTEX_ID {
        Some(ctx.load_io(StorageKind::Input, IoVariable::VertexIndex, None))
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_ald(
    ctx: &mut EmitterContext<'_, '_>,
    dest: i32,
    src_a: i32,
    src_b: i32,
    imm11: i32,
    al_size: i32,
    p: bool,
    o: bool,
    phys: bool,
) -> Result<(), String> {
    let stage = ctx.tc.definitions.stage;
    // Some of those attributes are per invocation, so we should ignore any primitive vertex indexing for those.
    let mut has_primitive_vertex_ = attrmap::has_primitive_vertex(stage, o) && !p;
    if !phys {
        has_primitive_vertex_ &= has_primitive_vertex(imm11);
    }
    let prim_vertex = if has_primitive_vertex_ {
        let s = get_src_reg(ctx, src_b);
        Some(ctx.copy(s))
    } else {
        None
    };
    for index in 0..al_size + 1 {
        let rd = dest + index;
        if rd == REGISTER_ZERO_INDEX {
            break;
        }
        let rd_op = ctx.ir.register(rd, regtype::GPR);
        if phys {
            let a = get_src_reg(ctx, src_a);
            let base = ctx.ir.const_(consts::USER_ATTRIBUTE_BASE);
            let offset = ctx.i_subtract(a, base);
            let c4 = ctx.ir.const_(4);
            let vec_index = ctx.shift_right_u32(offset, c4);
            let c2 = ctx.ir.const_(2);
            let sh = ctx.shift_right_u32(offset, c2);
            let c3 = ctx.ir.const_(3);
            let elem_index = ctx.bitwise_and(sh, c3);
            let kind = if o {
                StorageKind::Output
            } else {
                StorageKind::Input
            };
            let v = ctx.load_io_a(
                kind,
                IoVariable::UserDefined,
                prim_vertex,
                vec_index,
                elem_index,
            );
            ctx.copy_to(rd_op, v);
        } else if src_b == REGISTER_ZERO_INDEX || p {
            let offset = fixed_func_to_user_attribute(ctx, imm11 + index * 4, o);
            let is_output = o && can_load_output(offset);
            let converted = if !p && !is_output {
                try_convert_id_to_index_for_vulkan(ctx, offset)
            } else {
                None
            };
            if let Some(v) = converted {
                ctx.copy_to(rd_op, v);
            } else {
                let mut value =
                    attrmap::generate_attribute_load(ctx, prim_vertex, offset, is_output, p);
                let d = &ctx.tc.definitions;
                // Sscaled and Uscaled attribute types do not exist for the job, so that conversion never applies.
                let scaled_path = (!d.supports_scaled_vertex_formats || ctx.vertex_as_compute)
                    && d.stage == Stage::Vertex
                    && !o
                    && (0x80..0x280).contains(&offset);
                if !scaled_path && offset == consts::PRIMITIVE_ID && halve_primitive_id() {
                    let one = ctx.ir.const_(1);
                    value = ctx.shift_right_s32(value, one);
                }
                ctx.copy_to(rd_op, value);
            }
        } else {
            let offset = fixed_func_to_user_attribute(ctx, imm11 + index * 4, o);
            let is_output = o && can_load_output(offset);
            let v = attrmap::generate_attribute_load(ctx, prim_vertex, offset, is_output, false);
            ctx.copy_to(rd_op, v);
        }
    }
    Ok(())
}

/// `HalvePrimitiveId` of the job's graphics state.
fn halve_primitive_id() -> bool {
    false
}

pub(super) fn ast(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstAst(ctx.curr_op.raw);
    emit_ast(
        ctx,
        op.src_a(),
        op.src_b(),
        op.imm11(),
        op.al_size().0,
        op.p(),
        op.phys(),
    )
}

pub(crate) fn emit_ast(
    ctx: &mut EmitterContext<'_, '_>,
    src_a: i32,
    data: i32,
    imm11: i32,
    al_size: i32,
    p: bool,
    phys: bool,
) -> Result<(), String> {
    for index in 0..al_size + 1 {
        if data + index > REGISTER_ZERO_INDEX {
            break;
        }
        let rd = ctx.ir.register(data + index, regtype::GPR);
        if phys {
            let a = get_src_reg(ctx, src_a);
            let base = ctx.ir.const_(consts::USER_ATTRIBUTE_BASE);
            let offset = ctx.i_subtract(a, base);
            let c4 = ctx.ir.const_(4);
            let vec_index = ctx.shift_right_u32(offset, c4);
            let c2 = ctx.ir.const_(2);
            let sh = ctx.shift_right_u32(offset, c2);
            let c3 = ctx.ir.const_(3);
            let elem_index = ctx.bitwise_and(sh, c3);
            let invocation_id = if attrmap::has_invocation_id(ctx.tc.definitions.stage, true) {
                Some(ctx.load_io(StorageKind::Input, IoVariable::InvocationId, None))
            } else {
                None
            };
            ctx.store_io_a(
                StorageKind::Output,
                IoVariable::UserDefined,
                invocation_id,
                vec_index,
                elem_index,
                rd,
            );
        } else {
            let offset = imm11 + index * 4;
            // `IsUsedOutputAttribute`: SetNextStage is never called, so the next stage's fixed function usage is unknown.
            let offset = fixed_func_to_user_attribute(ctx, offset, true);
            attrmap::generate_attribute_store(ctx, offset, p, rd);
        }
    }
    Ok(())
}

pub(super) fn ipa(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstIpa(ctx.curr_op.raw);
    let idx = if op.idx() {
        Some(get_src_reg(ctx, op.src_a()))
    } else {
        None
    };
    emit_ipa(
        ctx,
        idx,
        op.imm10(),
        op.ipa_op(),
        op.src_b(),
        op.sat(),
        op.dest(),
    )
}

fn generate_ipa_load(ctx: &mut EmitterContext<'_, '_>, offset: i32) -> Opd {
    attrmap::generate_attribute_load(ctx, None, offset, false, false)
}

/// `TryFixedFuncToUserAttributeIpa`.
fn try_fixed_func_to_user_attribute_ipa(
    ctx: &mut EmitterContext<'_, '_>,
    attr: i32,
) -> (bool, Opd) {
    // Front colors and the fog coordinate (a TODO in the C#: emulate the fixed function behaviour).
    if (consts::FRONT_COLOR_DIFFUSE_R..consts::BACK_COLOR_DIFFUSE_R).contains(&attr)
        || attr == consts::FOG_COORD
    {
        let a = fixed_func_to_user_attribute(ctx, attr, false);
        (true, generate_ipa_load(ctx, a))
    } else if (consts::BACK_COLOR_DIFFUSE_R..consts::CLIP_DISTANCE0).contains(&attr) {
        (
            true,
            ctx.ir
                .const_f(if ((attr >> 2) & 3) == 3 { 1.0 } else { 0.0 }),
        )
    } else if (consts::TEX_COORD_BASE..consts::TEX_COORD_END).contains(&attr) {
        let a = fixed_func_to_user_attribute(ctx, attr, false);
        (true, generate_ipa_load(ctx, a))
    } else {
        (false, generate_ipa_load(ctx, attr))
    }
}

pub(crate) fn emit_ipa(
    ctx: &mut EmitterContext<'_, '_>,
    idx_address: Option<Opd>,
    imm10: i32,
    ipa_op: IpaOp,
    src_b: i32,
    sat: bool,
    dest: i32,
) -> Result<(), String> {
    let mut res;
    let mut is_fixed_func = false;
    if let Some(idx) = idx_address {
        let base = ctx.ir.const_(consts::USER_ATTRIBUTE_BASE);
        let offset = ctx.i_subtract(idx, base);
        let c4 = ctx.ir.const_(4);
        let vec_index = ctx.shift_right_u32(offset, c4);
        let c2 = ctx.ir.const_(2);
        let sh = ctx.shift_right_u32(offset, c2);
        let c3 = ctx.ir.const_(3);
        let elem_index = ctx.bitwise_and(sh, c3);
        res = ctx.load_io_a(
            StorageKind::Input,
            IoVariable::UserDefined,
            None,
            vec_index,
            elem_index,
        );
        let c3b = ctx.ir.const_(3);
        let w = ctx.load_io_e(StorageKind::Input, IoVariable::FragmentCoord, None, c3b);
        res = ctx.fp_multiply(res, w);
    } else {
        let (fixed, r) = try_fixed_func_to_user_attribute_ipa(ctx, imm10);
        is_fixed_func = fixed;
        res = r;
        if (consts::USER_ATTRIBUTE_BASE..consts::USER_ATTRIBUTE_END).contains(&imm10) {
            let index = ((imm10 - consts::USER_ATTRIBUTE_BASE) >> 4) as usize;
            let imap = ctx
                .tc
                .definitions
                .imap_types
                .get(index)
                .ok_or("IPA: imap type index out of range")?;
            if imap.first_used_type() == crate::translation::header::PixelImap::Perspective {
                let c3 = ctx.ir.const_(3);
                let w = ctx.load_io_e(StorageKind::Input, IoVariable::FragmentCoord, None, c3);
                res = ctx.fp_multiply(res, w);
            }
        } else if imm10 == consts::POSITION_X || imm10 == consts::POSITION_Y {
            // FragCoord X/Y must be divided by the render target scale.
            let rs = ctx.ir.const_(FIELD_RENDER_SCALE);
            let z = ctx.ir.const_(0);
            let scale = ctx.load_b2(StorageKind::ConstantBuffer, support_buffer::BINDING, rs, z);
            res = ctx.fp_divide(res, scale);
            // YNegateEnabled is false in the job's graphics state.
        } else if imm10 == consts::PRIMITIVE_ID && halve_primitive_id() {
            let one = ctx.ir.const_(1);
            res = ctx.shift_right_s32(res, one);
        }
        // QueryHostHasFrontFacingBug is false.
    }
    if ipa_op == IpaOp::Multiply && !is_fixed_func {
        let multiplier = get_src_reg(ctx, src_b);
        res = ctx.fp_multiply(res, multiplier);
    }
    res = ctx.fp_saturate_if(res, sat);
    let d = get_dest(ctx, dest);
    ctx.copy_to(d, res);
    Ok(())
}

pub(super) fn isberd(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstIsberd(ctx.curr_op.raw);
    let d = get_dest(ctx, op.dest());
    let a = get_src_reg(ctx, op.src_a());
    ctx.copy_to(d, a);
    Ok(())
}

/// OUT (geometry shaders): `EmitVertex` and `EndPrimitive`.
pub(super) fn out(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let raw = ctx.curr_op.raw;
    let out_type = match ctx.curr_op.emit {
        Some(Emit::OutR) => InstOutR(raw).out_type(),
        Some(Emit::OutI) => InstOutI(raw).out_type(),
        _ => InstOutC(raw).out_type(),
    };
    let emit = out_type.0 & OutType::Emit.0 != 0;
    let cut = out_type.0 & OutType::Cut.0 != 0;
    if !(emit || cut) {
        ctx.acc.log("Invalid OUT encoding.");
    }
    if emit {
        if ctx.tc.definitions.last_in_vertex_pipeline {
            // The position is only rewritten for the viewport transform disable and the depth mode, which fail in
            // `prepare_for_vertex_return`, so there is no saved position to restore after the vertex.
            ctx.prepare_for_vertex_return()?;
        }
        ctx.emit_vertex()?;
    }
    if cut {
        ctx.end_primitive()?;
    }
    Ok(())
}
