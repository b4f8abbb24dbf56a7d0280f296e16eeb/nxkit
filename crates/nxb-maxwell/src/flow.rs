//! Control flow discovery of the decoder: BRX jump table targets and SSY/PBK/PCNT propagation.

use super::decoder::{Block, Ctx, SyncTarget, get_block};
use super::insts::{InstBrx, InstImnmxI, InstLdc, InstShlI};
use super::names::InstName;
use super::table::{InstOp, props};
use std::collections::{HashMap, HashSet, VecDeque};

pub(super) fn find_brx_targets(
    ctx: &mut Ctx,
    all: &mut Vec<Block>,
    order: &[usize],
    work: &mut VecDeque<usize>,
    seen: &mut HashMap<u64, usize>,
) -> bool {
    let mut has_new = false;
    for &bi in order {
        let last = all[bi].last_op();
        let has_next = all[bi].has_next();
        if last.name == InstName::Brx && all[bi].succs.len() == usize::from(has_next) {
            let mut visited: HashSet<u64> = HashSet::new();
            let brx = InstBrx(last.raw);
            let base = last.absolute_address();
            let (cb_base, count) = find_brx_target_range(all, bi, brx.src_a());
            if count != 0 {
                has_new = true;
            }
            for i in 0..count {
                let target_off = ctx.read_cb1(cb_base + i * 4);
                let ta = base.wrapping_add(target_off as u64);
                if visited.insert(ta) {
                    let t = get_block(ta, all, work, seen);
                    all[t].preds.push(bi);
                    all[bi].succs.push(t);
                }
            }
        }
    }
    has_new
}

/// Location of an op: (block, index).
type Loc = Option<(usize, usize)>;

fn find_brx_target_range(all: &[Block], block: usize, brx_reg: i32) -> (i32, i32) {
    let mut visited: HashSet<usize> = HashSet::new();
    let ldc = find_first_reg_write(
        all,
        &mut visited,
        (block, all[block].ops.len() as i64),
        brx_reg,
    );
    let Some(ldc) = ldc else { return (0, 0) };
    let ldc_op = all[ldc.0].ops[ldc.1];
    if ldc_op.name != InstName::Ldc {
        return (0, 0);
    }
    let l = InstLdc(ldc_op.raw);
    if l.cbuf_slot() != 1 || l.address_mode().0 != 0 {
        return (0, 0);
    }
    let shl = find_first_reg_write(all, &mut visited, (ldc.0, ldc.1 as i64), l.src_a());
    let Some(shl) = shl else { return (0, 0) };
    if !is_imm_inst(all, shl, InstName::Shl) || InstShlI(all[shl.0].ops[shl.1].raw).imm20() != 2 {
        return (0, 0);
    }
    let imn = find_first_reg_write(
        all,
        &mut visited,
        (shl.0, shl.1 as i64),
        InstShlI(all[shl.0].ops[shl.1].raw).src_a(),
    );
    let Some(imn) = imn else { return (0, 0) };
    if !is_imm_inst(all, imn, InstName::Imnmx) {
        return (0, 0);
    }
    let m = InstImnmxI(all[imn.0].ops[imn.1].raw);
    if m.signed() || m.src_pred() != 7 || m.src_pred_inv() {
        return (0, 0);
    }
    (l.cbuf_offset(), m.imm20() + 1)
}

fn is_imm_inst(all: &[Block], l: (usize, usize), name: InstName) -> bool {
    let op = all[l.0].ops[l.1];
    op.name == name && op.has(props::IB)
}

fn find_first_reg_write(
    all: &[Block],
    visited: &mut HashSet<usize>,
    start: (usize, i64),
    reg: i32,
) -> Loc {
    let mut to_visit: VecDeque<(usize, i64)> = VecDeque::new();
    to_visit.push_back(start);
    visited.insert(start.0);
    while let Some((b, idx)) = to_visit.pop_front() {
        let mut i = idx - 1;
        while i >= 0 {
            if writes_to_register(&all[b].ops[i as usize], reg) {
                return Some((b, i as usize));
            }
            i -= 1;
        }
        for &p in &all[b].preds {
            if visited.insert(p) {
                to_visit.push_back((p, all[p].ops.len() as i64));
            }
        }
    }
    None
}

fn writes_to_register(op: &InstOp, reg: i32) -> bool {
    if op.props & (props::RD | props::RD2) == 0 {
        return false;
    }
    if op.has(props::RD2) && ((op.raw >> 28) as u8) as i32 == reg {
        return true;
    }
    (op.raw as u8) as i32 == reg
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Merge {
    Brk,
    Cont,
    Sync,
}

#[derive(Clone, Copy)]
enum Pbs {
    Block(usize),
    PopPush(usize),
    PushBranch(u64, Merge),
}

fn merge_of_push(n: InstName) -> Merge {
    match n {
        InstName::Pbk => Merge::Brk,
        InstName::Pcnt => Merge::Cont,
        _ => Merge::Sync,
    }
}

fn merge_of_pop(n: InstName) -> Merge {
    match n {
        InstName::Brk => Merge::Brk,
        InstName::Cont => Merge::Cont,
        _ => Merge::Sync,
    }
}

fn is_pop_branch(n: InstName) -> bool {
    matches!(n, InstName::Brk | InstName::Cont | InstName::Sync)
}

pub(super) fn propagate_push_op(
    all: &mut [Block],
    seen: &HashMap<u64, usize>,
    curr: usize,
    push_index: usize,
    tags: &mut u32,
) {
    let info_op = all[curr].push_ops[push_index].op;
    let target = seen[&info_op.absolute_address()];
    let mut work: Vec<Pbs> = Vec::new();
    let mut visited: HashSet<usize> = HashSet::new();
    let mut stack: Vec<(u64, Merge)> = Vec::new();
    let mut push_index = push_index;
    fn push(work: &mut Vec<Pbs>, visited: &HashSet<usize>, p: Pbs) {
        match p {
            Pbs::Block(b) if visited.contains(&b) => {}
            _ => work.push(p),
        }
    }
    push(&mut work, &visited, Pbs::Block(curr));
    while let Some(p) = work.pop() {
        let current = match p {
            Pbs::PushBranch(a, m) => {
                stack.push((a, m));
                continue;
            }
            Pbs::PopPush(n) => {
                while stack.len() > n {
                    stack.pop();
                }
                continue;
            }
            Pbs::Block(b) => b,
        };
        if !visited.insert(current) {
            continue;
        }
        let count = all[current].push_ops.len();
        if count != 0 {
            push(&mut work, &visited, Pbs::PopPush(stack.len()));
            for p in &all[current].push_ops[push_index.min(count)..] {
                let o = p.op;
                stack.push((o.absolute_address(), merge_of_push(o.name)));
            }
        }
        push_index = 0;
        let has_next = all[current].has_next();
        if has_next {
            let s0 = all[current].succs[0];
            push(&mut work, &visited, Pbs::Block(s0));
        }
        let last = all[current].last_op();
        if is_pop_branch(last.name) {
            let pop_merge = merge_of_pop(last.name);
            let mut found = true;
            let mut target_address = 0u64;
            loop {
                let Some((a, m)) = stack.pop() else {
                    found = false;
                    break;
                };
                target_address = a;
                push(&mut work, &visited, Pbs::PushBranch(a, m));
                if m == pop_merge {
                    break;
                }
            }
            if found {
                if stack.is_empty() {
                    let st = SyncTarget {
                        push: (curr, push_index_of(all, curr, info_op.address)),
                        push_op_id: all[current].sync_targets.len(),
                    };
                    if !all[current]
                        .sync_targets
                        .iter()
                        .any(|(k, _)| *k == info_op.address)
                    {
                        all[current].sync_targets.push((info_op.address, st));
                        let pi = st.push.1;
                        *tags += 1;
                        all[curr].push_ops[pi].consumers.push((current, *tags));
                        all[target].preds.push(current);
                        all[current].succs.push(target);
                    }
                } else {
                    push(&mut work, &visited, Pbs::Block(seen[&target_address]));
                }
            }
        } else {
            let mut succs: Vec<usize> = all[current].succs.clone();
            succs.sort_by(|a, b| all[*b].address.cmp(&all[*a].address));
            let first = all[current].succs.first().copied();
            for t in succs {
                if !has_next || Some(t) != first {
                    push(&mut work, &visited, Pbs::Block(t));
                }
            }
        }
    }
}

fn push_index_of(all: &[Block], block: usize, address: u64) -> usize {
    all[block]
        .push_ops
        .iter()
        .position(|p| p.op.address == address)
        .unwrap_or(0)
}
