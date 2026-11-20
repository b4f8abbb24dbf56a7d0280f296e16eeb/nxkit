//! Tests of loops whose condition needs statements.

use super::gaps_tests::msl;
use super::tests::{B, compute};
use crate::ir::agg::AggType;
use crate::ir::{Inst, StorageKind};
use crate::structured::ast::BlockType;

/// `do { if (t == 3) break; if (t == 4) continue; store } while (atomicAdd(shm[t], 1) != 0)`. A plain condition when
/// `atomic` is false.
pub(super) fn spin(atomic: bool) -> B {
    let mut b = compute();
    let zero = b.k(0);
    let t = b.load_io(
        StorageKind::Input,
        crate::ir::IoVariable::ThreadId,
        None,
        0,
        AggType::U32,
    );
    let cond = if atomic {
        let one = b.k(1);
        let at = b.op(
            Inst::AtomicAdd,
            StorageKind::SharedMemory,
            0,
            vec![zero, t, one],
        );
        b.op(Inst::CompareNotEqual, StorageKind::None, 0, vec![at, zero])
    } else {
        b.op(Inst::CompareNotEqual, StorageKind::None, 0, vec![t, zero])
    };
    let lp = b.s.info.ast.new_block(BlockType::DoWhile, Some(cond));
    for (k, op) in [(3, Inst::LoopBreak), (4, Inst::LoopContinue)] {
        let kk = b.k(k);
        let eq = b.op(Inst::CompareEqual, StorageKind::None, 0, vec![t, kk]);
        let c = b.def(AggType::BOOL, eq);
        let blk = b.s.info.ast.new_block(BlockType::If, Some(c));
        let jump = b.s.info.ast.new_simple_op(op, vec![]);
        b.s.info.ast.add(blk, jump).expect("add");
        b.s.info.ast.add(lp, blk).expect("add");
    }
    let st = b.op(Inst::Store, StorageKind::SharedMemory, 0, vec![zero, t, t]);
    b.s.info.ast.add(lp, st).expect("add");
    b.put(lp);
    b
}

#[test]
fn a_plain_condition_stays_a_do_while() {
    let m = msl(&spin(false));
    assert!(m.contains("} while ("), "{m}");
    assert!(!m.contains("while (true)"), "{m}");
    assert!(m.contains("break;"), "{m}");
}

#[test]
fn a_condition_with_an_atomic_leaves_the_loop_through_a_flag() {
    let m = msl(&spin(true));
    for want in [
        "while (true)",
        "bool lb",
        "} while (false);",
        "= true; break; }",
        "continue;",
        "atomic_fetch_add_explicit(",
    ] {
        assert!(m.contains(want), "missing {want}\n{m}");
    }
    assert!(m.contains("if (!("), "{m}");
}
