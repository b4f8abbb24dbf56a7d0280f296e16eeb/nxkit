//! `InstEmitFlowControl.cs`.

use super::cond::get_condition;
use crate::emit::emitter::EmitterContext;
use crate::ir::ops::{FALSE, OperandType, regtype};
use crate::maxwell::enums::Ccc;
use crate::maxwell::insts::*;
use crate::maxwell::names::Emit;
use crate::translation::fsi_match::FunctionType;

type Ctx<'a, 'b> = EmitterContext<'a, 'b>;
type R = Result<(), String>;

fn emit_branch(ctx: &mut Ctx, address: u64) {
    let op = ctx.curr_op;
    let cond = InstConditional(op.raw);
    // a branch to the next instruction is useless
    if address == op.address + 8 {
        return;
    }
    let label = ctx.get_label(address);
    let mut pred = ctx.ir.register(cond.pred(), regtype::PREDICATE);
    if cond.ccc() != Ccc::T {
        let c = get_condition(ctx, cond.ccc(), crate::ir::ops::TRUE);
        if cond.pred() == 7 {
            pred = c;
        } else if cond.pred_inv() {
            let n = ctx.bitwise_not(pred);
            pred = ctx.bitwise_and(n, c);
        } else {
            pred = ctx.bitwise_and(pred, c);
        }
        ctx.branch_if_true(label, pred);
    } else if cond.pred() == 7 {
        ctx.branch(label);
    } else if cond.pred_inv() {
        ctx.branch_if_false(label, pred);
    } else {
        ctx.branch_if_true(label, pred);
    }
}

fn emit_pbk_pcnt_ssy(ctx: &mut Ctx) -> R {
    let blk = ctx.curr_block.ok_or("no current block")?;
    let func = ctx.curr_function.ok_or("no current function")?;
    let addr = ctx.curr_op.address;
    let info = blk
        .push_ops
        .iter()
        .find(|p| p.op.address == addr)
        .ok_or("push op not found in block")?;
    for &(consumer, tag) in &info.consumers {
        let st = func.blocks[consumer]
            .sync_targets
            .iter()
            .find(|(k, _)| *k == addr)
            .ok_or("sync target not found")?
            .1;
        let local = ctx.tag_local(tag);
        let id = ctx.ir.const_(st.push_op_id as i32);
        ctx.copy_to(local, id);
    }
    Ok(())
}

fn emit_brk_cont_sync(ctx: &mut Ctx) -> R {
    let blk = ctx.curr_block.ok_or("no current block")?;
    let func = ctx.curr_function.ok_or("no current function")?;
    let targets = &blk.sync_targets;
    if targets.len() == 1 {
        let st = targets[0].1;
        let push_op = func.blocks[st.push.0].push_ops[st.push.1].op;
        emit_branch(ctx, push_op.absolute_address());
    } else {
        let me = ctx.curr_block_index()?;
        for (_, st) in targets {
            let info = &func.blocks[st.push.0].push_ops[st.push.1];
            let label = ctx.get_label(info.op.absolute_address());
            let tag = info
                .consumers
                .iter()
                .find(|c| c.0 == me)
                .ok_or("consumer not found")?
                .1;
            let local = ctx.tag_local(tag);
            let id = ctx.ir.const_(st.push_op_id as i32);
            let eq = ctx.i_compare_equal(local, id);
            ctx.branch_if_true(label, eq);
        }
    }
    Ok(())
}

fn brx(ctx: &mut Ctx) -> R {
    let op = InstBrx(ctx.curr_op.raw);
    let curr_op = ctx.curr_op;
    let blk = ctx.curr_block.ok_or("no current block")?;
    let func = ctx.curr_function.ok_or("no current function")?;
    let start = usize::from(blk.has_next());
    if blk.succs.len() <= start {
        ctx.acc.log(&format!(
            "Failed to find targets for BRX instruction at 0x{:X}.",
            curr_op.address
        ));
        return Ok(());
    }
    let offset = curr_op.absolute_address() as i32;
    let reg = ctx.ir.register(op.src_a(), regtype::GPR);
    let off = ctx.ir.const_(offset);
    let address = ctx.i_add(reg, off);
    let targets: Vec<&crate::maxwell::decoder::Block> = blk.succs[start..]
        .iter()
        .map(|&i| &func.blocks[i])
        .collect();
    let mut asc = targets.clone();
    asc.sort_by_key(|b| b.address);
    let total = blk.succs.len() - start;
    let mut all_single_pred = true;
    for (n, t) in asc.iter().enumerate() {
        if n + 1 < total && (t.preds.len() > 1 || t.address <= blk.address) {
            all_single_pred = false;
            break;
        }
    }
    if all_single_pred {
        let mut current: Option<u64> = None;
        let mut first = 0;
        for next in &asc {
            match current {
                Some(c) => {
                    if c != next.address {
                        ctx.set_brx_target(c, address, c as i32, next.address);
                    }
                }
                None => first = next.address,
            }
            current = Some(next.address);
        }
        let l = ctx.get_label(first);
        ctx.branch(l);
    } else {
        let mut desc = targets;
        desc.sort_by_key(|b| std::cmp::Reverse(b.address));
        let mut last = u64::MAX;
        for (n, t) in desc.iter().enumerate() {
            let label = ctx.get_label(t.address);
            if n + 1 < total {
                if t.address != last {
                    let c = ctx.ir.const_(t.address as i32);
                    let eq = ctx.i_compare_equal(address, c);
                    ctx.branch_if_true(label, eq);
                }
                last = t.address;
            } else {
                ctx.branch(label);
            }
        }
    }
    Ok(())
}

/// `Cal`: a call of a user function or the begin and end of the fragment shader interlock.
fn cal(ctx: &mut Ctx) -> R {
    let address = ctx.curr_op.absolute_address();
    if let Some((_, ty)) = ctx.builtin_functions.iter().find(|(a, _)| *a == address) {
        match ty {
            FunctionType::FsiBegin => ctx.fsi_begin(),
            FunctionType::FsiEnd => ctx.fsi_end(),
            _ => {}
        }
        return Ok(());
    }
    let id = ctx
        .function_addresses
        .iter()
        .position(|a| *a == address)
        .ok_or("CAL to an unknown function")?;
    ctx.call(id as i32, false, &[]);
    Ok(())
}

fn exit(ctx: &mut Ctx) -> R {
    let op = InstExit(ctx.curr_op.raw);
    if ctx.is_non_main {
        ctx.acc.log("Invalid exit on non-main function.");
        return Ok(());
    }
    if op.ccc() == Ccc::T {
        if ctx.prepare_for_return()? {
            ctx.return_();
        }
    } else {
        let cond = get_condition(ctx, op.ccc(), FALSE);
        // an always false condition needs nothing
        if ctx.ir.ty(cond) != OperandType::Constant || ctx.ir.value(cond) != FALSE {
            let skip = ctx.ir.label();
            ctx.branch_if_false(skip, cond);
            if ctx.prepare_for_return()? {
                ctx.return_();
            }
            ctx.mark_label(skip);
        }
    }
    Ok(())
}

pub(super) fn try_emit(ctx: &mut Ctx, emit: Emit) -> Option<R> {
    Some(match emit {
        Emit::Bra => (|| {
            let blk = ctx.curr_block.ok_or("no current block")?;
            let last = *blk.succs.last().ok_or("BRA without successor")?;
            let a = ctx.block_address(last);
            emit_branch(ctx, a);
            Ok(())
        })(),
        Emit::Brk | Emit::Cont | Emit::Sync => emit_brk_cont_sync(ctx),
        Emit::Brx => brx(ctx),
        Emit::Exit => exit(ctx),
        Emit::Cal => cal(ctx),
        Emit::Kil => {
            ctx.discard();
            Ok(())
        }
        Emit::Pbk | Emit::Pcnt | Emit::Ssy => emit_pbk_pcnt_ssy(ctx),
        Emit::Ret => {
            if ctx.is_non_main {
                ctx.return_();
            } else {
                ctx.acc.log("Invalid return on main function.");
            }
            Ok(())
        }
        _ => return None,
    })
}
