//! `ShufflePass` and the shuffle helper functions of `HelperFunctionManager` (`Shuffle`, `ShuffleDown`, `ShuffleUp`,
//! `ShuffleXor`), generated for the host subgroup size.
use super::XformCtx;
use crate::helper::{B, Function, HelperName, Hfm};
use crate::ir::{Block, Inst, IoVariable, Ir, Opd, StorageKind};
use crate::translation::host;

type R<T> = Result<T, String>;

impl Hfm {
    /// `GetOrCreateShuffleFunctionId`.
    pub fn get_or_create_shuffle_function_id(
        &mut self,
        ir: &mut Ir,
        name: HelperName,
        subgroup_size: i32,
    ) -> R<i32> {
        if let Some(id) = self.ids.get(&(name as i32)) {
            return Ok(*id);
        }
        let f = generate_shuffle(ir, name, subgroup_size)?;
        let id = self.add_function(f);
        self.ids.insert(name as i32, id);
        Ok(id)
    }
}

fn c(b: &mut B, v: i32) -> Opd {
    b.ir.const_(v)
}

fn lane_id(b: &mut B, subgroup_size: i32) -> Opd {
    let io = c(b, IoVariable::SubgroupLaneId.0 as i32);
    let l = b.op_kind(Inst::Load, StorageKind::Input, &[io]);
    if subgroup_size <= 32 {
        return l;
    }
    let m = c(b, 0x1f);
    b.op(Inst::BitwiseAnd, &[l, m])
}

fn shuffle_index(b: &mut B, src: Opd, subgroup_size: i32) -> Opd {
    if subgroup_size <= 32 {
        return src;
    }
    let io = c(b, IoVariable::SubgroupLaneId.0 as i32);
    let l = b.op_kind(Inst::Load, StorageKind::Input, &[io]);
    let m = c(b, 0x60);
    let hi = b.op(Inst::BitwiseAnd, &[l, m]);
    b.op(Inst::BitwiseOr, &[hi, src])
}

/// `(mask >> 8) & 0x1f`.
fn seg_mask(b: &mut B, mask: Opd) -> Opd {
    let k8 = c(b, 8);
    let sh = b.op(Inst::ShiftRightU32, &[mask, k8]);
    let k = c(b, 0x1f);
    b.op(Inst::BitwiseAnd, &[sh, k])
}

/// `(clamp & ~segMask) | minThreadId`.
fn max_thread(b: &mut B, clamp: Opd, seg: Opd, min: Opd) -> Opd {
    let n = b.op(Inst::BitwiseNot, &[seg]);
    let a = b.op(Inst::BitwiseAnd, &[clamp, n]);
    b.op(Inst::BitwiseOr, &[a, min])
}

fn generate_shuffle(ir: &mut Ir, name: HelperName, subgroup_size: i32) -> R<Function> {
    let mut b = B::new(ir);
    let value = b.ir.argument(0);
    let index = b.ir.argument(1);
    let mask = b.ir.argument(2);
    let (valid, label) = match name {
        HelperName::Shuffle | HelperName::ShuffleDown | HelperName::ShuffleXor => {
            let k = c(&mut b, 0x1f);
            let clamp = b.op(Inst::BitwiseAnd, &[mask, k]);
            let seg = seg_mask(&mut b, mask);
            let (src, min, max);
            if name == HelperName::Shuffle {
                let lane = lane_id(&mut b, subgroup_size);
                min = b.op(Inst::BitwiseAnd, &[lane, seg]);
                max = max_thread(&mut b, clamp, seg, min);
                let n = b.op(Inst::BitwiseNot, &[seg]);
                let a = b.op(Inst::BitwiseAnd, &[index, n]);
                src = b.op(Inst::BitwiseOr, &[a, min]);
            } else {
                let lane = lane_id(&mut b, subgroup_size);
                min = b.op(Inst::BitwiseAnd, &[lane, seg]);
                max = max_thread(&mut b, clamp, seg, min);
                let inst = if name == HelperName::ShuffleDown {
                    Inst::Add
                } else {
                    Inst::BitwiseExclusiveOr
                };
                src = b.op(inst, &[lane, index]);
            }
            let valid = b.op(Inst::CompareLessOrEqualU32, &[src, max]);
            (valid, (src, "Shuffle"))
        }
        HelperName::ShuffleUp => {
            let seg = seg_mask(&mut b, mask);
            let lane = lane_id(&mut b, subgroup_size);
            let min = b.op(Inst::BitwiseAnd, &[lane, seg]);
            let src = b.op(Inst::Subtract, &[lane, index]);
            let valid = b.op(Inst::CompareGreaterOrEqual, &[src, min]);
            (valid, (src, "ShuffleUp"))
        }
        _ => return Err("not a shuffle function".into()),
    };
    let (src, _) = label;
    let arg3 = b.ir.argument(3);
    let copy =
        b.ir.new_op(Inst::Copy, StorageKind::None, Some(arg3), &[valid]);
    b.push(copy);
    let si = shuffle_index(&mut b, src, subgroup_size);
    let result = b.op(Inst::Shuffle, &[value, si]);
    let sel = b.op(Inst::ConditionalSelect, &[valid, result, value]);
    b.ret(Some(sel));
    let fname = match name {
        HelperName::Shuffle => "Shuffle",
        HelperName::ShuffleDown => "ShuffleDown",
        HelperName::ShuffleUp => "ShuffleUp",
        _ => "ShuffleXor",
    };
    b.finish_with(fname, true, 3, 1)
}

/// `ShufflePass.RunPass` over all blocks.
pub fn run_pass(ir: &mut Ir, blocks: &mut [Block], ctx: &mut XformCtx) -> R<()> {
    for block in blocks.iter_mut() {
        let mut i = 0;
        while i < block.ops.len() {
            let node = block.ops[i];
            if ir.is_op(node) {
                let o = ir.op(node);
                let name = match o.inst {
                    Inst::Shuffle => Some(HelperName::Shuffle),
                    Inst::ShuffleDown => Some(HelperName::ShuffleDown),
                    Inst::ShuffleUp => Some(HelperName::ShuffleUp),
                    Inst::ShuffleXor => Some(HelperName::ShuffleXor),
                    _ => None,
                };
                if let Some(name) = name
                    && o.sources.len() == 3
                    && o.dests.len() == 2
                {
                    let (Some(result), Some(valid)) = (o.dests[0], o.dests[1]) else {
                        return Err("shuffle without destinations".into());
                    };
                    let (value, index, mask) = match (o.sources[0], o.sources[1], o.sources[2]) {
                        (Some(a), Some(b), Some(c)) => (a, b, c),
                        _ => return Err("shuffle without sources".into()),
                    };
                    let id =
                        ctx.hfm
                            .get_or_create_shuffle_function_id(ir, name, host::SUBGROUP_SIZE)?;
                    ir.clear_dests(node);
                    let fid = ir.const_(id);
                    let call = ir.new_op_multi(
                        Inst::Call,
                        0,
                        &[Some(result)],
                        &[fid, value, index, mask, valid],
                    );
                    block.ops.insert(i, call);
                    crate::opt::utils::delete_node(ir, block, i + 1);
                }
            }
            i += 1;
        }
    }
    Ok(())
}
