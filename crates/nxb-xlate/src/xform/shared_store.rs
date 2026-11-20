//! `SharedStoreSmallIntCas` and `HelperFunctionManager.GenerateSharedStore`: a store of a byte or a half word to shared
//! memory becomes a call of a helper that inserts it into its word with a compare-and-swap loop, since neighbouring
//! threads write neighbouring bytes. The MSL emitter replaces the body of the helper with one byte or half word store
//! (`msl/shared_store.rs`).
use super::XformCtx;
use crate::helper::{B, Function};
use crate::ir::{Block, Inst, Ir, StorageKind};

type R<T> = Result<T, String>;

/// Key of a helper in `Hfm::ids`: the store width and the shared memory id are parameters of the function.
const KEY_BASE: i32 = 0x2000_0000;

fn generate(ir: &mut Ir, id: i32, bit_size: i32) -> R<Function> {
    let mut b = B::new(ir);
    let offset = b.ir.argument(0);
    let value = b.ir.argument(1);
    let k2 = b.ir.const_(2);
    let word_offset = b.op(Inst::ShiftRightU32, &[offset, k2]);
    let k3 = b.ir.const_(3);
    let low = b.op(Inst::BitwiseAnd, &[offset, k3]);
    let k3 = b.ir.const_(3);
    let bit_offset = b.op(Inst::ShiftLeft, &[low, k3]);
    let retry = b.ir.label();
    b.mark_label(retry);
    let mem = b.ir.const_(id);
    let old = b.op_kind(Inst::Load, StorageKind::SharedMemory, &[mem, word_offset]);
    let size = b.ir.const_(bit_size);
    let new = b.op(Inst::BitfieldInsert, &[old, value, bit_offset, size]);
    let found = b.op_kind(
        Inst::AtomicCompareAndSwap,
        StorageKind::SharedMemory,
        &[mem, word_offset, old, new],
    );
    let failed = b.op(Inst::CompareNotEqual, &[found, old]);
    b.branch_if_true(retry, failed);
    b.ret(None);
    b.finish_with(&format!("SharedStore{bit_size}_{id}"), false, 2, 0)
}

/// `SharedStoreSmallIntCas.RunPass` over all blocks.
pub fn run_pass(ir: &mut Ir, blocks: &mut [Block], ctx: &mut XformCtx) -> R<()> {
    for block in blocks.iter_mut() {
        let mut i = 0;
        while i < block.ops.len() {
            let node = block.ops[i];
            if ir.is_op(node) && ir.op(node).inst == Inst::Store {
                let bit_size = match ir.op(node).storage_kind {
                    StorageKind::SharedMemory8 => 8,
                    StorageKind::SharedMemory16 => 16,
                    _ => 0,
                };
                if bit_size != 0 {
                    let srcs = &ir.op(node).sources;
                    let (Some(mem), Some(offset), Some(value)) = (
                        srcs.first().copied().flatten(),
                        srcs.get(1).copied().flatten(),
                        srcs.get(2).copied().flatten(),
                    ) else {
                        return Err("shared store without operands".into());
                    };
                    let mem_id = ir.value(mem);
                    let key = KEY_BASE | bit_size << 24 | mem_id;
                    let fid = if let Some(id) = ctx.hfm.ids.get(&key) {
                        *id
                    } else {
                        let f = generate(ir, mem_id, bit_size)?;
                        let id = ctx.hfm.add_function(f);
                        ctx.hfm.ids.insert(key, id);
                        id
                    };
                    let fc = ir.const_(fid);
                    let call = ir.new_op_multi(Inst::Call, 0, &[], &[fc, offset, value]);
                    block.ops.insert(i, call);
                    crate::opt::utils::delete_node(ir, block, i + 1);
                }
            }
            i += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Hfm;
    use crate::translation::props::ShaderStage;

    fn store(ir: &mut Ir, kind: StorageKind, mem: i32) -> crate::ir::NodeId {
        let (m, off, v) = (ir.const_(mem), ir.const_(6), ir.const_(7));
        ir.new_op(Inst::Store, kind, None, &[m, off, v])
    }

    #[test]
    fn small_shared_stores_become_calls_of_one_helper_per_width_and_memory() {
        let mut ir = Ir::default();
        let ops = vec![
            store(&mut ir, StorageKind::SharedMemory8, 0),
            store(&mut ir, StorageKind::SharedMemory8, 0),
            store(&mut ir, StorageKind::SharedMemory16, 0),
            store(&mut ir, StorageKind::SharedMemory, 0),
        ];
        let mut blocks = vec![Block {
            ops,
            ..Block::default()
        }];
        let mut hfm = Hfm::new(1, ShaderStage::Compute);
        let mut features = 0;
        let mut ctx = XformCtx {
            stage: ShaderStage::Compute,
            used_features: &mut features,
            hfm: &mut hfm,
            definitions: None,
            idom: &[],
        };
        run_pass(&mut ir, &mut blocks, &mut ctx).expect("pass");
        let insts: Vec<Inst> = blocks[0].ops.iter().map(|n| ir.op(*n).inst).collect();
        assert_eq!(insts, [Inst::Call, Inst::Call, Inst::Call, Inst::Store]);
        assert_eq!(hfm.functions.len(), 3);
        let names: Vec<&str> = hfm.functions[1..]
            .iter()
            .flatten()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, ["SharedStore8_0", "SharedStore16_0"]);
    }
}
