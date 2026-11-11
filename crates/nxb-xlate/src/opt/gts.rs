//! `GlobalToStorage`: the inline storage path (global memory loads, stores and atomics with a known storage buffer).
//! The helper-function paths (small integers, single and multi target functions, call generation) are in `gts_fn`.
//! the raw path (address traced to no storage buffer) fails closed.
use super::gts_fn::{self, Entry};
use super::{Env, utils};
use crate::ir::{Block, Inst, Ir, NodeId, Opd, OperandType, StorageKind};
use crate::xform::helper::Hfm;
use std::collections::HashMap;

type R<T> = Result<T, String>;

const DRIVER_RESERVED_CB: i32 = 0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum LsType {
    Local,
    Shared,
}

#[derive(Clone, Copy)]
struct Search {
    sb_cb_slot: i32,
    sb_cb_offset: i32,
    offset: Option<Opd>,
    const_offset: i32,
}

impl Search {
    fn found(&self) -> bool {
        self.sb_cb_slot != -1
    }
    fn not_found() -> Search {
        Search {
            sb_cb_slot: -1,
            sb_cb_offset: 0,
            offset: None,
            const_offset: 0,
        }
    }
}

type LsKey = (Option<Opd>, i32, LsType);

/// What the resolve of a group of raw word loads left for each member: the chosen table entry (0 when no single entry
/// holds the range) and that entry's host delta.
#[derive(Clone, Copy)]
struct Resolved {
    entry: Opd,
    delta_low: Opd,
    delta_high: Opd,
}

#[derive(Default)]
struct Gts {
    shared: HashMap<LsKey, Vec<(u32, Search)>>,
    entries: Vec<Entry>,
    /// The raw word loads of a group, by node.
    members: HashMap<NodeId, Resolved>,
    /// The helper that resolves a range and the one that loads in it, once made.
    range_fn: Option<i32>,
    load_fn: Option<i32>,
    /// The immediate dominator of each block, made when the first group looks across blocks.
    idom: Option<Vec<Option<usize>>>,
}

impl Gts {
    fn add_memory_target_cb(
        &mut self,
        ty: LsType,
        base: Option<Opd>,
        co: i32,
        target: u32,
        r: Search,
    ) {
        let entry = self.shared.entry((base, co, ty)).or_default();
        if let Some(e) = entry.iter_mut().find(|(t, _)| *t == target) {
            let ex = e.1;
            if ex.offset.is_some() && (ex.offset != r.offset || ex.const_offset != r.const_offset) {
                e.1 = Search {
                    sb_cb_slot: r.sb_cb_slot,
                    sb_cb_offset: r.sb_cb_offset,
                    offset: None,
                    const_offset: 0,
                };
            }
        } else {
            entry.push((target, r));
        }
    }

    fn try_get_memory_target_cb(&self, ty: LsType, base: Option<Opd>, co: i32) -> Option<Search> {
        let e = self.shared.get(&(base, co, ty))?;
        if e.len() == 1 && e[0].1.found() {
            return Some(e[0].1);
        }
        None
    }
}

fn s(ir: &Ir, n: NodeId, i: usize) -> R<Opd> {
    ir.op(n)
        .sources
        .get(i)
        .copied()
        .flatten()
        .ok_or_else(|| "missing source".to_string())
}

fn is_global_memory(k: StorageKind) -> bool {
    matches!(
        k,
        StorageKind::GlobalMemory
            | StorageKind::GlobalMemoryS8
            | StorageKind::GlobalMemoryS16
            | StorageKind::GlobalMemoryU8
            | StorageKind::GlobalMemoryU16
    )
}

pub fn run_pass(ir: &mut Ir, blocks: &mut [Block], env: &mut Env, hfm: &mut Hfm) -> R<()> {
    let mut ctx = Gts::default();
    for bi in 0..blocks.len() {
        let mut i = 0;
        while i < blocks[bi].ops.len() {
            let node = blocks[bi].ops[i];
            if !ir.is_op(node) {
                i += 1;
                continue;
            }
            let (inst, kind) = (ir.op(node).inst, ir.op(node).storage_kind);
            if is_global_memory(kind) {
                i = replace(ir, blocks, bi, i, env, &mut ctx, hfm)?;
            } else if inst == Inst::Store
                && (kind == StorageKind::SharedMemory || kind == StorageKind::LocalMemory)
            {
                if let Some((ty, base, co)) = try_get_memory_offsets(ir, node)? {
                    let n = ir.op(node).sources.len();
                    let value = s(ir, node, n - 1)?;
                    let r = find_unique_base_address_cb(ir, blocks, bi, &ctx, value, false)?;
                    if r.found() {
                        let target = gts_fn::pack_cb(r.sb_cb_slot, r.sb_cb_offset);
                        ctx.add_memory_target_cb(ty, base, co, target, r);
                    }
                }
                i += 1;
            } else {
                i += 1;
            }
        }
    }
    Ok(())
}

/// Returns the index to continue from.
fn replace(
    ir: &mut Ir,
    blocks: &mut [Block],
    bi: usize,
    pos: usize,
    env: &mut Env,
    ctx: &mut Gts,
    hfm: &mut Hfm,
) -> R<usize> {
    let node = blocks[bi].ops[pos];
    let global_address = s(ir, node, 0)?;
    let r = find_unique_base_address_cb(ir, blocks, bi, ctx, global_address, true)?;
    if !r.found() {
        let mut pos = pos;
        if ir.op(node).inst == Inst::Load
            && ir.op(node).storage_kind == StorageKind::GlobalMemory
            && !ctx.members.contains_key(&node)
            && plan_group(ir, blocks, bi, pos, env, ctx, hfm)?
        {
            // The resolve went in before the node
            pos = blocks[bi]
                .ops
                .iter()
                .position(|n| *n == node)
                .ok_or("group member lost")?;
        }
        if let Some(m) = ctx.members.get(&node).copied() {
            let id = ctx.load_fn.ok_or("group without a load helper")?;
            let extra = [m.entry, m.delta_low, m.delta_high];
            return Ok(call_storage_op(ir, blocks, bi, pos, None, id, &extra));
        }
        return multi_target(ir, blocks, bi, pos, env, ctx, hfm);
    }
    let mut pos = pos;
    let mut offset = r.offset.ok_or("storage offset")?;
    // QueryHasUnalignedStorageBuffer is false for the job accessor
    if r.const_offset != 0 {
        let new_offset = ir.local();
        let c = ir.const_(r.const_offset);
        let add = ir.new_op(Inst::Add, StorageKind::None, Some(new_offset), &[offset, c]);
        blocks[bi].ops.insert(pos, add);
        pos += 1;
        offset = new_offset;
    }
    let (inst, kind) = (ir.op(node).inst, ir.op(node).storage_kind);
    let can_inline = kind == StorageKind::GlobalMemory; // the target language is SPIR-V
    if !can_inline {
        let targets = [gts_fn::pack_cb(r.sb_cb_slot, r.sb_cb_offset)];
        let id = match gts_fn::find_function(&ctx.entries, inst, kind, false, &targets) {
            Some(id) => id,
            None => {
                let Some(f) =
                    gts_fn::single_target(ir, env, inst, kind, r.sb_cb_slot, r.sb_cb_offset)?
                else {
                    return Err(
                        "unported: failed to reserve a storage buffer for a global memory operation"
                            .into(),
                    );
                };
                let id = hfm.add_function(f);
                ctx.entries
                    .push(Entry::new(id, inst, kind, false, targets.to_vec()));
                id
            }
        };
        return Ok(call_storage_op(ir, blocks, bi, pos, Some(offset), id, &[]));
    }
    let is_store = inst == Inst::Store || gts_fn::is_atomic(Inst(inst.0 & Inst::MASK));
    let Some(binding) =
        env.rm
            .try_get_storage_buffer_binding(env.acc, r.sb_cb_slot, r.sb_cb_offset, is_store)
    else {
        return Err(
            "unported: failed to reserve a storage buffer for a global memory operation".into(),
        );
    };
    let word_offset = ir.local();
    let nsrc = ir.op(node).sources.len();
    let c_binding = ir.const_(binding);
    let c0 = ir.const_(0);
    let sources: Vec<Opd> = if inst == Inst::AtomicCompareAndSwap {
        vec![
            c_binding,
            c0,
            word_offset,
            s(ir, node, nsrc - 2)?,
            s(ir, node, nsrc - 1)?,
        ]
    } else if is_store {
        vec![c_binding, c0, word_offset, s(ir, node, nsrc - 1)?]
    } else {
        vec![c_binding, c0, word_offset]
    };
    let c2 = ir.const_(2);
    let shift = ir.new_op(
        Inst::ShiftRightU32,
        StorageKind::None,
        Some(word_offset),
        &[offset, c2],
    );
    let dest = ir.op(node).dests.first().copied().flatten();
    let storage = ir.new_op(inst, StorageKind::StorageBuffer, dest, &sources);
    blocks[bi].ops.insert(pos, shift);
    blocks[bi].ops.insert(pos + 1, storage);
    utils::delete_node(ir, &mut blocks[bi], pos + 2);
    Ok(pos + 2)
}

/// `TryGenerateMultiTargetStorageOp` and `GenerateCallStorageOp` without an offset.
fn multi_target(
    ir: &mut Ir,
    blocks: &mut [Block],
    bi: usize,
    pos: usize,
    env: &mut Env,
    ctx: &mut Gts,
    hfm: &mut Hfm,
) -> R<usize> {
    let node = blocks[bi].ops[pos];
    let (inst, kind) = (ir.op(node).inst, ir.op(node).storage_kind);
    let targets = trace_targets(ir, blocks, bi, node, ctx)?;
    if targets.is_empty() {
        let m = format!(
            "Global memory operation \"{}\" traced to no storage buffer: it goes through the memory pool table.",
            gts_fn::inst_name(inst)?
        );
        env.acc.log(&m);
    }
    let id = match gts_fn::find_function(&ctx.entries, inst, kind, true, &targets) {
        Some(id) => id,
        None => {
            let returns = !ir.op(node).dests.is_empty();
            let made = if targets.is_empty() {
                super::gts_raw::raw_target(ir, env, inst, kind, returns)?
            } else {
                gts_fn::multi_target(ir, env, inst, kind, returns, &targets)?
            };
            let Some(f) = made else {
                return Err(
                    "unported: failed to reserve a storage buffer for a global memory operation"
                        .into(),
                );
            };
            let id = hfm.add_function(f);
            ctx.entries.push(Entry::new(id, inst, kind, true, targets));
            id
        }
    };
    Ok(call_storage_op(ir, blocks, bi, pos, None, id, &[]))
}

/// The storage buffers a global memory operation's address may point to, sorted: none when it goes through the pool
/// table.
fn trace_targets(ir: &mut Ir, blocks: &[Block], bi: usize, node: NodeId, ctx: &Gts) -> R<Vec<u32>> {
    let mut global_address = s(ir, node, 0)?;
    if let Some(add) = ir.asg_operation(global_address)
        && ir.op(add).inst == Inst::Add
    {
        let (a, c) = (s(ir, add, 0)?, s(ir, add, 1)?);
        if ir.ty(a) == OperandType::Constant && ir.ty(c) == OperandType::LocalVariable {
            global_address = c;
        } else if ir.ty(a) == OperandType::LocalVariable && ir.ty(c) == OperandType::Constant {
            global_address = a;
        }
    }
    let phi_of = |ir: &Ir, o: Opd| ir.data(o).asg_op.filter(|n| ir.is_phi(*n));
    let mut visited: Vec<NodeId> = Vec::new();
    let mut queue: std::collections::VecDeque<NodeId> = Default::default();
    let mut targets: Vec<u32> = Vec::new();
    if let Some(phi) = phi_of(ir, global_address) {
        visited.push(phi);
        queue.push_back(phi);
    } else {
        let a0 = s(ir, node, 0)?;
        let r = find_unique_base_address_cb(ir, blocks, bi, ctx, a0, false)?;
        if r.found() {
            targets.push(gts_fn::pack_cb(r.sb_cb_slot, r.sb_cb_offset));
        }
    }
    while let Some(phi) = queue.pop_front() {
        let sources = match &ir.nodes[phi.0 as usize] {
            crate::ir::Node::Phi(p) => p.sources.clone(),
            _ => return Err("phi expected".into()),
        };
        for (block, source) in sources {
            let r = find_unique_base_address_cb(ir, blocks, block as usize, ctx, source, false)?;
            if r.found() {
                let t = gts_fn::pack_cb(r.sb_cb_slot, r.sb_cb_offset);
                if !targets.contains(&t) {
                    targets.push(t);
                }
            } else if let Some(p2) = phi_of(ir, source)
                && !visited.contains(&p2)
            {
                visited.push(p2);
                queue.push_back(p2);
            }
        }
    }
    targets.sort_unstable();
    Ok(targets)
}

/// `GenerateCallStorageOp`: returns the index to continue from.
fn call_storage_op(
    ir: &mut Ir,
    blocks: &mut [Block],
    bi: usize,
    pos: usize,
    offset: Option<Opd>,
    function_id: i32,
    extra: &[Opd],
) -> usize {
    let node = blocks[bi].ops[pos];
    let ops: Vec<Opd> = ir.op(node).sources.iter().flatten().copied().collect();
    let mut sources = vec![ir.const_(function_id)];
    match offset {
        Some(o) => {
            sources.push(o);
            sources.extend_from_slice(&ops[2..]);
        }
        None => sources.extend_from_slice(&ops),
    }
    sources.extend_from_slice(extra);
    let returns = ir.op(node).dests.first().copied().flatten().is_some();
    let rv = returns.then(|| ir.local());
    let call = ir.new_op(Inst::Call, StorageKind::None, rv, &sources);
    blocks[bi].ops.insert(pos, call);
    if let Some(rv) = rv {
        ir.turn_into_copy(node, rv);
        pos + 2
    } else {
        utils::delete_node(ir, &mut blocks[bi], pos + 1);
        pos + 1
    }
}

fn get_base_address_cb_with_offset(ir: &Ir, base: Opd, offset: Opd, const_offset: i32) -> Search {
    if ir.ty(base) == OperandType::ConstantBuffer {
        let (slot, off) = (ir.cbuf_slot(base), ir.cbuf_offset(base));
        if off & 1 == 0 {
            return Search {
                sb_cb_slot: slot,
                sb_cb_offset: off,
                offset: Some(offset),
                const_offset,
            };
        }
    }
    Search::not_found()
}

fn find_from_memory(
    ir: &Ir,
    ctx: &Gts,
    op: Option<NodeId>,
    const_offset: i32,
    needs_offset: bool,
) -> R<Search> {
    if let Some(op) = op
        && let Some((ty, bo, co)) = try_get_memory_offsets(ir, op)?
        && let Some(r) = ctx.try_get_memory_target_cb(ty, bo, co)
        && (r.offset.is_some() || !needs_offset)
    {
        if const_offset != 0 {
            return Ok(Search {
                const_offset: r.const_offset + const_offset,
                ..r
            });
        }
        return Ok(r);
    }
    Ok(Search::not_found())
}

fn find_unique_base_address_cb(
    ir: &mut Ir,
    blocks: &[Block],
    bi: usize,
    ctx: &Gts,
    global_address: Opd,
    needs_offset: bool,
) -> R<Search> {
    let global_address = utils::find_last_operation(ir, blocks, global_address, bi, true)?;
    if ir.ty(global_address) == OperandType::ConstantBuffer {
        let z = ir.const_(0);
        return Ok(get_base_address_cb_with_offset(ir, global_address, z, 0));
    }
    let mut operation = ir.asg_operation(global_address);
    let Some(op) = operation.filter(|o| ir.op(*o).inst == Inst::Add) else {
        return find_from_memory(ir, ctx, operation, 0, needs_offset);
    };
    let (mut src1, mut src2) = (s(ir, op, 0)?, s(ir, op, 1)?);
    let mut const_offset = 0;
    let (t1, t2) = (ir.ty(src1), ir.ty(src2));
    if (t1 == OperandType::LocalVariable && t2 == OperandType::Constant)
        || (t2 == OperandType::LocalVariable && t1 == OperandType::Constant)
    {
        let (base_addr, offset) = if t1 == OperandType::LocalVariable {
            (
                utils::find_last_operation(ir, blocks, src1, bi, true)?,
                src2,
            )
        } else {
            (
                utils::find_last_operation(ir, blocks, src2, bi, true)?,
                src1,
            )
        };
        let r = get_base_address_cb_with_offset(ir, base_addr, offset, 0);
        if r.found() {
            return Ok(r);
        }
        const_offset = ir.value(offset);
        operation = ir.asg_operation(base_addr);
        match operation.filter(|o| ir.op(*o).inst == Inst::Add) {
            None => return find_from_memory(ir, ctx, operation, const_offset, needs_offset),
            Some(o) => {
                src1 = s(ir, o, 0)?;
                src2 = s(ir, o, 1)?;
            }
        }
    }
    let (c1, c2) = (
        ir.ty(src1) == OperandType::ConstantBuffer,
        ir.ty(src2) == OperandType::ConstantBuffer,
    );
    if c1
        && c2
        && ir.cbuf_slot(src1) == DRIVER_RESERVED_CB
        && ir.cbuf_slot(src2) != DRIVER_RESERVED_CB
    {
        return Ok(get_base_address_cb_with_offset(
            ir,
            src1,
            src2,
            const_offset,
        ));
    }
    if !c1 || (c1 && c2) || (c2 && ir.cbuf_slot(src2) == DRIVER_RESERVED_CB) {
        return Ok(get_base_address_cb_with_offset(
            ir,
            src2,
            src1,
            const_offset,
        ));
    }
    Ok(get_base_address_cb_with_offset(
        ir,
        src1,
        src2,
        const_offset,
    ))
}

fn try_get_memory_offsets(ir: &Ir, op: NodeId) -> R<Option<(LsType, Option<Opd>, i32)>> {
    let o = ir.op(op);
    if o.inst == Inst::Load || o.inst == Inst::Store {
        if o.storage_kind == StorageKind::SharedMemory {
            return Ok(shared_offsets(ir, op)?.map(|(b, c)| (LsType::Shared, Some(b), c)));
        } else if o.storage_kind == StorageKind::LocalMemory {
            let off = s(ir, op, 1)?;
            if ir.ty(off) == OperandType::Constant {
                return Ok(Some((LsType::Local, None, ir.value(off))));
            }
            return Ok(None);
        }
    }
    Ok(None)
}

fn shared_offsets(ir: &Ir, op: NodeId) -> R<Option<(Opd, i32)>> {
    let mut base: Option<Opd> = None;
    let mut const_offset = 0;
    let src1 = s(ir, op, 1)?;
    if let Some(sr) = ir.asg_operation(src1)
        && ir.op(sr).inst == Inst::ShiftRightU32
    {
        let c = s(ir, sr, 1)?;
        if ir.ty(c) == OperandType::Constant && ir.value(c) == 2 {
            base = Some(s(ir, sr, 0)?);
        }
    }
    if let Some(b) = base
        && let Some(add) = ir.asg_operation(b)
        && ir.op(add).inst == Inst::Add
    {
        let (a, c) = (s(ir, add, 0)?, s(ir, add, 1)?);
        if ir.ty(a) == OperandType::Constant && ir.ty(c) == OperandType::LocalVariable {
            const_offset = ir.value(a);
            base = Some(c);
        } else if ir.ty(a) == OperandType::LocalVariable && ir.ty(c) == OperandType::Constant {
            base = Some(a);
            const_offset = ir.value(c);
        }
    }
    Ok(match base {
        Some(b) if ir.ty(b) == OperandType::LocalVariable => Some((b, const_offset)),
        _ => None,
    })
}

/// The members of a group lie this far from the first member's offset at most, in bytes, so that the range one
/// resolve covers stays a few kilobytes.
const GROUP_WINDOW: u32 = 2048;
/// The largest offset a group adds to its base: the sum of the offsets stays a positive 32 bit number.
const GROUP_MAX_OFFSET: u32 = 1 << 30;

/// The operand a chain of copies leads to.
fn canon(ir: &Ir, mut o: Opd) -> Opd {
    while let Some(n) = ir.asg_operation(o)
        && ir.op(n).inst == Inst::Copy
        && let Some(Some(src)) = ir.op(n).sources.first().copied()
        && ir.ty(src) == OperandType::LocalVariable
    {
        o = src;
    }
    o
}

/// The value of a constant operand; an undefined one is the zero register.
fn constant_of(ir: &Ir, o: Opd) -> Option<i32> {
    match ir.ty(o) {
        OperandType::Constant => Some(ir.value(o)),
        OperandType::Undefined => Some(0),
        _ => None,
    }
}

/// The constant source of `Add` and the other one.
fn add_with_constant(ir: &Ir, n: NodeId) -> Option<(Opd, u32)> {
    let o = ir.op(n);
    if o.inst != Inst::Add || o.sources.len() != 2 {
        return None;
    }
    let (a, b) = (o.sources[0]?, o.sources[1]?);
    let (x, k) = match (constant_of(ir, a), constant_of(ir, b)) {
        (None, Some(k)) => (a, k),
        (Some(k), None) => (b, k),
        _ => return None,
    };
    let k = u32::try_from(k).ok().filter(|k| *k < GROUP_MAX_OFFSET)?;
    Some((x, k))
}

/// `{x, y} + k` of the 64 bit address `{lo, hi}`: the low word is `x + k` and the high word `y` plus the carry out of
/// that sum, as the front end writes a 64 bit add of a constant.
fn base_plus_constant(ir: &Ir, lo: Opd, hi: Opd) -> Option<(Opd, Opd, u32)> {
    let (x, k) = add_with_constant(ir, ir.asg_operation(lo)?)?;
    let hadd = ir.asg_operation(hi)?;
    let o = ir.op(hadd);
    if o.inst != Inst::Add || o.sources.len() != 2 {
        return None;
    }
    let (p, q) = (o.sources[0]?, o.sources[1]?);
    let is_carry = |c: Opd| -> bool {
        let Some(sel) = ir.asg_operation(canon(ir, c)) else {
            return false;
        };
        let so = ir.op(sel);
        if so.inst != Inst::ConditionalSelect || so.sources.len() != 3 {
            return false;
        }
        let (Some(cond), Some(one), Some(zero)) = (so.sources[0], so.sources[1], so.sources[2])
        else {
            return false;
        };
        if ir.ty(one) != OperandType::Constant
            || ir.value(one) != 1
            || ir.ty(zero) != OperandType::Constant
            || ir.value(zero) != 0
        {
            return false;
        }
        let Some(cmp) = ir.asg_operation(canon(ir, cond)) else {
            return false;
        };
        let co = ir.op(cmp);
        // The sum is below an addend exactly when the add carried
        co.inst == Inst::CompareLessU32
            && co.sources.len() == 2
            && co.sources[0].map(|a| canon(ir, a)) == Some(canon(ir, lo))
            && co.sources[1].is_some_and(|b| {
                let b = canon(ir, b);
                b == canon(ir, x) || constant_of(ir, b).is_some_and(|c| c as u32 == k)
            })
    };
    let y = if is_carry(q) {
        p
    } else if is_carry(p) {
        q
    } else {
        return None;
    };
    Some((x, y, k))
}

/// The base of the 64 bit address `{lo, hi}` and the constant offset from it: the address is `base + offset`, however
/// many constant adds make it.
fn decompose(ir: &Ir, lo: Opd, hi: Opd) -> (Opd, Opd, u32) {
    let (mut lo, mut hi, mut k) = (canon(ir, lo), canon(ir, hi), 0u32);
    for _ in 0..8 {
        let Some((x, y, c)) = base_plus_constant(ir, lo, hi) else {
            break;
        };
        let Some(sum) = k.checked_add(c).filter(|s| *s < GROUP_MAX_OFFSET) else {
            break;
        };
        (lo, hi, k) = (canon(ir, x), canon(ir, y), sum);
    }
    (lo, hi, k)
}

/// The immediate dominator of each block ("A Simple, Fast Dominance Algorithm"); block 0 is the entry and its own
/// dominator, and a block it does not reach has none.
fn immediate_dominators(blocks: &[Block]) -> Vec<Option<usize>> {
    let n = blocks.len();
    let mut idom: Vec<Option<usize>> = vec![None; n];
    if n == 0 {
        return idom;
    }
    // Post order of the depth first search from the entry
    let mut post: Vec<usize> = Vec::new();
    let mut seen = vec![false; n];
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    seen[0] = true;
    while let Some((b, k)) = stack.pop() {
        let succ = [blocks[b].next, blocks[b].branch];
        if k < 2 {
            stack.push((b, k + 1));
            if let Some(c) = succ[k]
                && c < n
                && !seen[c]
            {
                seen[c] = true;
                stack.push((c, 0));
            }
        } else {
            post.push(b);
        }
    }
    let mut number = vec![usize::MAX; n];
    for (i, b) in post.iter().enumerate() {
        number[*b] = i;
    }
    idom[0] = Some(0);
    loop {
        let mut changed = false;
        for &b in post.iter().rev().skip(1) {
            let mut new: Option<usize> = None;
            for &p in &blocks[b].preds {
                if p >= n || idom[p].is_none() || number[p] == usize::MAX {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(mut x) => {
                        let mut y = p;
                        while x != y {
                            while number[x] < number[y] {
                                x = idom[x].unwrap_or(0);
                            }
                            while number[y] < number[x] {
                                y = idom[y].unwrap_or(0);
                            }
                        }
                        x
                    }
                });
            }
            if idom[b] != new {
                idom[b] = new;
                changed = true;
            }
        }
        if !changed {
            return idom;
        }
    }
}

/// Block `a` dominates block `b`.
fn dominates(idom: &[Option<usize>], a: usize, b: usize) -> bool {
    let mut cur = b;
    loop {
        if cur == a {
            return true;
        }
        match idom.get(cur).copied().flatten() {
            Some(next) if next != cur => cur = next,
            _ => return false,
        }
    }
}

/// The two operands are one operand, or constants of one value.
fn same_operand(ir: &Ir, a: Opd, b: Opd) -> bool {
    a == b || (constant_of(ir, a).is_some() && constant_of(ir, a) == constant_of(ir, b))
}

/// The raw word loads of one block that read near one 64 bit base: one resolve of the range they span finds the pool
/// table entry that holds all of it, and each load adds that entry's delta to its address, with the helper that
/// searches the table for the loads that find no such entry. Returns whether a group was made, with the resolve put
/// in before the load at `pos`.
fn plan_group(
    ir: &mut Ir,
    blocks: &mut [Block],
    bi: usize,
    pos: usize,
    env: &mut Env,
    ctx: &mut Gts,
    hfm: &mut Hfm,
) -> R<bool> {
    let first = blocks[bi].ops[pos];
    let (lo, hi) = (s(ir, first, 0)?, s(ir, first, 1)?);
    if ir.op(first).sources.len() != 2 {
        return Ok(false);
    }
    let (base_lo, base_hi, k0) = decompose(ir, lo, hi);
    if matches!(
        ir.ty(base_lo),
        OperandType::Constant | OperandType::Undefined
    ) {
        return Ok(false);
    }
    if !trace_targets(ir, blocks, bi, first, ctx)?.is_empty() {
        return Ok(false);
    }
    let idom = ctx
        .idom
        .get_or_insert_with(|| immediate_dominators(blocks))
        .clone();
    let reachable = |cb: usize| cb == 0 || idom.get(cb).copied().flatten().is_some();
    // Every raw word load of the function that reads near the same base, in the blocks the entry reaches
    let mut members = vec![(first, k0, bi)];
    for cb in 0..blocks.len() {
        if !reachable(cb) {
            continue;
        }
        for j in 0..blocks[cb].ops.len() {
            let n = blocks[cb].ops[j];
            if n == first || !ir.is_op(n) {
                continue;
            }
            let o = ir.op(n);
            if o.inst != Inst::Load
                || o.storage_kind != StorageKind::GlobalMemory
                || o.sources.len() != 2
                || ctx.members.contains_key(&n)
            {
                continue;
            }
            let (l, h) = (s(ir, n, 0)?, s(ir, n, 1)?);
            let (bl, bh, k) = decompose(ir, l, h);
            if !same_operand(ir, bl, base_lo)
                || !same_operand(ir, bh, base_hi)
                || k.abs_diff(k0) > GROUP_WINDOW
            {
                continue;
            }
            if trace_targets(ir, blocks, cb, n, ctx)?.is_empty() {
                members.push((n, k, cb));
            }
        }
    }
    if members.len() < 2 {
        return Ok(false);
    }
    // The resolve goes into the block that dominates all of them, before the first of them in it, or before its
    // branch. The base is computed by then.
    let mut at_block = bi;
    for (_, _, cb) in &members {
        while !dominates(&idom, at_block, *cb) {
            at_block = idom[at_block].ok_or("block without dominator")?;
        }
    }
    for o in [base_lo, base_hi] {
        if ir.ty(o) != OperandType::LocalVariable {
            continue;
        }
        let Some(def) = ir.data(o).asg_op else {
            return Ok(false);
        };
        let Some(def_block) = (0..blocks.len()).find(|cb| blocks[*cb].ops.contains(&def)) else {
            return Ok(false);
        };
        if !dominates(&idom, def_block, at_block) {
            return Ok(false);
        }
    }
    let ends = blocks[at_block].ops.len()
        - usize::from(blocks[at_block].ops.last().is_some_and(|n| {
            ir.is_op(*n)
                && matches!(
                    ir.op(*n).inst,
                    Inst::Branch
                        | Inst::BranchIfTrue
                        | Inst::BranchIfFalse
                        | Inst::Discard
                        | Inst::Return
                )
        }));
    let put_at = members
        .iter()
        .filter(|m| m.2 == at_block)
        .filter_map(|m| blocks[at_block].ops.iter().position(|n| *n == m.0))
        .min()
        .map_or(ends, |p| p.min(ends));
    let members: Vec<(NodeId, u32)> = members.into_iter().map(|m| (m.0, m.1)).collect();
    let Some(map_binding) =
        env.rm
            .try_get_storage_buffer_binding(env.acc, super::gts_raw::GMEM_MAP_CB_SLOT, 0, false)
    else {
        return Ok(false);
    };
    let (Some(min_k), Some(max_k)) = (
        members.iter().map(|m| m.1).min(),
        members.iter().map(|m| m.1).max(),
    ) else {
        return Ok(false);
    };
    let span = max_k - min_k + 4;
    let range_id = match ctx.range_fn {
        Some(id) => id,
        None => {
            let Some(f) = super::gts_raw::raw_range(ir, env)? else {
                return Ok(false);
            };
            let id = hfm.add_function(f);
            ctx.range_fn = Some(id);
            id
        }
    };
    if ctx.load_fn.is_none() {
        let Some(f) = super::gts_raw::raw_load_in_range(ir, env)? else {
            return Ok(false);
        };
        ctx.load_fn = Some(hfm.add_function(f));
    }
    let mut ops: Vec<NodeId> = Vec::new();
    let mut add = |ir: &mut Ir, inst: Inst, kind: StorageKind, src: &[Opd]| -> Opd {
        let d = ir.local();
        let n = ir.new_op(inst, kind, Some(d), src);
        ops.push(n);
        d
    };
    // A constant base word, or the zero register, is a constant argument
    let arg = |ir: &mut Ir, o: Opd| match constant_of(ir, o) {
        Some(v) => ir.const_(v),
        None => o,
    };
    let base_lo = arg(ir, base_lo);
    let base_hi = arg(ir, base_hi);
    let c_fn = ir.const_(range_id);
    let (c_min, c_span) = (ir.const_(min_k as i32), ir.const_(span as i32));
    let entry = add(
        ir,
        Inst::Call,
        StorageKind::None,
        &[c_fn, base_lo, base_hi, c_min, c_span],
    );
    let c_words = ir.const_(super::gts_raw::GMEM_MAP_ENTRY_WORDS);
    let at = add(ir, Inst::Multiply, StorageKind::None, &[entry, c_words]);
    let mut table_word = |ir: &mut Ir, word: i32| -> Opd {
        let c_w = ir.const_(word);
        let idx = add(ir, Inst::Add, StorageKind::None, &[at, c_w]);
        let (c_b, c0) = (ir.const_(map_binding), ir.const_(0));
        add(ir, Inst::Load, StorageKind::StorageBuffer, &[c_b, c0, idx])
    };
    let delta_low = table_word(ir, 4);
    let delta_high = table_word(ir, 5);
    let resolved = Resolved {
        entry,
        delta_low,
        delta_high,
    };
    for (i, op) in ops.into_iter().enumerate() {
        blocks[at_block].ops.insert(put_at + i, op);
    }
    for (n, _) in members {
        ctx.members.insert(n, resolved);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `{x, y} + k` the way the front end writes it: the low word `x + k`, the high word `y` plus 1 where the sum is
    /// below `x`.
    fn plus(ir: &mut Ir, x: Opd, y: Opd, k: i32, compare_with: Option<Opd>) -> (Opd, Opd) {
        let (lo, ck) = (ir.local(), ir.const_(k));
        ir.new_op(Inst::Add, StorageKind::None, Some(lo), &[x, ck]);
        let (wrapped, one, zero, carry, hi) = (
            ir.local(),
            ir.const_(1),
            ir.const_(0),
            ir.local(),
            ir.local(),
        );
        let against = compare_with.unwrap_or(x);
        ir.new_op(
            Inst::CompareLessU32,
            StorageKind::None,
            Some(wrapped),
            &[lo, against],
        );
        ir.new_op(
            Inst::ConditionalSelect,
            StorageKind::None,
            Some(carry),
            &[wrapped, one, zero],
        );
        ir.new_op(Inst::Add, StorageKind::None, Some(hi), &[y, carry]);
        (lo, hi)
    }

    #[test]
    fn a_constant_add_with_its_carry_is_the_base_plus_the_constant() {
        let mut ir = Ir::default();
        let (x, y) = (ir.local(), ir.local());
        let (lo, hi) = plus(&mut ir, x, y, 20, None);
        assert_eq!(decompose(&ir, lo, hi), (x, y, 20));
        // The sum of two adds: the base stays the first one
        let (lo2, hi2) = plus(&mut ir, lo, hi, 8, None);
        assert_eq!(decompose(&ir, lo2, hi2), (x, y, 28));
    }

    #[test]
    fn an_address_without_the_carry_is_its_own_base() {
        let mut ir = Ir::default();
        let (x, y, z) = (ir.local(), ir.local(), ir.local());
        // The carry out of a different sum does not make `x + 4` a 64 bit add
        let (lo, hi) = plus(&mut ir, x, y, 4, Some(z));
        assert_eq!(decompose(&ir, lo, hi), (lo, hi, 0));
        // A plain pair
        assert_eq!(decompose(&ir, x, y), (x, y, 0));
    }

    #[test]
    fn a_negative_constant_is_not_a_group_offset() {
        let mut ir = Ir::default();
        let (x, y) = (ir.local(), ir.local());
        let (lo, hi) = plus(&mut ir, x, y, -4, None);
        assert_eq!(decompose(&ir, lo, hi), (lo, hi, 0));
    }

    #[test]
    fn two_adds_of_one_base_meet_in_one_base() {
        let mut ir = Ir::default();
        let (x, y) = (ir.local(), ir.local());
        let (a_lo, a_hi) = plus(&mut ir, x, y, 16, None);
        let (b_lo, b_hi) = plus(&mut ir, x, y, 1000, None);
        let (a, b) = (decompose(&ir, a_lo, a_hi), decompose(&ir, b_lo, b_hi));
        assert_eq!((a.0, a.1, b.0, b.1), (x, y, x, y));
        assert_eq!((a.2, b.2), (16, 1000));
    }
}
