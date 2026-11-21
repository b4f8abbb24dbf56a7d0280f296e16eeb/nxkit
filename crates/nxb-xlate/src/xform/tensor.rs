//! `TensorPass` and `HelperFunctionManager.GenerateHmma16816F16`: Ampere HMMA.16816 with an f16 accumulator becomes a call of the
//! helper `dmsh_hmma16816_f16`.
//!
//! Arguments: the lane, the subgroup's block offset in the scratch, A (4), B (2), C (2), then what `prepare` found: the
//! flags below, and for `ADIRECT` the row address of the LDSM that read A, the offset of each of A's four matrices
//! from lane 0's row and the row stride (bytes). The translator's own body reads only the registers, so each flag is a
//! promise about values that body computes anyway. The MSL body (`msl::hmma`) uses them to keep matrices in the
//! SIMD-group's registers from one call to the next and to load A where its LDSM read it. `MatrixLoad` (one matrix of
//! an LDSM in a compute shader) becomes its per-lane code here, after `prepare` has seen which HMMAs it feeds.
use super::XformCtx;
use super::uniformity::Uniformity;
use crate::emit::ampere::tensor::{LaneSink, ldsm_lane};
use crate::helper::{B, Function};
use crate::ir::{Block, Inst, IoVariable, Ir, NodeId, Opd, OperandType, StorageKind};
use std::collections::HashMap;

/// A is the previous call's (the call before it in the same block): its matrices are still loaded.
pub const AREUSE: i32 = 1;
/// C is the D of an earlier call of the block, left in accumulator slot `flags >> 8`, and has no other use.
pub const CSLOT: i32 = 2;
/// D is a later call's C and nothing else's: it stays in the slot, the registers are not written.
pub const DSLOT: i32 = 4;
/// C is zero.
pub const CZERO: i32 = 8;
/// A is the matrices of one LDSM of the block with no shared memory write between: rows at the LDSM's lane 0 address
/// plus offset, stride apart.
pub const ADIRECT: i32 = 16;
/// That LDSM is .MT88 (A is the transpose of what lies in memory).
pub const ATRANS: i32 = 32;
/// Accumulator slots, two 8 x 8 matrices each.
const SLOTS: usize = 8;
/// lane, block, A (4), B (2), C (2), flags, LDSM address, A's four offsets, row stride
pub const HMMA_IN_ARGUMENTS: usize = 17;

type R<T> = Result<T, String>;

/// Key of a helper in `Hfm::ids` (the scratch memory id is a parameter of the function).
const KEY_BASE: i32 = 0x4000_0000;

fn c(b: &mut B, v: i32) -> Opd {
    b.ir.const_(v)
}

fn add(b: &mut B, x: Opd, y: Opd) -> Opd {
    b.op(Inst::Add, &[x, y])
}

fn shl(b: &mut B, x: Opd, k: i32) -> Opd {
    let k = c(b, k);
    b.op(Inst::ShiftLeft, &[x, k])
}

fn unpack(b: &mut B, a: Opd, index: i32) -> Opd {
    let d = b.ir.local();
    let n =
        b.ir.new_op_multi(Inst::UnpackHalf2x16, index, &[Some(d)], &[a]);
    b.push(n);
    d
}

fn fma(b: &mut B, x: Opd, y: Opd, z: Opd) -> Opd {
    b.op(Inst(Inst::FP32) | Inst::FusedMultiplyAdd, &[x, y, z])
}

fn load(b: &mut B, id: i32, off: Opd) -> Opd {
    let idc = c(b, id);
    b.op_kind(Inst::Load, StorageKind::SharedMemory, &[idc, off])
}

fn store(b: &mut B, id: i32, off: Opd, v: Opd) {
    let idc = c(b, id);
    b.raw(Inst::Store, StorageKind::SharedMemory, None, &[idc, off, v]);
}

/// `At(row, word)`: `bse + (row << 3) + (t + word)`.
fn at(b: &mut B, bse: Opd, t: Opd, row: Opd, word: i32) -> Opd {
    let s = shl(b, row, 3);
    let w = c(b, word);
    let tw = add(b, t, w);
    let i = add(b, s, tw);
    add(b, bse, i)
}

/// `Ct(row)`: `bse + (192 + ((row << 2) + t))`.
fn ct(b: &mut B, bse: Opd, t: Opd, row: Opd) -> Opd {
    let s = shl(b, row, 2);
    let st = add(b, s, t);
    let k = c(b, 192);
    let i = add(b, k, st);
    add(b, bse, i)
}

fn generate(ir: &mut Ir, id: i32) -> R<Function> {
    let mut b = B::new(ir);
    let lane = b.ir.argument(0);
    let bse = b.ir.argument(1);
    let k2 = c(&mut b, 2);
    let g = b.op(Inst::ShiftRightU32, &[lane, k2]);
    let k3 = c(&mut b, 3);
    let t = b.op(Inst::BitwiseAnd, &[lane, k3]);
    let k8 = c(&mut b, 8);
    let g8 = add(&mut b, g, k8);
    let a = |b: &mut B, n: i32| b.ir.argument(n);
    let o = at(&mut b, bse, t, g, 0);
    let v = a(&mut b, 2);
    store(&mut b, id, o, v);
    let o = at(&mut b, bse, t, g8, 0);
    let v = a(&mut b, 3);
    store(&mut b, id, o, v);
    let o = at(&mut b, bse, t, g, 4);
    let v = a(&mut b, 4);
    store(&mut b, id, o, v);
    let o = at(&mut b, bse, t, g8, 4);
    let v = a(&mut b, 5);
    store(&mut b, id, o, v);
    for (word, arg) in [(0, 6), (4, 7)] {
        let x = at(&mut b, bse, t, g, word);
        let k = c(&mut b, 128);
        let o = add(&mut b, k, x);
        let v = a(&mut b, arg);
        store(&mut b, id, o, v);
    }
    let o = ct(&mut b, bse, t, g);
    let v = a(&mut b, 8);
    store(&mut b, id, o, v);
    let o = ct(&mut b, bse, t, g8);
    let v = a(&mut b, 9);
    store(&mut b, id, o, v);
    b.raw(Inst::SubgroupBarrier, StorageKind::None, None, &[]);
    let mut d = Vec::new();
    for q in 0..2 {
        let row = if q == 0 { g } else { g8 };
        let cw = a(&mut b, 8 + q);
        let lo = unpack(&mut b, cw, 0);
        let hi = unpack(&mut b, cw, 1);
        let mut acc = [lo, hi];
        let rs = shl(&mut b, row, 3);
        let row_base = add(&mut b, bse, rs);
        for j in 0..2 {
            let ts = shl(&mut b, t, 1);
            let jj = c(&mut b, j);
            let col = add(&mut b, ts, jj);
            let cs = shl(&mut b, col, 3);
            let k = c(&mut b, 128);
            let kc = add(&mut b, k, cs);
            let col_base = add(&mut b, bse, kc);
            for kw in 0..8 {
                let kk = c(&mut b, kw);
                let ao = add(&mut b, row_base, kk);
                let aw = load(&mut b, id, ao);
                let kk2 = c(&mut b, kw);
                let bo = add(&mut b, col_base, kk2);
                let bw = load(&mut b, id, bo);
                let (al, bl) = (unpack(&mut b, aw, 0), unpack(&mut b, bw, 0));
                acc[j as usize] = fma(&mut b, al, bl, acc[j as usize]);
                let (ah, bh) = (unpack(&mut b, aw, 1), unpack(&mut b, bw, 1));
                acc[j as usize] = fma(&mut b, ah, bh, acc[j as usize]);
            }
        }
        d.push(b.op(Inst::PackHalf2x16, &[acc[0], acc[1]]));
    }
    b.raw(Inst::SubgroupBarrier, StorageKind::None, None, &[]);
    let out = a(&mut b, HMMA_IN_ARGUMENTS as i32);
    let copy =
        b.ir.new_op(Inst::Copy, StorageKind::None, Some(out), &[d[1]]);
    b.push(copy);
    b.ret(Some(d[0]));
    b.finish_with("dmsh_hmma16816_f16", true, HMMA_IN_ARGUMENTS as i32, 1)
}

impl LaneSink for B<'_> {
    fn konst(&mut self, v: i32) -> Opd {
        self.ir.const_(v)
    }
    fn put(&mut self, inst: Inst, kind: StorageKind, src: &[Opd]) -> Opd {
        self.op_kind(inst, kind, src)
    }
}

/// `LowerLoad`: MatrixLoad (shared memory id, mode, row address, matrix) becomes `LdsmLane`, as the emitter writes it
/// outside compute shaders. Returns the number of operations put in place of it.
fn lower_load(ir: &mut Ir, block: &mut Block, i: usize) -> R<usize> {
    let node = block.ops[i];
    let o = ir.op(node);
    let src: Vec<Opd> = o.sources.iter().flatten().copied().collect();
    let Some(dest) = o.dests.first().copied().flatten() else {
        return Err("MatrixLoad without a destination".into());
    };
    if src.len() != 4 {
        return Err("MatrixLoad sources".into());
    }
    let (id, mode, addr, m) = (ir.value(src[0]), ir.value(src[1]), src[2], ir.value(src[3]));
    ir.clear_dests(node);
    let mut b = B::new(ir);
    let io = b.ir.const_(IoVariable::SubgroupLaneId.0 as i32);
    let mut lane = b.op_kind(Inst::Load, StorageKind::Input, &[io]);
    if crate::translation::host::SUBGROUP_SIZE > 32 {
        let k = c(&mut b, 0x1f);
        lane = b.op(Inst::BitwiseAnd, &[lane, k]);
    }
    let k2 = c(&mut b, 2);
    let g = b.op(Inst::ShiftRightU32, &[lane, k2]);
    let k3 = c(&mut b, 3);
    let t = b.op(Inst::BitwiseAnd, &[lane, k3]);
    let v = ldsm_lane(&mut b, g, t, addr, id, m, mode);
    b.raw(Inst::Copy, StorageKind::None, Some(dest), &[v]);
    let ops = b.take();
    let n = ops.len();
    for (k, op) in ops.into_iter().enumerate() {
        block.ops.insert(i + k, op);
    }
    crate::opt::utils::delete_node(ir, block, i + n);
    Ok(n)
}

/// Whether an operation may write shared memory, or let the other subgroups' writes show (a barrier). A call is taken
/// to do either.
fn writes_shared(ir: &Ir, n: NodeId) -> bool {
    let o = ir.op(n);
    let inst = Inst(o.inst.0 & Inst::MASK);
    let atomic = matches!(
        inst,
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
    );
    if inst == Inst::Store || atomic {
        return matches!(
            o.storage_kind,
            StorageKind::SharedMemory | StorageKind::SharedMemory8 | StorageKind::SharedMemory16
        );
    }
    matches!(
        inst,
        Inst::Barrier | Inst::GroupMemoryBarrier | Inst::MemoryBarrier | Inst::Call
    )
}

fn src(ir: &Ir, n: NodeId, i: usize) -> Option<Opd> {
    ir.op(n).sources.get(i).copied().flatten()
}

fn is_zero(ir: &Ir, x: Option<Opd>) -> bool {
    x.is_some_and(|x| ir.ty(x) == OperandType::Constant && ir.value(x) == 0)
}

/// `c0`, `c1` read by `op` as its C and by nothing else.
fn only_as_c(ir: &Ir, op: NodeId, c0: Opd, c1: Opd) -> bool {
    if c0 == c1 || ir.data(c0).use_ops.len() != 1 || ir.data(c1).use_ops.len() != 1 {
        return false;
    }
    ir.op(op)
        .sources
        .iter()
        .enumerate()
        .all(|(s, x)| s == 9 || s == 10 || (*x != Some(c0) && *x != Some(c1)))
}

fn same_a(ir: &Ir, a: NodeId, b: NodeId) -> bool {
    (3..7).all(|k| match (src(ir, a, k), src(ir, b, k)) {
        (Some(x), Some(y)) => {
            x == y
                || (ir.ty(x) == OperandType::Constant
                    && ir.ty(y) == OperandType::Constant
                    && ir.value(x) == ir.value(y))
        }
        _ => false,
    })
}

/// The MatrixLoads of A's four registers, all of one LDSM (the same row address and mode).
fn loads_of_a(ir: &Ir, op: NodeId) -> Option<[NodeId; 4]> {
    let mut loads = [NodeId(0); 4];
    for k in 0..4 {
        let a = src(ir, op, 3 + k)?;
        if ir.ty(a) != OperandType::LocalVariable {
            return None;
        }
        let def = ir.asg_operation(a)?;
        if ir.op(def).inst != Inst::MatrixLoad {
            return None;
        }
        if k != 0
            && (src(ir, def, 2) != src(ir, loads[0], 2)
                || src(ir, def, 1).map(|x| ir.value(x))
                    != src(ir, loads[0], 1).map(|x| ir.value(x)))
        {
            return None;
        }
        loads[k] = def;
    }
    Some(loads)
}

/// `TensorPass.Prepare`, before any other transform: the flags of each MatrixMultiplyAdd, appended to its sources.
pub fn prepare(ir: &mut Ir, blocks: &[Block], ctx: &XformCtx) {
    let mut uniformity: Option<Uniformity> = None;
    for block in blocks {
        let mut mmas = Vec::new();
        let mut epoch_at: HashMap<NodeId, i32> = HashMap::new();
        let mut epoch = 0;
        for &n in &block.ops {
            if !ir.is_op(n) {
                continue;
            }
            match ir.op(n).inst {
                Inst::MatrixLoad => {
                    epoch_at.insert(n, epoch);
                }
                Inst::MatrixMultiplyAdd => {
                    mmas.push(n);
                    epoch_at.insert(n, epoch);
                }
                _ if writes_shared(ir, n) => epoch += 1,
                _ => {}
            }
        }
        if mmas.is_empty() {
            continue;
        }
        // Chains: a call whose C is the D of an earlier call that nothing else reads.
        let count = mmas.len();
        let (mut prev, mut next, mut slot) =
            (vec![-1i32; count], vec![-1i32; count], vec![0usize; count]);
        let index: HashMap<NodeId, usize> = mmas.iter().enumerate().map(|(i, n)| (*n, i)).collect();
        for i in 0..count {
            let (Some(c0), Some(c1)) = (src(ir, mmas[i], 9), src(ir, mmas[i], 10)) else {
                continue;
            };
            if ir.ty(c0) != OperandType::LocalVariable {
                continue;
            }
            let Some(from) = ir.asg_operation(c0) else {
                continue;
            };
            let Some(&j) = index.get(&from) else {
                continue;
            };
            let d = &ir.op(from).dests;
            if j < i
                && d.first().copied().flatten() == Some(c0)
                && d.get(1).copied().flatten() == Some(c1)
                && only_as_c(ir, mmas[i], c0, c1)
            {
                prev[i] = j as i32;
                next[j] = i as i32;
            }
        }
        let mut busy = [false; SLOTS];
        for i in 0..count {
            let op = mmas[i];
            if prev[i] >= 0 {
                slot[i] = slot[prev[i] as usize];
            } else if next[i] >= 0 {
                match busy.iter().position(|b| !b) {
                    // More chains at once than slots: this one goes through the registers.
                    None => {
                        prev[next[i] as usize] = -1;
                        next[i] = -1;
                    }
                    Some(free) => {
                        busy[free] = true;
                        slot[i] = free;
                    }
                }
            }
            if prev[i] >= 0 && next[i] < 0 {
                busy[slot[i]] = false;
            }
            let mut flags = 0;
            if prev[i] >= 0 {
                flags |= CSLOT | (slot[i] as i32) << 8;
            }
            if next[i] >= 0 {
                flags |= DSLOT | (slot[i] as i32) << 8;
            }
            if prev[i] < 0 && is_zero(ir, src(ir, op, 9)) && is_zero(ir, src(ir, op, 10)) {
                flags |= CZERO;
            }
            if i > 0 && same_a(ir, mmas[i - 1], op) {
                flags |= AREUSE;
            }
            let mut addr = None;
            let mut offsets = [0; 4];
            let mut stride = 0;
            let loads = loads_of_a(ir, op);
            let same_epoch = loads.is_some_and(|l| {
                l.iter().all(|x| {
                    epoch_at
                        .get(x)
                        .is_some_and(|e| Some(e) == epoch_at.get(&op))
                })
            });
            if let (true, Some(loads), Some(defs)) = (same_epoch, loads, ctx.definitions) {
                let u = uniformity.get_or_insert_with(|| {
                    Uniformity::new(ir, blocks, ctx.idom, defs.compute_local_size)
                });
                let rows = src(ir, loads[0], 2);
                let m_of = |l: NodeId| src(ir, l, 3).map_or(0, |x| ir.value(x)) as usize;
                let n = loads.iter().map(|l| m_of(*l) + 1).max().unwrap_or(1);
                if let Some((starts, st)) = rows.and_then(|r| u.row_starts(ir, r, n)) {
                    let trans = src(ir, loads[0], 1).is_some_and(|x| ir.value(x) == 1);
                    flags |= ADIRECT | if trans { ATRANS } else { 0 };
                    addr = rows;
                    stride = st;
                    for k in 0..4 {
                        offsets[k] = starts[m_of(loads[k])];
                    }
                }
            }
            let addr = addr.unwrap_or_else(|| ir.const_(0));
            let mut extra = vec![ir.const_(flags), addr];
            for v in offsets.into_iter().chain([stride]) {
                extra.push(ir.const_(v));
            }
            ir.append_sources(op, &extra);
        }
    }
}

/// `TensorPass.RunPass` over all blocks.
pub fn run_pass(ir: &mut Ir, blocks: &mut [Block], ctx: &mut XformCtx) -> R<()> {
    for block in blocks.iter_mut() {
        let mut i = 0;
        while i < block.ops.len() {
            let node = block.ops[i];
            if ir.is_op(node) && ir.op(node).inst == Inst::MatrixLoad {
                i += lower_load(ir, block, i)?;
                continue;
            }
            if ir.is_op(node) && ir.op(node).inst == Inst::MatrixMultiplyAdd {
                let o = ir.op(node);
                let (Some(mem), Some(d0), Some(d1)) = (
                    o.sources.first().copied().flatten(),
                    o.dests.first().copied().flatten(),
                    o.dests.get(1).copied().flatten(),
                ) else {
                    return Err("MatrixMultiplyAdd without operands".into());
                };
                let srcs: Vec<Opd> = o.sources.iter().skip(1).flatten().copied().collect();
                if srcs.len() != HMMA_IN_ARGUMENTS {
                    return Err("MatrixMultiplyAdd sources".into());
                }
                let mem_id = ir.value(mem);
                let key = KEY_BASE + mem_id;
                let fid = if let Some(id) = ctx.hfm.ids.get(&key) {
                    *id
                } else {
                    let f = generate(ir, mem_id)?;
                    let id = ctx.hfm.add_function(f);
                    ctx.hfm.ids.insert(key, id);
                    id
                };
                ir.clear_dests(node);
                let fc = ir.const_(fid);
                let mut args = vec![fc];
                args.extend(srcs);
                args.push(d1);
                let call = ir.new_op_multi(Inst::Call, 0, &[Some(d0)], &args);
                block.ops.insert(i, call);
                crate::opt::utils::delete_node(ir, block, i + 1);
            }
            i += 1;
        }
    }
    Ok(())
}
