//! Tests of the facts that were checked and found complete: `precise` min, max and clamp, and the border emulation.

use super::abi::{self, Kind};
use super::gaps_tests::{msl, translate};
use super::tests::{B, compute, fragment};
use crate::ir::agg::AggType;
use crate::ir::{Inst, IoVariable, StorageKind};

fn fp(i: Inst) -> Inst {
    Inst(i.0 | Inst::FP32)
}

/// A compute shader with the float and the integer forms of min, max and clamp.
fn min_max() -> B {
    let mut b = compute();
    b.io(StorageKind::Input, IoVariable::ThreadId, 0);
    let t = b.load_scalar(IoVariable::ThreadId, AggType::U32);
    let cv = b.op(Inst::ConvertU32ToFP32, StorageKind::None, 0, vec![t]);
    let f = b.def(AggType::FP32, cv);
    for i in [Inst::Minimum, Inst::Maximum] {
        let o = b.op(fp(i), StorageKind::None, 0, vec![f, f]);
        let _ = b.def(AggType::FP32, o);
        let o = b.op(i, StorageKind::None, 0, vec![t, t]);
        let _ = b.def(AggType::S32, o);
    }
    let o = b.op(fp(Inst::Clamp), StorageKind::None, 0, vec![f, f, f]);
    let _ = b.def(AggType::FP32, o);
    let o = b.op(Inst::Clamp, StorageKind::None, 0, vec![t, t, t]);
    let _ = b.def(AggType::S32, o);
    b
}

#[test]
fn float_min_max_and_clamp_take_the_ieee_forms() {
    let m = msl(&min_max());
    for want in ["precise::min(", "precise::max(", "precise::clamp("] {
        assert!(m.contains(want), "missing {want}\n{m}");
    }
    assert!(!m.contains("fast::"), "{m}");
    assert!(
        m.contains(" min(") || m.contains("= min("),
        "integer min\n{m}"
    );
    assert_eq!(m.matches("precise::min(").count(), 1);
}

#[test]
fn a_plain_sample_has_a_twin_sampler_and_a_border_bit() {
    let b = fragment();
    let t = translate(&b);
    let l = &t.libraries[0];
    let r = l.reflection.as_ref().expect("reflection");
    let twin = r
        .bindings
        .iter()
        .find(|x| x.msl_name.ends_with(abi::BORDER_TWIN))
        .expect("twin");
    assert_eq!((twin.kind, twin.border_bit), (Kind::SampTex, Some(0)));
    assert_eq!(twin.reg, Some(3));
    let blk = r
        .bindings
        .iter()
        .find(|x| x.msl_name == abi::BORDER_MEMBER)
        .expect("block");
    assert_eq!(blk.kind, Kind::Border);
    assert!(blk.arg_id > twin.arg_id);
    for want in [
        "struct dm_border_t",
        "dm_bs(",
        "spvDescriptorSet0.dm_border, 0u",
        "constant dm_border_t* dm_border [[id(",
    ] {
        assert!(l.msl.contains(want), "missing {want}\n{}", l.msl);
    }
}
