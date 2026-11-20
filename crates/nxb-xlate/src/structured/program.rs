//! `StructuredProgram.MakeStructuredProgram`.

use super::ast::{A, AstTex, Kind};
use super::context::Context;
use super::{StructInfo, StructResources, goto, helper, optimizer};
use crate::ir::agg::*;
use crate::ir::*;
use crate::props::Definitions;

const FUNC_PARAMETER_TYPE: AggType = AggType::S32;

/// A function as the structuring sees it (`Function`): blocks after all transforms.
pub struct FunctionIn<'a> {
    pub name: String,
    pub blocks: &'a mut Vec<Block>,
    pub in_arguments_count: usize,
    pub out_arguments_count: usize,
    pub returns_value: bool,
}

fn is_branch_inst(i: Inst) -> bool {
    matches!(i, Inst::Branch | Inst::BranchIfFalse | Inst::BranchIfTrue)
}

/// `PhiFunctions.Remove`.
fn remove_phis(ir: &mut Ir, blocks: &mut [Block]) -> Result<(), String> {
    for blk in 0..blocks.len() {
        let mut i = 0;
        while i < blocks[blk].ops.len() {
            let n = blocks[blk].ops[i];
            let Node::Phi(phi) = &ir.nodes[n.0 as usize] else {
                i += 1;
                continue;
            };
            let phi = phi.clone();
            let temp = ir.local();
            for (src_block, src) in &phi.sources {
                let copy = ir.new_op(Inst::Copy, StorageKind::None, Some(temp), &[*src]);
                append(ir, &mut blocks[*src_block as usize], copy);
            }
            let dest = phi.dest.ok_or("phi without destination")?;
            let copy2 = ir.new_op(Inst::Copy, StorageKind::None, Some(dest), &[temp]);
            // the new node goes after the phi, then the phi is removed: net effect, it takes the phi's place
            // (appending to a predecessor can insert into this very block, before `i`)
            let pos = blocks[blk]
                .ops
                .iter()
                .position(|x| *x == n)
                .ok_or("phi vanished")?;
            blocks[blk].ops[pos] = copy2;
            i = pos + 1;
        }
    }
    Ok(())
}

/// `BasicBlock.Append`: before a trailing control flow instruction.
fn append(ir: &Ir, block: &mut Block, node: NodeId) {
    if let Some(&last) = block.ops.last()
        && let Node::Op(o) = &ir.nodes[last.0 as usize]
        && matches!(
            o.inst,
            Inst::Branch | Inst::BranchIfFalse | Inst::BranchIfTrue | Inst::Discard | Inst::Return
        )
    {
        let at = block.ops.len() - 1;
        block.ops.insert(at, node);
        return;
    }
    block.ops.push(node);
}

/// `MakeStructuredProgram` (the target language is SPIR-V).
pub fn make_structured_program(
    ir: &mut Ir,
    functions: &mut [FunctionIn],
    defs: &Definitions,
    resources: &mut dyn StructResources,
    passthrough_attributes: i32,
    debug_mode: bool,
) -> Result<StructInfo, String> {
    let mut ctx = Context::new(defs, resources, passthrough_attributes, debug_mode);
    let counts: Vec<usize> = functions.iter().map(|f| f.in_arguments_count).collect();
    for f in functions.iter_mut() {
        let return_type = if f.returns_value {
            FUNC_PARAMETER_TYPE
        } else {
            AggType::VOID
        };
        ctx.enter_function(
            f.blocks.len(),
            &f.name,
            return_type,
            vec![FUNC_PARAMETER_TYPE; f.in_arguments_count],
            vec![FUNC_PARAMETER_TYPE; f.out_arguments_count],
        );
        remove_phis(ir, f.blocks)?;
        for blk in 0..f.blocks.len() {
            ctx.enter_block(ir, f.blocks, blk)?;
            for n in f.blocks[blk].ops.clone() {
                match &ir.nodes[n.0 as usize] {
                    Node::Comment(c) => {
                        let c = ctx.ast.new_comment(c);
                        ctx.add_node(c)?;
                    }
                    Node::Op(o) if is_branch_inst(o.inst) => {
                        ctx.leave_block(ir, f.blocks, blk, n)?;
                    }
                    Node::Op(_) => add_operation(&mut ctx, ir, n, &counts)?,
                    Node::Phi(_) => return Err("phi node left after removal".into()),
                }
            }
        }
        let gotos = std::mem::take(&mut ctx.gotos);
        goto::eliminate(&mut ctx.ast, &gotos)?;
        optimizer::optimize(&mut ctx)?;
        ctx.leave_function();
    }
    let mut info = ctx.info;
    info.ast = ctx.ast;
    Ok(info)
}

fn is_vector_dest_inst(i: Inst) -> bool {
    matches!(i, Inst::ImageLoad | Inst::TextureSample)
}

fn is_bitwise_inst(i: Inst) -> bool {
    matches!(
        i,
        Inst::BitwiseAnd | Inst::BitwiseExclusiveOr | Inst::BitwiseNot | Inst::BitwiseOr
    )
}

fn logical_from_bitwise(i: Inst) -> Inst {
    match i {
        Inst::BitwiseAnd => Inst::LogicalAnd,
        Inst::BitwiseExclusiveOr => Inst::LogicalExclusiveOr,
        Inst::BitwiseNot => Inst::LogicalNot,
        _ => Inst::LogicalOr,
    }
}

fn operand_var_type(ctx: &Context, a: A) -> Option<AggType> {
    match ctx.ast.kind(a) {
        Kind::Operand { var_type, .. } => Some(*var_type),
        _ => None,
    }
}

fn set_var_type(ctx: &mut Context, a: A, t: AggType) {
    if let Kind::Operand { var_type, .. } = &mut ctx.ast.nodes[a.0 as usize].kind {
        *var_type = t;
    }
}

fn add_operation(
    ctx: &mut Context,
    ir: &Ir,
    node: NodeId,
    in_arg_counts: &[usize],
) -> Result<(), String> {
    let operation = ir.op(node).clone();
    let mut inst = operation.inst;
    let storage_kind = operation.storage_kind;
    let src = |i: usize| -> Result<Opd, String> {
        operation
            .sources
            .get(i)
            .copied()
            .flatten()
            .ok_or_else(|| "missing source".to_string())
    };

    if inst == Inst::Load || inst == Inst::Store {
        let is_io = matches!(
            storage_kind,
            StorageKind::Input
                | StorageKind::InputPerPatch
                | StorageKind::Output
                | StorageKind::OutputPerPatch
        );
        if is_io {
            let io_variable = IoVariable(ir.value(src(0)?) as u32);
            let is_output = matches!(
                storage_kind,
                StorageKind::Output | StorageKind::OutputPerPatch
            );
            let mut location = 0;
            let mut component = 0;
            if ctx
                .defs
                .has_per_location_input_or_output(io_variable, is_output)
            {
                location = ir.value(src(1)?);
                if operation.sources.len() > 2 {
                    let s2 = src(2)?;
                    if ir.ty(s2) == OperandType::Constant
                        && ctx.defs.has_per_location_input_or_output_component(
                            io_variable,
                            location,
                            ir.value(s2),
                            is_output,
                        )
                    {
                        component = ir.value(s2);
                    }
                }
            }
            ctx.info.add_io(super::IoDef {
                storage_kind,
                io_variable,
                location,
                component,
            });
        } else if storage_kind == StorageKind::ConstantBuffer
            && ir.ty(src(0)?) == OperandType::Constant
        {
            ctx.resources
                .set_used_constant_buffer_binding(ir.value(src(0)?));
        }
    }

    let vector_dest = is_vector_dest_inst(inst);
    let sources_count = operation.sources.len();
    let out_dests_count = if !operation.dests.is_empty() && !vector_dest {
        operation.dests.len() - 1
    } else {
        0
    };
    let mut sources: Vec<A> = Vec::with_capacity(sources_count + out_dests_count);

    if inst == Inst::Call {
        let func_index = ir.value(src(0)?);
        let first = ctx.ast.new_operand(OperandType::Constant, func_index);
        sources.push(first);
        let in_args = *in_arg_counts
            .get(func_index as usize)
            .ok_or("call to an unknown function")?;
        for index in 1..sources_count {
            let source = ctx.get_operand_or_cb_load(ir, src(index)?)?;
            if index - 1 < in_args {
                let arg_temp = ctx.new_temp(FUNC_PARAMETER_TYPE);
                let asg = ctx.ast.new_assign(arg_temp, source);
                ctx.add_node(asg)?;
                sources.push(arg_temp);
            } else {
                sources.push(source);
            }
        }
    } else {
        for index in 0..sources_count {
            let s = ctx.get_operand_or_cb_load(ir, src(index)?)?;
            sources.push(s);
        }
    }

    for index in 0..out_dests_count {
        let oper = ctx
            .get_operand(ir, operation.dests[1 + index])
            .ok_or("null out destination")?;
        let t = src_var_type(inst, sources_count + index)?;
        set_var_type(ctx, oper, t);
        sources.push(oper);
    }

    let components_count = (operation.index as u32).count_ones();

    if vector_dest && components_count > 1 {
        let mut dest_type = dest_var_type(inst)?;
        let source;
        if let Some(t) = &operation.tex {
            if inst == Inst::ImageLoad {
                dest_type = format_component_type(t.format);
            }
            source = make_tex_with(ctx, &operation, inst, sources)?;
        } else {
            source = ctx.ast.new_op(
                inst,
                operation.storage_kind,
                operation.force_precise,
                operation.index,
                sources,
            );
        }
        let dest_elem_type = dest_type;
        match components_count {
            2 => dest_type |= AggType::VECTOR2,
            3 => dest_type |= AggType::VECTOR3,
            4 => dest_type |= AggType::VECTOR4,
            _ => {}
        }
        let dest_vec = ctx.new_temp(dest_type);
        let asg = ctx.ast.new_assign(dest_vec, source);
        ctx.add_node(asg)?;
        for i in 0..operation.dests.len() {
            let dest = ctx
                .get_operand(ir, operation.dests[i])
                .ok_or("null vector destination")?;
            let index = ctx.ast.new_operand(OperandType::Constant, i as i32);
            set_var_type(ctx, dest, dest_elem_type);
            let ex = ctx.ast.new_op(
                Inst::VectorExtract,
                StorageKind::None,
                false,
                0,
                vec![dest_vec, index],
            );
            let asg = ctx.ast.new_assign(dest, ex);
            ctx.add_node(asg)?;
        }
    } else if let Some(dest_opd) = operation.dests.first().copied().flatten() {
        let dest = ctx
            .get_operand(ir, Some(dest_opd))
            .ok_or("null destination")?;
        // all sources bool: use the short-circuiting logical operations
        if is_bitwise_inst(inst) && all_source_types_equal(ctx, &sources, AggType::BOOL) {
            inst = logical_from_bitwise(inst);
        }
        let is_cond_sel = inst == Inst::ConditionalSelect;
        let is_copy = inst == Inst::Copy;
        if is_cond_sel || is_copy {
            let ty = var_type_from_uses(ir, dest_opd)?;
            if is_cond_sel && ty == AggType::FP32 {
                inst = Inst(inst.0 | Inst::FP32);
            }
            set_var_type(ctx, dest, ty);
        } else {
            let t = dest_var_type(inst)?;
            set_var_type(ctx, dest, t);
        }
        let source;
        if let Some(t) = &operation.tex {
            if inst == Inst::ImageLoad {
                set_var_type(ctx, dest, format_component_type(t.format));
            }
            source = make_tex_with(ctx, &operation, inst, sources)?;
        } else if !is_copy {
            source = ctx.ast.new_op(
                inst,
                operation.storage_kind,
                operation.force_precise,
                operation.index,
                sources,
            );
        } else {
            source = sources[0];
        }
        let asg = ctx.ast.new_assign(dest, source);
        ctx.add_node(asg)?;
    } else if operation.tex.is_some() {
        let t = make_tex_with(ctx, &operation, inst, sources)?;
        ctx.add_node(t)?;
    } else {
        let o = ctx.ast.new_op(
            inst,
            operation.storage_kind,
            operation.force_precise,
            operation.index,
            sources,
        );
        ctx.add_node(o)?;
    }

    match Inst(operation.inst.0) {
        Inst::MultiplyHighS32 => ctx.info.helper_functions_mask |= helper::MULTIPLY_HIGH_S32,
        Inst::MultiplyHighU32 => ctx.info.helper_functions_mask |= helper::MULTIPLY_HIGH_U32,
        Inst::SwizzleAdd => ctx.info.helper_functions_mask |= helper::SWIZZLE_ADD,
        Inst::FSIBegin | Inst::FSIEnd => ctx.info.helper_functions_mask |= helper::FSI,
        _ => {}
    }
    Ok(())
}

fn make_tex_with(
    ctx: &mut Context,
    operation: &Operation,
    inst: Inst,
    sources: Vec<A>,
) -> Result<A, String> {
    let t = operation.tex.as_ref().ok_or("not a texture operation")?;
    Ok(ctx.ast.new_tex_op(
        inst,
        AstTex {
            ty: t.ty,
            format: t.format,
            flags: t.flags,
            set: t.set,
            binding: t.binding,
            sampler_set: t.sampler_set,
            sampler_binding: t.sampler_binding,
        },
        operation.index,
        sources,
    ))
}

fn all_source_types_equal(ctx: &Context, sources: &[A], ty: AggType) -> bool {
    sources
        .iter()
        .all(|s| operand_var_type(ctx, *s) == Some(ty))
}

/// `GetVarTypeFromUses`.
fn var_type_from_uses(ir: &Ir, dest: Opd) -> Result<AggType, String> {
    let mut visited: Vec<Opd> = vec![dest];
    let mut pending: std::collections::VecDeque<Opd> = std::collections::VecDeque::new();
    pending.push_back(dest);
    while let Some(operand) = pending.pop_front() {
        for use_node in ir.data(operand).use_ops.iter() {
            let Node::Op(operation) = &ir.nodes[use_node.0 as usize] else {
                continue;
            };
            if operation.inst == Inst::Copy {
                let d = operation
                    .dests
                    .first()
                    .copied()
                    .flatten()
                    .ok_or("copy without destination")?;
                if ir.ty(d) == OperandType::LocalVariable {
                    if !visited.contains(&d) {
                        visited.push(d);
                        pending.push_back(d);
                        break;
                    }
                } else {
                    return var_type_of_operand_type(ir.ty(d));
                }
            } else {
                for (index, s) in operation.sources.iter().enumerate() {
                    if *s == Some(operand) {
                        return src_var_type(operation.inst, index);
                    }
                }
            }
        }
    }
    Ok(AggType::S32)
}
