//! Grouping of raw word loads near one 64 bit base: base decomposition, dominators and `plan_group`.

use super::*;

/// The members of a group lie this far from the first member's offset at most, in bytes, so that the range one
/// resolve covers stays a few kilobytes.
pub(super) const GROUP_WINDOW: u32 = 2048;
/// The largest offset a group adds to its base: the sum of the offsets stays a positive 32 bit number.
pub(super) const GROUP_MAX_OFFSET: u32 = 1 << 30;

/// The operand a chain of copies leads to.
pub(super) fn canon(ir: &Ir, mut o: Opd) -> Opd {
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
pub(super) fn constant_of(ir: &Ir, o: Opd) -> Option<i32> {
    match ir.ty(o) {
        OperandType::Constant => Some(ir.value(o)),
        OperandType::Undefined => Some(0),
        _ => None,
    }
}

/// The constant source of `Add` and the other one.
pub(super) fn add_with_constant(ir: &Ir, n: NodeId) -> Option<(Opd, u32)> {
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
pub(super) fn base_plus_constant(ir: &Ir, lo: Opd, hi: Opd) -> Option<(Opd, Opd, u32)> {
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
pub(super) fn decompose(ir: &Ir, lo: Opd, hi: Opd) -> (Opd, Opd, u32) {
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
pub(super) fn immediate_dominators(blocks: &[Block]) -> Vec<Option<usize>> {
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
pub(super) fn dominates(idom: &[Option<usize>], a: usize, b: usize) -> bool {
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
pub(super) fn same_operand(ir: &Ir, a: Opd, b: Opd) -> bool {
    a == b || (constant_of(ir, a).is_some() && constant_of(ir, a) == constant_of(ir, b))
}

/// The raw word loads of one block that read near one 64 bit base: one resolve of the range they span finds the pool
/// table entry that holds all of it, and each load adds that entry's delta to its address, with the helper that
/// searches the table for the loads that find no such entry. Returns whether a group was made, with the resolve put
/// in before the load at `pos`.
pub(super) fn plan_group(
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
    let Some(map_binding) = env.rm.try_get_storage_buffer_binding(
        env.acc,
        super::super::gts_raw::GMEM_MAP_CB_SLOT,
        0,
        false,
    ) else {
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
            let Some(f) = super::super::gts_raw::raw_range(ir, env)? else {
                return Ok(false);
            };
            let id = hfm.add_function(f);
            ctx.range_fn = Some(id);
            id
        }
    };
    if ctx.load_fn.is_none() {
        let Some(f) = super::super::gts_raw::raw_load_in_range(ir, env)? else {
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
    let c_words = ir.const_(super::super::gts_raw::GMEM_MAP_ENTRY_WORDS);
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
