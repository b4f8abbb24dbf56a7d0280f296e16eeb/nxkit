//! `Decoder.Decode`: machine code to functions and basic blocks.

use super::enums::Ccc;
use super::flow::{find_brx_targets, propagate_push_op};
use super::insts::{InstAld, InstAst, InstConditional, InstIpa};
use super::names::InstName;
use super::table::{InstOp, get_op, props};
use crate::attr::{AttributeUsage, Stage, consts as ac, feature};
use std::collections::{HashMap, VecDeque};

/// A SSY, PBK or PCNT with the blocks that consume its address.
#[derive(Clone, Debug)]
pub struct PushOpInfo {
    pub op: InstOp,
    /// block index to the tag of the local the emitter allocates
    pub consumers: Vec<(usize, u32)>,
}

/// A sync target of a block.
#[derive(Clone, Copy, Debug)]
pub struct SyncTarget {
    /// (block, push op index) of the owner
    pub push: (usize, usize),
    pub push_op_id: usize,
}

/// A basic block (index into [`DecodedFunction::blocks`] order after sorting).
#[derive(Clone, Debug)]
pub struct Block {
    pub address: u64,
    pub end_address: u64,
    pub preds: Vec<usize>,
    pub succs: Vec<usize>,
    pub ops: Vec<InstOp>,
    pub push_ops: Vec<PushOpInfo>,
    pub sync_targets: Vec<(u64, SyncTarget)>,
}

impl Block {
    pub(crate) fn new(address: u64) -> Block {
        Block {
            address,
            end_address: 0,
            preds: vec![],
            succs: vec![],
            ops: vec![],
            push_ops: vec![],
            sync_targets: vec![],
        }
    }

    pub fn last_op(&self) -> InstOp {
        self.ops.last().copied().unwrap_or(InstOp::none())
    }

    pub fn has_next(&self) -> bool {
        !self.ops.is_empty() && !is_unconditional_branch(&self.last_op())
    }
}

/// One function: blocks sorted by address. Indices in `preds` and `succs` refer to `blocks`.
#[derive(Clone, Debug)]
pub struct DecodedFunction {
    pub address: u64,
    pub blocks: Vec<Block>,
}

/// The decoded program.
#[derive(Clone, Debug)]
pub struct DecodedProgram {
    pub functions: Vec<DecodedFunction>,
    pub attribute_usage: AttributeUsage,
    pub used_features: i32,
    pub clip_distances_written: u8,
    pub cb1_data_size: i32,
    pub size: u64,
}

/// What the decoder records into the shader definitions (indexing of outputs and inputs).
#[derive(Clone, Copy, Debug, Default)]
pub struct IndexingUse {
    pub input_indexing: bool,
    pub output_indexing: bool,
}

/// What the decoder reads from the host: word reads of constant buffer 1 (jump tables of BRX).
pub trait Cb1Reader {
    fn constant_buffer1_read(&mut self, offset: i32) -> u32;
    /// `QueryConstantBuffer1Size`
    fn constant_buffer1_size(&self) -> i32 {
        0
    }
}

pub(crate) struct Ctx<'a> {
    pub(crate) attr: AttributeUsage,
    pub(crate) features: i32,
    pub(crate) clip: u8,
    pub(crate) cb1: i32,
    pub(crate) acc: &'a mut dyn Cb1Reader,
    pub(crate) indexing: IndexingUse,
}

impl Ctx<'_> {
    pub(super) fn read_cb1(&mut self, offset: i32) -> u32 {
        if self.cb1 < offset + 4 {
            self.cb1 = offset + 4;
        }
        self.acc.constant_buffer1_read(offset)
    }
}

pub fn is_unconditional_branch(op: &InstOp) -> bool {
    if op.ampere {
        return op.has(props::BRA)
            && super::ampere_decode::is_unconditional(&super::ampere::AmpOp::of(op));
    }
    is_unconditional(op) && op.has(props::BRA)
}

fn is_unconditional(op: &InstOp) -> bool {
    let c = InstConditional(op.raw);
    if (op.name == InstName::Bra || op.name == InstName::Exit) && c.ccc() != Ccc::T {
        return false;
    }
    c.pred() == 7 && !c.pred_inv()
}

/// Decode from `start` (the byte offset of the first instruction in `code`).
pub fn decode(
    stage: Stage,
    acc: &mut dyn Cb1Reader,
    code: &[u8],
    start: u64,
) -> Result<(DecodedProgram, IndexingUse), String> {
    let mut ctx = Ctx {
        attr: AttributeUsage::default(),
        features: 0,
        clip: 0,
        cb1: 0,
        acc,
        indexing: IndexingUse::default(),
    };
    let mut funcs: Vec<DecodedFunction> = Vec::new();
    let mut visited_fn: HashMap<u64, usize> = HashMap::new();
    let mut queue: VecDeque<usize> = VecDeque::new();
    let mut enqueue =
        |addr: u64, funcs: &mut Vec<DecodedFunction>, queue: &mut VecDeque<usize>| -> usize {
            if let Some(&i) = visited_fn.get(&addr) {
                return i;
            }
            funcs.push(DecodedFunction {
                address: addr,
                blocks: vec![],
            });
            visited_fn.insert(addr, funcs.len() - 1);
            queue.push_back(funcs.len() - 1);
            funcs.len() - 1
        };
    enqueue(0, &mut funcs, &mut queue);
    while let Some(fi) = queue.pop_front() {
        let fa = funcs[fi].address;
        let blocks = decode_function(stage, &mut ctx, code, start, fa, &mut |a| {
            enqueue(a, &mut funcs, &mut queue)
        })?;
        funcs[fi].blocks = blocks;
    }
    let max_end = funcs
        .iter()
        .flat_map(|f| f.blocks.iter().map(|b| b.end_address))
        .max()
        .unwrap_or(0);
    Ok((
        DecodedProgram {
            functions: funcs,
            attribute_usage: ctx.attr,
            used_features: ctx.features,
            clip_distances_written: ctx.clip,
            cb1_data_size: ctx.cb1,
            size: max_end + start,
        },
        ctx.indexing,
    ))
}

pub(crate) fn get_block(
    a: u64,
    all: &mut Vec<Block>,
    work: &mut VecDeque<usize>,
    seen: &mut HashMap<u64, usize>,
) -> usize {
    if let Some(&i) = seen.get(&a) {
        return i;
    }
    all.push(Block::new(a));
    work.push_back(all.len() - 1);
    seen.insert(a, all.len() - 1);
    all.len() - 1
}

fn decode_function(
    stage: Stage,
    ctx: &mut Ctx,
    code: &[u8],
    start: u64,
    fn_addr: u64,
    enqueue: &mut dyn FnMut(u64) -> usize,
) -> Result<Vec<Block>, String> {
    // Blocks are created in an arena (`all`) and sorted by address in `order`, indices in preds and succs are arena indices
    // until the end, where they are remapped to positions in the sorted list.
    let mut all: Vec<Block> = Vec::new();
    let mut order: Vec<usize> = Vec::new();
    let mut work: VecDeque<usize> = VecDeque::new();
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let mut tags = 0u32;
    get_block(fn_addr, &mut all, &mut work, &mut seen);
    loop {
        while let Some(cur) = work.pop_front() {
            let sorted: Vec<&Block> = order.iter().map(|&i| &all[i]).collect();
            let (found, nidx) = bsearch_refs(&sorted, all[cur].address);
            if found {
                let n = order[nidx];
                if all[n].address == all[cur].address {
                    return Err("Found duplicate block address on the list.".into());
                }
                split(&mut all, n, cur)?;
                order.insert(nidx + 1, cur);
                continue;
            }
            let mut limit = u64::MAX;
            if nidx != order.len() {
                let n = &all[order[nidx]];
                let next = nidx + 1;
                if n.address < all[cur].address && next < order.len() {
                    limit = all[order[next]].address;
                } else if n.address > all[cur].address {
                    limit = n.address;
                }
            }
            fill_block(stage, ctx, code, start, &mut all[cur], limit)?;
            if !all[cur].ops.is_empty() {
                let pushes: Vec<u64> = all[cur]
                    .push_ops
                    .iter()
                    .map(|p| p.op.absolute_address())
                    .collect();
                for a in pushes {
                    get_block(a, &mut all, &mut work, &mut seen);
                }
                let last = all[cur].last_op();
                if last.name == InstName::Cal {
                    enqueue(last.absolute_address());
                } else if last.name == InstName::Bra {
                    let s = get_block(last.absolute_address(), &mut all, &mut work, &mut seen);
                    all[cur].succs.push(s);
                    all[s].preds.push(cur);
                }
                if !is_unconditional_branch(&last) {
                    let e = all[cur].end_address;
                    let s = get_block(e, &mut all, &mut work, &mut seen);
                    all[cur].succs.insert(0, s);
                    all[s].preds.push(cur);
                }
            }
            if !order.is_empty() {
                let n = &all[order[nidx]];
                let at = nidx + usize::from(n.address < all[cur].address);
                order.insert(at, cur);
            } else {
                order.push(cur);
            }
        }
        for &bi in &order.clone() {
            for pi in 0..all[bi].push_ops.len() {
                propagate_push_op(&mut all, &seen, bi, pi, &mut tags);
            }
        }
        let new_target = find_brx_targets(ctx, &mut all, &order, &mut work, &mut seen);
        if !new_target {
            break;
        }
    }
    // remap arena indices to sorted positions
    let mut pos = vec![0usize; all.len()];
    for (p, &i) in order.iter().enumerate() {
        pos[i] = p;
    }
    let mut out = Vec::new();
    for &i in &order {
        let mut b = all[i].clone();
        b.preds = b.preds.iter().map(|&x| pos[x]).collect();
        b.succs = b.succs.iter().map(|&x| pos[x]).collect();
        for p in &mut b.push_ops {
            for c in &mut p.consumers {
                c.0 = pos[c.0];
            }
        }
        for (_, s) in &mut b.sync_targets {
            s.push.0 = pos[s.push.0];
        }
        out.push(b);
    }
    Ok(out)
}

pub(crate) fn bsearch_refs(blocks: &[&Block], address: u64) -> (bool, usize) {
    let mut index = 0;
    let (mut left, mut right) = (0i64, blocks.len() as i64 - 1);
    while left <= right {
        let middle = left + ((right - left) >> 1);
        let b = blocks[middle as usize];
        index = middle as usize;
        if address >= b.address && address < b.end_address {
            return (true, index);
        }
        if address < b.address {
            right = middle - 1;
        } else {
            left = middle + 1;
        }
    }
    (false, index)
}

fn op_bsearch(ops: &[InstOp], address: u64) -> usize {
    let (mut left, mut middle, mut right) = (0i64, 0usize, ops.len() as i64 - 1);
    while left <= right {
        middle = (left + ((right - left) >> 1)) as usize;
        let a = ops[middle].address;
        if address == a {
            break;
        }
        if address < a {
            right = middle as i64 - 1;
        } else {
            left = middle as i64 + 1;
        }
    }
    middle
}

/// `Block.Split`: `right` becomes the tail of `left`.
pub(crate) fn split(all: &mut [Block], left: usize, right: usize) -> Result<(), String> {
    let raddr = all[right].address;
    let mut idx = op_bsearch(&all[left].ops, raddr);
    if all[left].ops[idx].address < raddr {
        idx += 1;
    }
    if all[left].ops.len() <= idx {
        return Err("Can't split at right block address.".into());
    }
    let end = all[left].end_address;
    let succs = all[left].succs.clone();
    all[right].end_address = end;
    all[right].succs.extend(succs);
    all[right].preds.push(left);
    all[left].end_address = raddr;
    all[left].succs = vec![right];
    let moved: Vec<InstOp> = all[left].ops.split_off(idx);
    all[right].ops.extend(moved);
    // push consumers that point to this block
    let sts = all[left].sync_targets.clone();
    for (_, st) in &sts {
        let p = &mut all[st.push.0].push_ops[st.push.1];
        if let Some(i) = p.consumers.iter().position(|c| c.0 == left) {
            let (_, local) = p.consumers.remove(i);
            p.consumers.push((right, local));
        }
    }
    for (k, v) in sts {
        // the owner of a moved sync target is the block that holds the push op
        all[right].sync_targets.push((k, v));
    }
    all[left].sync_targets.clear();
    // the old successors have the right block as their predecessor now
    for s in all[right].succs.clone() {
        for p in &mut all[s].preds {
            if *p == left {
                *p = right;
            }
        }
    }
    if let Some(cut) = all[left]
        .push_ops
        .iter()
        .position(|p| p.op.address >= raddr)
    {
        let tail: Vec<PushOpInfo> = all[left].push_ops.split_off(cut);
        let base = all[right].push_ops.len();
        all[right].push_ops.extend(tail);
        // A split after the push ops were propagated (a BRX target found in a later round) moves push ops that
        // sync targets of any block already name by (block, index): they follow the push op to the right block.
        // Before, they kept the index into the left block, past its end or at another push op.
        for b in all.iter_mut() {
            for (_, st) in &mut b.sync_targets {
                if st.push.0 == left && st.push.1 >= cut {
                    st.push = (right, st.push.1 - cut + base);
                }
            }
        }
    }
    Ok(())
}

fn fill_block(
    stage: Stage,
    ctx: &mut Ctx,
    code: &[u8],
    start: u64,
    block: &mut Block,
    limit: u64,
) -> Result<(), String> {
    let mut address = block.address;

    loop {
        if address.wrapping_add(7) >= limit {
            break;
        }
        if address & 0x1f == 0 {
            address += 8;
            continue;
        }
        let at = (start + address) as usize;
        let Some(w) = code.get(at..at + 8) else {
            return Err("IndexOutOfRangeException: code ends inside a block".into());
        };
        let raw = u64::from_le_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]);
        let op = get_op(address, raw);
        if op.has(props::TEX_B) {
            ctx.features |= feature::BINDLESS;
        }
        use InstName::*;
        match op.name {
            Ald | Ast | Ipa => set_user_attribute_uses(stage, ctx, op.name, raw),
            Pbk | Pcnt | Ssy => block.push_ops.push(PushOpInfo {
                op,
                consumers: vec![],
            }),
            Shfl => ctx.features |= feature::SHUFFLE,
            Ldl | Stl => ctx.features |= feature::LOCAL_MEMORY,
            Atoms | AtomsCas | Lds | Sts => ctx.features |= feature::SHARED_MEMORY,
            Atom | AtomCas | Red | Stg | Suatom | SuatomB | SuatomB2 | SuatomCas | SuatomCasB
            | Sured | SuredB | Sust | SustB | SustD | SustDB => ctx.features |= feature::STORE,
            _ => {}
        }
        block.ops.push(op);
        address += 8;
        if op.has(props::BRA) {
            break;
        }
    }
    block.end_address = address;
    Ok(())
}

fn set_user_attribute_uses(stage: Stage, ctx: &mut Ctx, name: InstName, raw: u64) {
    let (offset, count, is_store, indexed, per_patch);
    if name == InstName::Ast {
        let o = InstAst(raw);
        count = o.al_size().0 + 1;
        offset = o.imm11();
        indexed = o.phys();
        per_patch = o.p();
        is_store = true;
    } else if name == InstName::Ald {
        let o = InstAld(raw);
        count = o.al_size().0 + 1;
        offset = o.imm11();
        indexed = o.phys();
        per_patch = o.p();
        is_store = o.o();
    } else {
        let o = InstIpa(raw);
        count = 1;
        offset = o.imm10();
        indexed = o.idx();
        per_patch = false;
        is_store = false;
    }
    user_attribute_uses(stage, ctx, offset, count, is_store, indexed, per_patch);
}

/// `SetUserAttributeUses(definitions, context, offset, count, isStore, indexed, perPatch)`.
pub(crate) fn user_attribute_uses(
    stage: Stage,
    ctx: &mut Ctx,
    offset: i32,
    count: i32,
    is_store: bool,
    indexed: bool,
    per_patch: bool,
) {
    if indexed {
        if is_store {
            ctx.attr.set_all_output_user_attributes();
            ctx.indexing.output_indexing = true;
        } else {
            ctx.attr.set_all_input_user_attributes();
            ctx.indexing.input_indexing = true;
        }
        return;
    }
    for e in 0..count {
        let attr = offset + e * 4;
        if per_patch {
            if (ac::USER_ATTRIBUTE_PER_PATCH_BASE..ac::USER_ATTRIBUTE_PER_PATCH_END).contains(&attr)
            {
                let index = (attr - ac::USER_ATTRIBUTE_PER_PATCH_BASE) / 16;
                if is_store {
                    ctx.attr.set_output_user_attribute_per_patch(index);
                } else {
                    ctx.attr.set_input_user_attribute_per_patch(index);
                }
            }
        } else if (ac::USER_ATTRIBUTE_BASE..ac::USER_ATTRIBUTE_END).contains(&attr) {
            let user = attr - ac::USER_ATTRIBUTE_BASE;
            let index = user / 16;
            if is_store {
                ctx.attr.set_output_user_attribute(index);
            } else {
                ctx.attr.set_input_user_attribute(index, (user >> 2) & 3);
            }
        }
        let fixed = attr == ac::FOG_COORD
            || (ac::FRONT_COLOR_DIFFUSE_R..ac::CLIP_DISTANCE0).contains(&attr)
            || (ac::TEX_COORD_BASE..ac::TEX_COORD_END).contains(&attr);
        if !is_store && fixed {
            ctx.features |= feature::FIXED_FUNC_ATTR;
        } else if is_store {
            match attr {
                ac::LAYER if !matches!(stage, Stage::Compute | Stage::Fragment) => {
                    ctx.features |= feature::RT_LAYER
                }
                ac::VIEWPORT_INDEX if stage != Stage::Fragment => {
                    ctx.features |= feature::VIEWPORT_INDEX
                }
                a if (ac::CLIP_DISTANCE0..ac::CLIP_DISTANCE0 + 32).contains(&a)
                    && (a - ac::CLIP_DISTANCE0) % 4 == 0 =>
                {
                    if stage.is_vtg() {
                        ctx.clip |= 1 << ((a - ac::CLIP_DISTANCE0) / 4);
                    }
                }
                ac::VIEWPORT_MASK if stage != Stage::Fragment => {
                    ctx.features |= feature::VIEWPORT_MASK
                }
                _ => {}
            }
        } else {
            match attr {
                ac::POSITION_X | ac::POSITION_Y if stage == Stage::Fragment => {
                    ctx.features |= feature::FRAG_COORD_XY
                }
                ac::INSTANCE_ID if stage == Stage::Vertex => ctx.features |= feature::INSTANCE_ID,
                _ => {}
            }
        }
    }
}

/// The text the probe writes for the decoded program (first attempt).
pub fn dump(p: &DecodedProgram) -> String {
    use std::fmt::Write;
    let mut s = String::from("== attempt\n");
    for f in &p.functions {
        let _ = writeln!(s, "func {:x} type=0 blocks={}", f.address, f.blocks.len());
        for b in &f.blocks {
            let _ = write!(s, " block {:x}-{:x} succ=[", b.address, b.end_address);
            for x in &b.succs {
                let _ = write!(s, "{:x},", f.blocks[*x].address);
            }
            s.push_str("] push=[");
            for x in &b.push_ops {
                let _ = write!(s, "{:x},", x.op.address);
            }
            s.push_str("]\n");
            for op in &b.ops {
                let _ = writeln!(s, "  {:x} {:?} {:016x}", op.address, op.name, op.raw);
            }
        }
    }
    let _ = writeln!(
        s,
        "features={} clip={} cb1={} size={}",
        p.used_features, p.clip_distances_written, p.cb1_data_size, p.size
    );
    let _ = writeln!(
        s,
        "attr in={:x} out={:x}",
        p.attribute_usage.used_input_attributes, p.attribute_usage.used_output_attributes
    );
    s
}
