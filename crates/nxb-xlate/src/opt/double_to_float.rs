//! `DoubleToFloat`: float64 operations become float32 ones when the host has no float64 support, and the packing and
//! unpacking of doubles go through the soft float helper functions.
use super::utils;
use crate::helper::{HelperName, Hfm};
use crate::ir::{Block, Inst, Ir, StorageKind};

type R<T> = Result<T, String>;

/// `DoubleToFloat.RunPass` for one block.
pub fn run_block(ir: &mut Ir, block: &mut Block, hfm: &mut Hfm) -> R<()> {
    let mut i = 0;
    while i < block.ops.len() {
        let node = block.ops[i];
        if !ir.is_op(node) {
            i += 1;
            continue;
        }
        let inst = ir.op(node).inst;
        if inst == Inst::PackDouble2x32 {
            let id = hfm.get_or_create_function_id(ir, HelperName::ConvertDoubleToFloat)?;
            let (s0, s1) = (src(ir, node, 0)?, src(ir, node, 1)?);
            let float_value = ir.op(node).dests.first().copied().flatten();
            let fid = ir.const_(id);
            ir.clear_dests(node);
            let call = ir.new_op_multi(Inst::Call, 0, &[float_value], &[fid, s0, s1]);
            block.ops.insert(i, call);
            utils::delete_node(ir, block, i + 1);
        } else if inst == Inst::UnpackDouble2x32 {
            let id = hfm.get_or_create_function_id(ir, HelperName::ConvertFloatToDouble)?;
            let index = ir.op(node).index;
            let dest = ir.op(node).dests.first().copied().flatten();
            let low = if index == 0 { dest } else { Some(ir.local()) };
            let high = if index == 1 { dest } else { Some(ir.local()) };
            ir.clear_dests(node);
            let s0 = src(ir, node, 0)?;
            let fid = ir.const_(id);
            let args = [
                fid,
                s0,
                low.ok_or("unpack without destination")?,
                high.ok_or("unpack without destination")?,
            ];
            let call = ir.new_op(Inst::Call, StorageKind::None, None, &args);
            block.ops.insert(i, call);
            utils::delete_node(ir, block, i + 1);
        } else {
            turn_double_into_float(ir, node);
        }
        i += 1;
    }
    Ok(())
}

fn src(ir: &Ir, n: crate::ir::NodeId, i: usize) -> R<crate::ir::Opd> {
    ir.op(n)
        .sources
        .get(i)
        .copied()
        .flatten()
        .ok_or_else(|| "missing source".to_string())
}

/// `Operation.TurnDoubleIntoFloat`.
fn turn_double_into_float(ir: &mut Ir, n: crate::ir::NodeId) {
    let inst = ir.op(n).inst;
    let new = if inst.0 & !Inst::MASK == Inst::FP64 {
        Inst((inst.0 & Inst::MASK) | Inst::FP32)
    } else {
        match inst {
            Inst::ConvertFP32ToFP64 | Inst::ConvertFP64ToFP32 => Inst::Copy,
            Inst::ConvertFP64ToS32 => Inst::ConvertFP32ToS32,
            Inst::ConvertFP64ToU32 => Inst::ConvertFP32ToU32,
            Inst::ConvertS32ToFP64 => Inst::ConvertS32ToFP32,
            Inst::ConvertU32ToFP64 => Inst::ConvertU32ToFP32,
            _ => return,
        }
    };
    ir.op_mut(n).inst = new;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_operations_become_float_operations() {
        let mut ir = Ir::default();
        let (a, b, d) = (ir.local(), ir.local(), ir.local());
        let add = ir.new_op(
            Inst(Inst::FP64 | Inst::Add.0),
            StorageKind::None,
            Some(d),
            &[a, b],
        );
        let conv = ir.new_op(Inst::ConvertS32ToFP64, StorageKind::None, Some(a), &[b]);
        let narrow = ir.new_op(Inst::ConvertFP64ToFP32, StorageKind::None, Some(b), &[a]);
        let mut block = Block {
            index: 0,
            ops: vec![add, conv, narrow],
            ..Default::default()
        };
        let mut hfm = Hfm::new(1, crate::translation::props::ShaderStage::Compute);
        run_block(&mut ir, &mut block, &mut hfm).expect("pass");
        assert_eq!(ir.op(add).inst.0, Inst::FP32 | Inst::Add.0);
        assert_eq!(ir.op(conv).inst, Inst::ConvertS32ToFP32);
        assert_eq!(ir.op(narrow).inst, Inst::Copy);
        assert!(hfm.functions.len() == 1);
    }
}
