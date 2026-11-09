//! Tests of the texture and sampler pools and of the type 1 handle with a driver table sampler.

use super::abi::{self, Kind};
use super::gaps_tests::{msl, translate};
use super::tests::{B, F4};
use crate::attr::Stage;
use crate::driver::TexInfo;
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::props::TextureDef;
use crate::structured::ast::{AstTex, BlockType};
use crate::structured::types::AggType;
use crate::types::SamplerType;

/// A pixel shader that samples element `idx` of an array of `len` textures. `discard` adds a discard.
pub(super) fn pool_sample(
    len: i32,
    cbuf_slot: i64,
    handle: i64,
    separate: bool,
    discard: bool,
) -> B {
    let mut b = B::new(Stage::Fragment, "ps");
    b.s.props.definitions.imap_types = vec![[2, 2, 2, 2]];
    b.s.props.definitions.fragment_output_types = vec![F4, F4];
    b.io(StorageKind::Input, IoVariable::UserDefined, 0);
    b.io(StorageKind::Output, IoVariable::FragmentOutputColor, 1);
    let u = b.load_io(
        StorageKind::Input,
        IoVariable::UserDefined,
        Some(0),
        0,
        AggType::FP32,
    );
    let v = b.load_io(
        StorageKind::Input,
        IoVariable::UserDefined,
        Some(0),
        1,
        AggType::FP32,
    );
    let name = if cbuf_slot == 0 {
        "fp_t2d_cb0_40000000".to_string()
    } else {
        format!("fp_t2d_cb{cbuf_slot}_{handle:X}")
    };
    let def = |binding: i32, name: &str, ty: SamplerType, separate: bool| TextureDef {
        set: 0,
        binding,
        array_length: len,
        separate,
        name: name.into(),
        ty,
        format: 0,
        flags: 0,
    };
    let info = |binding: i64, ty: SamplerType, separate: bool| TexInfo {
        binding,
        ty: ty.0,
        format: 0,
        cbuf_slot,
        handle,
        array_length: i64::from(len),
        separate,
        flags: 0,
    };
    b.s.props
        .properties
        .textures
        .push(def(0, &name, SamplerType::TEXTURE_2D, separate));
    b.s.program
        .textures
        .push(info(0, SamplerType::TEXTURE_2D, separate));
    let mut tex = AstTex {
        ty: SamplerType::TEXTURE_2D,
        format: 0,
        flags: 0,
        set: 0,
        binding: 0,
        sampler_set: 0,
        sampler_binding: -1,
    };
    let idx = b.k(5);
    let mut srcs = vec![idx];
    if separate {
        b.s.props
            .properties
            .textures
            .push(def(1, "fp_s_cb0_40000000", SamplerType::NONE, false));
        b.s.program.textures.push(info(1, SamplerType::NONE, false));
        tex.sampler_binding = 1;
        srcs.push(b.k(6));
    }
    srcs.extend([u, v]);
    let s = b.s.info.ast.new_tex_op(Inst::TextureSample, tex, 15, srcs);
    let _ = b.def(F4, s);
    if discard {
        let lt = b.op(Inst::CompareLess, StorageKind::None, 0, vec![u, v]);
        let c = b.def(AggType::BOOL, lt);
        let blk = b.s.info.ast.new_block(BlockType::If, Some(c));
        let d = b.s.info.ast.new_simple_op(Inst::Discard, vec![]);
        b.s.info.ast.add(blk, d).expect("add");
        b.put(blk);
    }
    b.store_io(IoVariable::FragmentOutputColor, Some(1), 0, u);
    b
}

fn binding(b: &B, name: &str) -> super::refl::Binding {
    let t = translate(b);
    let r = t.libraries[0].reflection.clone().expect("reflection");
    r.bindings
        .iter()
        .find(|x| x.msl_name == name)
        .cloned()
        .expect("binding")
}

#[test]
fn a_driver_pool_is_a_pointer_with_a_clamped_read() {
    let b = pool_sample(1 << 20, 0, 0x4000_0000, false, false);
    let m = msl(&b);
    for want in [
        "struct spvPool_fp_t2d_cb0_40000000\n{\n    uint n;\n    ulong seen;\n    array<texture2d<float>, 1> e;\n};",
        "constant spvPool_fp_t2d_cb0_40000000* fp_t2d_cb0_40000000 [[id(0)]];",
        "constant spvPool_fp_t2d_cb0_40000000Smplr* fp_t2d_cb0_40000000Smplr [[id(1)]];",
        "dm_pool_mark(spvDescriptorSet0.fp_t2d_cb0_40000000, min(uint(5), spvDescriptorSet0.fp_t2d_cb0_40000000->n))",
        "(&spvDescriptorSet0.fp_t2d_cb0_40000000Smplr->e[0])[min(uint(5), spvDescriptorSet0.fp_t2d_cb0_40000000Smplr->n)]",
        "uint dm_pool_mark(constant P* p, uint i)",
    ] {
        assert!(m.contains(want), "missing {want}\n{m}");
    }
    assert!(!m.contains("array<texture2d<float>, 1048576>"));
    let t = binding(&b, "fp_t2d_cb0_40000000");
    assert_eq!(
        (t.kind, t.elem.as_deref()),
        (Kind::TexPool, Some("texture2d<float>"))
    );
    let s = binding(&b, "fp_t2d_cb0_40000000Smplr");
    assert_eq!(
        (s.kind, s.elem.as_deref()),
        (Kind::SampPool, Some("sampler"))
    );
}

#[test]
fn argument_ids_count_a_pointer_once() {
    let b = pool_sample(64, 0, 0x4000_0000, false, false);
    let t = translate(&b);
    let r = t.libraries[0].reflection.clone().expect("reflection");
    let ids: Vec<u32> = r.bindings.iter().map(|x| x.arg_id).collect();
    // The texture pool, the sampler pool and the blend word of the pixel shader.
    assert_eq!(ids, vec![0, 1, 2]);
}

#[test]
fn a_short_array_keeps_its_slots() {
    let m = msl(&pool_sample(32, 0, 0x4000_0000, false, false));
    assert!(m.contains("array<texture2d<float>, 32> fp_t2d_cb0_40000000 [[id(0)]]"));
    assert!(!m.contains("spvPool_"));
}

#[test]
fn a_table_of_guest_handles_names_its_buffer_and_word() {
    let b = pool_sample(64, 7, 0x2a, false, false);
    let t = binding(&b, "fp_t2d_cb7_2A");
    assert_eq!((t.kind, t.reg, t.off), (Kind::TexHtab, Some(4), Some(0xa8)));
    assert_eq!(t.elem.as_deref(), Some("texture2d<float>"));
    let s = binding(&b, "fp_t2d_cb7_2ASmplr");
    assert_eq!(
        (s.kind, s.reg, s.off),
        (Kind::SampHtab, Some(4), Some(0xa8))
    );
}

#[test]
fn a_table_with_a_driver_table_sampler_names_the_sampler_word() {
    let h = (1i64 << 28) | (0x5c << 14) | 0x2a;
    let b = pool_sample(64, 6 | (3 << 16), h, false, false);
    let name = format!("fp_t2d_cb196614_{h:X}");
    let t = binding(&b, &name);
    assert_eq!(
        (t.kind, t.soff, t.sreg),
        (Kind::TexHtab, Some(0x170), Some(abi::SREG_C2))
    );
    let s = binding(&b, &format!("{name}Smplr"));
    assert_eq!((s.kind, s.off), (Kind::SampHtab, Some(0xa8)));
    assert_eq!((s.soff, s.sreg), (Some(0x170), Some(abi::SREG_C2)));
}

#[test]
fn a_separate_sampler_pool_is_not_marked() {
    let b = pool_sample(64, 0, 0x4000_0000, true, false);
    let m = msl(&b);
    assert!(m.contains("constant spvPool_fp_s_cb0_40000000* fp_s_cb0_40000000 [[id(1)]];"));
    assert!(m.contains("(&spvDescriptorSet0.fp_s_cb0_40000000->e[0])[min(uint(6), spvDescriptorSet0.fp_s_cb0_40000000->n)]"));
    assert_eq!(binding(&b, "fp_s_cb0_40000000").kind, Kind::SampPool);
}

#[test]
fn the_mark_makes_the_depth_test_early() {
    let m = msl(&pool_sample(64, 0, 0x4000_0000, false, false));
    assert!(m.contains("[[early_fragment_tests]] fragment "), "{m}");
    let m = msl(&pool_sample(64, 0, 0x4000_0000, false, true));
    assert!(!m.contains("early_fragment_tests"), "{m}");
    let m = msl(&pool_sample(32, 0, 0x4000_0000, false, false));
    assert!(!m.contains("early_fragment_tests"), "{m}");
}

#[test]
fn a_forced_early_test_and_a_mark_write_the_attribute_once_per_entry() {
    let mut b = pool_sample(64, 0, 0x4000_0000, false, false);
    b.s.props.definitions.early_z_force = true;
    let m = msl(&b);
    let entries: Vec<&str> = m.lines().filter(|l| l.contains(" fragment ")).collect();
    assert!(!entries.is_empty());
    for l in entries {
        assert_eq!(l.matches("[[early_fragment_tests]]").count(), 1, "{l}");
    }
    let mut b = pool_sample(32, 0, 0x4000_0000, false, false);
    b.s.props.definitions.early_z_force = true;
    assert!(msl(&b).contains("[[early_fragment_tests]] fragment "));
}
