//! `Translator.EmitShader`, `EmitOutputsInitialization` and `EmitOps` (the Maxwell path).

use super::context::TranslatorContext;
use super::fsi_match::{self, FunctionType};
use super::host;
use super::resources::ResourceManager;
use crate::accessor::Accessor;
use crate::attr::Stage;
use crate::emit;
use crate::ir::emitter::EmitterContext;
use crate::ir::inst::{IoVariable, StorageKind};
use crate::ir::{Ir, NodeId, regtype};
use crate::maxwell::decoder::Block;
use crate::maxwell::insts::InstConditional;
use crate::maxwell::names::InstName;
use crate::maxwell::table::props;

/// Arena, functions and the emitter errors of a lenient run.
pub type EmitResult = (Ir, Vec<FunctionCode>, Vec<(String, String)>);

/// `FunctionCode`: the operations of one function.
pub struct FunctionCode {
    pub code: Vec<NodeId>,
}

/// `EmitShader` for a context with `initialize_outputs` (the main shader). The arena is threaded through `ir`.
pub fn emit_shader(
    tc: &TranslatorContext,
    acc: &mut Accessor,
    rm: &mut ResourceManager,
    ir: Ir,
    initialize_outputs: bool,
) -> Result<(Ir, Vec<FunctionCode>), String> {
    emit_shader_mode(tc, acc, rm, ir, initialize_outputs, false).map(|(ir, f, _)| (ir, f))
}

/// `emit_shader` for a program translated as compute (`asCompute`): the stage is emulated by a compute shader.
pub fn emit_shader_compute(
    tc: &TranslatorContext,
    acc: &mut Accessor,
    rm: &mut ResourceManager,
    ir: Ir,
    initialize_outputs: bool,
) -> Result<(Ir, Vec<FunctionCode>), String> {
    emit_shader_full(tc, acc, rm, ir, initialize_outputs, false, true).map(|(ir, f, _)| (ir, f))
}

/// `emit_shader` with the lenient test mode: also returns the recorded emitter errors (instruction name, message).
pub fn emit_shader_mode(
    tc: &TranslatorContext,
    acc: &mut Accessor,
    rm: &mut ResourceManager,
    ir: Ir,
    initialize_outputs: bool,
    lenient: bool,
) -> Result<EmitResult, String> {
    emit_shader_full(tc, acc, rm, ir, initialize_outputs, lenient, false)
}

fn emit_shader_full(
    tc: &TranslatorContext,
    acc: &mut Accessor,
    rm: &mut ResourceManager,
    ir: Ir,
    initialize_outputs: bool,
    lenient: bool,
    vertex_as_compute: bool,
) -> Result<EmitResult, String> {
    let types = if tc.options.ampere {
        vec![FunctionType::User; tc.program.functions.len()]
    } else {
        fsi_match::run_pass(&tc.program)
    };
    let builtins: Vec<(u64, FunctionType)> = tc
        .program
        .functions
        .iter()
        .zip(&types)
        .filter(|(_, t)| **t != FunctionType::User)
        .map(|(f, t)| (f.address, *t))
        .collect();
    // `program.Where(!IsCompilerGenerated).OrderBy(Address)`: the position is the function id.
    let mut order: Vec<usize> = (0..tc.program.functions.len())
        .filter(|&i| types[i] == FunctionType::User)
        .collect();
    order.sort_by_key(|&i| tc.program.functions[i].address);
    let addresses: Vec<u64> = order
        .iter()
        .map(|&i| tc.program.functions[i].address)
        .collect();
    let mut ir = ir;
    let mut funcs = Vec::with_capacity(order.len());
    let mut errors = Vec::new();
    for (index, &fi) in order.iter().enumerate() {
        let function = &tc.program.functions[fi];
        let mut ctx = EmitterContext::new(ir, tc, acc, rm, vertex_as_compute, index != 0);
        ctx.lenient = lenient;
        ctx.function_addresses.clone_from(&addresses);
        ctx.builtin_functions.clone_from(&builtins);
        ctx.emit_start()?;
        if initialize_outputs && index == 0 {
            emit_outputs_initialization(&mut ctx)?;
        }
        ctx.curr_function = Some(function);
        for block in &function.blocks {
            ctx.curr_block = Some(block);
            ctx.enter_block(block.address);
            if tc.options.ampere {
                emit::ampere::ops::emit_ops_ampere(&mut ctx, block)?;
            } else {
                emit_ops(&mut ctx, block)?;
            }
        }
        errors.append(&mut ctx.errors);
        let (i, code) = ctx.finish();
        ir = i;
        funcs.push(FunctionCode { code });
    }
    Ok((ir, funcs, errors))
}

fn emit_outputs_initialization(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let stage = ctx.tc.definitions.stage;
    if stage == Stage::Compute {
        return Ok(());
    }
    if stage == Stage::Fragment {
        // dual source blend is off in the job's graphics state
        return Ok(());
    }
    if matches!(stage, Stage::Vertex | Stage::TessellationEvaluation) {
        for c in 0..4 {
            let v = ctx.ir.const_f(if c == 3 { 1.0 } else { 0.0 });
            let ci = ctx.ir.const_(c);
            ctx.store_io_e(StorageKind::Output, IoVariable::Position, None, ci, v);
        }
        if ctx.tc.program.clip_distances_written != 0 {
            for i in 0..8 {
                let v = ctx.ir.const_f(0.0);
                let ci = ctx.ir.const_(i);
                ctx.store_io_e(StorageKind::Output, IoVariable::ClipDistance, None, ci, v);
            }
        }
    }
    if stage == Stage::Geometry {
        let d = &ctx.tc.definitions;
        if d.gp_passthrough
            && ctx.uses_viewport_mask()
            && !ctx.acc.query_host_supports_geometry_shader_passthrough()
            && host::SUPPORTS_VIEWPORT_MASK
        {
            // A passthrough shader writes no mask: all viewports stay enabled.
            let c0 = ctx.ir.const_(0);
            let all = ctx.ir.const_(-1);
            ctx.store_io_e(StorageKind::Output, IoVariable::ViewportMask, None, c0, all);
        }
    }
    // NextInputAttributesComponents, per-patch and fixed function attributes only exist after SetNextStage (never called).
    if !matches!(
        stage,
        Stage::Vertex | Stage::Geometry | Stage::TessellationEvaluation
    ) {
        return Err("unported: output initialisation for this stage".into());
    }
    Ok(())
}

fn emit_ops(ctx: &mut EmitterContext<'_, '_>, block: &Block) -> Result<(), String> {
    for (op_index, op) in block.ops.iter().enumerate() {
        if ctx.tc.options.debug_mode {
            let name = if op.emit.is_some() {
                format!("{:?}", op.name)
            } else {
                ctx.acc.log(&format!(
                    "Invalid instruction at 0x{:06X} (0x{:016X}).",
                    op.address, op.raw
                ));
                "???".to_string()
            };
            ctx.comment(&format!("0x{:06X}: 0x{:016X} {}", op.address, op.raw, name));
        }
        let cond = InstConditional(op.raw);
        let mut no_pred = op.props & props::NO_PRED != 0;
        if !no_pred && cond.pred() == 7 && cond.pred_inv() {
            continue;
        }
        let mut pred_skip = None;
        if matches!(op.name, InstName::Brk | InstName::Cont | InstName::Sync) {
            no_pred = block.sync_targets.len() <= 1;
        } else if op.name == InstName::Bra {
            no_pred = true;
        }
        if !(cond.pred() == 7 || no_pred) {
            let label = if op_index == block.ops.len() - 1 && block.has_next() {
                let succ = *block.succs.first().ok_or("block without successor")?;
                ctx.get_label(ctx.block_address(succ))
            } else {
                let l = ctx.ir.label();
                pred_skip = Some(l);
                l
            };
            let pred = ctx.ir.register(cond.pred(), regtype::PREDICATE);
            if cond.pred_inv() {
                ctx.branch_if_true(label, pred);
            } else {
                ctx.branch_if_false(label, pred);
            }
        }
        ctx.curr_op = *op;
        if let Some(e) = op.emit
            && let Err(m) = emit::dispatch(ctx, e)
        {
            if !ctx.lenient {
                return Err(m);
            }
            ctx.errors.push((format!("{:?}", op.name), m));
        }
        if let Some(l) = pred_skip {
            ctx.mark_label(l);
        }
    }
    Ok(())
}
