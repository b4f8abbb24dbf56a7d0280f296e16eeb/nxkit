//! `split` of the decoder.

use crate::decoder::{Block, PushOpInfo, SyncTarget, split};
use crate::names::InstName;
use crate::table::InstOp;

fn op(address: u64, name: InstName) -> InstOp {
    InstOp {
        address,
        name,
        ..InstOp::none()
    }
}

/// A block split after the push ops were propagated (a BRX target of a later round lands in front of a SSY): the
/// sync target that names the SSY follows it into the right block (issue 42, Xenoblade Chronicles 3: the emitter
/// read `push_ops[0]` of the left block, which had none left).
#[test]
fn a_split_moves_the_sync_targets_of_the_moved_push_ops() {
    let mut left = Block::new(0x08);
    left.ops = vec![
        op(0x08, InstName::Mov),
        op(0x10, InstName::Ssy),
        op(0x18, InstName::Bra),
    ];
    left.end_address = 0x20;
    left.push_ops = vec![PushOpInfo {
        op: left.ops[1],
        consumers: vec![(1, 1)],
    }];
    left.succs = vec![1];
    let mut consumer = Block::new(0x100);
    consumer.preds = vec![0];
    consumer.sync_targets = vec![(
        0x10,
        SyncTarget {
            push: (0, 0),
            push_op_id: 0,
        },
    )];
    let mut all = vec![left, consumer, Block::new(0x10)];
    split(&mut all, 0, 2).unwrap();
    assert!(all[0].push_ops.is_empty());
    assert_eq!(all[2].push_ops.len(), 1);
    assert_eq!(all[1].sync_targets[0].1.push, (2, 0));
    assert_eq!(all[1].preds, vec![2]);
    assert_eq!(all[0].succs, vec![2]);
    assert_eq!(all[2].succs, vec![1]);
}

/// A push op in front of the split point stays, and so does the sync target that names it.
#[test]
fn a_split_keeps_the_push_ops_in_front() {
    let mut left = Block::new(0x08);
    left.ops = vec![
        op(0x08, InstName::Ssy),
        op(0x10, InstName::Mov),
        op(0x18, InstName::Bra),
    ];
    left.end_address = 0x20;
    left.push_ops = vec![PushOpInfo {
        op: left.ops[0],
        consumers: vec![],
    }];
    let mut consumer = Block::new(0x100);
    consumer.sync_targets = vec![(
        0x08,
        SyncTarget {
            push: (0, 0),
            push_op_id: 0,
        },
    )];
    let mut all = vec![left, consumer, Block::new(0x10)];
    split(&mut all, 0, 2).unwrap();
    assert_eq!(all[0].push_ops.len(), 1);
    assert_eq!(all[1].sync_targets[0].1.push, (0, 0));
}
