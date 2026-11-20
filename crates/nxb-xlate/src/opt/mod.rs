//! `Optimizer` and the optimization passes it runs.
#![allow(missing_docs)]

pub mod bindless_array;
pub mod bindless_convert;
pub mod bindless_elim;
pub mod branch_elim;
pub mod double_to_float;
pub mod fold;
pub mod frag_w;
pub mod gts;
pub mod gts_fn;
pub mod gts_raw;
pub mod simplify;
pub mod tex;
pub mod utils;
pub mod xmad;

use crate::ir::{Block, Inst, Ir, NodeId, Opd, OperandType};
use std::collections::VecDeque;

type R<T> = Result<T, String>;

/// The collaborators of the passes that register resources.
pub struct Env<'a, 'j> {
    pub rm: &'a mut crate::translation::resources::ResourceManager,
    pub acc: &'a mut crate::accessor::Accessor<'j>,
}

impl Resources for crate::translation::resources::ResourceManager {
    fn try_get_constant_buffer_slot(&self, binding: i32) -> Option<i32> {
        crate::translation::resources::ResourceManager::try_get_constant_buffer_slot(self, binding)
    }
}

/// What the optimizer asks of the `ResourceManager`.
pub trait Resources {
    /// `ResourceManager.TryGetConstantBufferSlot`.
    fn try_get_constant_buffer_slot(&self, binding: i32) -> Option<i32>;
}

/// `HasSideEffects`.
pub fn has_side_effects(ir: &Ir, n: NodeId) -> bool {
    if let crate::ir::Node::Op(o) = &ir.nodes[n.0 as usize] {
        return matches!(
            Inst(o.inst.0 & Inst::MASK),
            Inst::AtomicAdd
                | Inst::AtomicAnd
                | Inst::AtomicCompareAndSwap
                | Inst::AtomicMaxS32
                | Inst::AtomicMaxU32
                | Inst::AtomicMinS32
                | Inst::AtomicMinU32
                | Inst::AtomicOr
                | Inst::AtomicSwap
                | Inst::AtomicXor
                | Inst::Call
                | Inst::ImageAtomic
        );
    }
    false
}

/// `DestIsLocalVar`.
pub fn dest_is_local_var(ir: &Ir, n: NodeId) -> bool {
    let dests = ir.node_dests(n);
    if dests.is_empty() {
        return false;
    }
    dests
        .iter()
        .flatten()
        .all(|d| ir.ty(*d) == OperandType::LocalVariable)
}

/// `DestHasNoUses`.
pub fn dest_has_no_uses(ir: &Ir, n: NodeId) -> bool {
    ir.node_dests(n)
        .iter()
        .flatten()
        .all(|d| ir.data(*d).use_ops.is_empty())
}

fn is_unused(ir: &Ir, n: NodeId) -> bool {
    !has_side_effects(ir, n) && dest_is_local_var(ir, n) && dest_has_no_uses(ir, n)
}

/// `Optimizer.RemoveNode(block, node)` for the node at `pos` of the block.
pub fn remove_node(ir: &mut Ir, block: &mut Block, pos: usize) {
    let node = block.ops.remove(pos);
    remove_node_uses(ir, node);
}

/// The use-list part of `RemoveNode`, for a node that is already out of its block.
pub fn remove_node_uses(ir: &mut Ir, node: NodeId) {
    let mut queue: VecDeque<NodeId> = VecDeque::new();
    queue.push_back(node);
    while let Some(node) = queue.pop_front() {
        for src in ir.node_sources(node).into_iter().flatten() {
            if ir.ty(src) != OperandType::LocalVariable {
                continue;
            }
            let set = &mut ir.operands[src.0 as usize].use_ops;
            if set.remove(node)
                && set.is_empty()
                && let Some(asg) = ir.data(src).asg_op
                && !has_side_effects(ir, asg)
                && dest_has_no_uses(ir, asg)
            {
                queue.push_back(asg);
            }
        }
    }
}

/// Replace every source equal to `dest` in the nodes that use `dest` by `src` (`PropagateCopy` / `PropagatePhi`).
fn replace_uses(ir: &mut Ir, dest: Opd, src: Opd) {
    let uses: Vec<NodeId> = ir.data(dest).use_ops.iter().collect();
    for u in uses {
        let n = ir.node_sources(u).len();
        for i in 0..n {
            if ir.node_sources(u)[i] == Some(dest) {
                ir.set_node_source(u, i, Some(src));
            }
        }
    }
}

fn propagate_phi(ir: &mut Ir, phi: NodeId) -> R<bool> {
    if !utils::are_all_sources_the_same_operand(ir, phi)? {
        return Ok(false);
    }
    let first = ir.node_sources(phi)[0].ok_or("phi source")?;
    let dest = ir.node_dests(phi)[0].ok_or("phi dest")?;
    replace_uses(ir, dest, first);
    Ok(true)
}

fn src(ir: &Ir, n: NodeId, i: usize) -> R<Opd> {
    ir.op(n)
        .sources
        .get(i)
        .copied()
        .flatten()
        .ok_or_else(|| "missing source".to_string())
}

fn propagate_pack(ir: &mut Ir, pack: NodeId) -> R<bool> {
    let mut modified = false;
    let dest = ir.op(pack).dests[0].ok_or("dest")?;
    let (s0, s1) = (src(ir, pack, 0)?, src(ir, pack, 1)?);
    let uses: Vec<NodeId> = ir.data(dest).use_ops.iter().collect();
    for u in uses {
        if !ir.is_op(u) || ir.op(u).inst != Inst::UnpackHalf2x16 {
            continue;
        }
        if src(ir, u, 0)? == dest {
            let pick = if ir.op(u).index == 1 { s1 } else { s0 };
            ir.turn_into_copy(u, pick);
            modified = true;
        }
    }
    Ok(modified)
}

fn match_ddx_or_ddy(ir: &mut Ir, op: NodeId) -> R<bool> {
    let mut modified = false;
    let (src2, src3) = (src(ir, op, 1)?, src(ir, op, 2)?);
    if ir.ty(src2) != OperandType::Constant || (ir.value(src2) != 1 && ir.value(src2) != 2) {
        return Ok(false);
    }
    if ir.ty(src3) != OperandType::Constant || ir.value(src3) != 0x1c03 {
        return Ok(false);
    }
    let is_ddy = ir.value(src2) == 2;
    let is_ddx = !is_ddy;
    let dest = ir.op(op).dests[0].ok_or("dest")?;
    let uses: Vec<NodeId> = ir.data(dest).use_ops.iter().collect();
    for u in uses {
        if !ir.is_op(u) || ir.op(u).inst != Inst::SwizzleAdd {
            continue;
        }
        let (a, b, c) = (src(ir, u, 0)?, src(ir, u, 1)?, src(ir, u, 2)?);
        if a != dest || b != src(ir, op, 0)? || ir.ty(c) != OperandType::Constant {
            continue;
        }
        let mask = ir.value(c);
        if (is_ddx && mask != 0b10011001) || (is_ddy && mask != 0b10100101) {
            continue;
        }
        ir.turn_into(u, if is_ddx { Inst::Ddx } else { Inst::Ddy }, b);
        modified = true;
    }
    Ok(modified)
}

/// `Optimizer.RunOptimizationPasses`.
pub fn run_optimization_passes(ir: &mut Ir, blocks: &mut [Block], res: &dyn Resources) -> R<()> {
    loop {
        let mut modified = false;
        for bi in 0..blocks.len() {
            let mut i = 0;
            while i < blocks[bi].ops.len() {
                let node = blocks[bi].ops[i];
                let mut unused = is_unused(ir, node);
                if !ir.is_op(node) || unused {
                    if ir.is_phi(node) && !unused {
                        unused = propagate_phi(ir, node)?;
                    }
                    if unused {
                        remove_node(ir, &mut blocks[bi], i);
                        modified = true;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                fold::run_pass(ir, res, node)?;
                simplify::run_pass(ir, node)?;
                if dest_is_local_var(ir, node) {
                    let inst = ir.op(node).inst;
                    if inst == Inst::Copy {
                        let dest = ir.op(node).dests[0].ok_or("dest")?;
                        let s = src(ir, node, 0)?;
                        replace_uses(ir, dest, s);
                        remove_node(ir, &mut blocks[bi], i);
                        modified = true;
                        continue;
                    } else if (inst == Inst::PackHalf2x16 && propagate_pack(ir, node)?)
                        || (inst == Inst::ShuffleXor && match_ddx_or_ddy(ir, node)?)
                    {
                        if dest_has_no_uses(ir, node) {
                            remove_node(ir, &mut blocks[bi], i);
                            modified = true;
                            continue;
                        }
                        modified = true;
                    }
                }
                i += 1;
            }
            if branch_elim::run_pass(ir, blocks, bi)? {
                let last = blocks[bi].ops.len().checked_sub(1).ok_or("empty block")?;
                remove_node(ir, &mut blocks[bi], last);
                modified = true;
            }
        }
        if !modified {
            return Ok(());
        }
    }
}

/// Facts of the translation the optimizer reads from the `TransformContext`.
pub struct PassCtx {
    pub fragment: bool,
    pub host_supports_float64: bool,
}

/// `Optimizer.RunPass`.
pub fn run_pass(
    ir: &mut Ir,
    blocks: &mut [Block],
    env: &mut Env,
    ctx: &PassCtx,
    hfm: &mut crate::helper::Hfm,
) -> R<()> {
    for b in blocks.iter_mut() {
        xmad::run_pass(ir, b)?;
    }
    run_optimization_passes(ir, blocks, &*env.rm)?;
    gts::run_pass(ir, blocks, env, hfm)?;
    for bi in 0..blocks.len() {
        bindless_array::run_block(ir, blocks, bi, env)?;
        bindless_elim::run_block(ir, blocks, bi, env, true)?;
        if ctx.fragment {
            let b = blocks[bi].clone();
            frag_w::run_block(ir, &b)?;
        }
        if !ctx.host_supports_float64 {
            double_to_float::run_block(ir, &mut blocks[bi], hfm)?;
        }
    }
    for bi in 0..blocks.len() {
        bindless_elim::run_block(ir, blocks, bi, env, false)?;
    }
    run_optimization_passes(ir, blocks, &*env.rm)
}

/// First half of `Optimizer.RunPass` that needs nothing beyond the block list (Xmad and the optimization loop).
pub fn run_basic(ir: &mut Ir, blocks: &mut [Block], res: &dyn Resources) -> R<()> {
    for b in blocks.iter_mut() {
        xmad::run_pass(ir, b)?;
    }
    run_optimization_passes(ir, blocks, res)
}

#[cfg(test)]
pub(crate) mod tests;
