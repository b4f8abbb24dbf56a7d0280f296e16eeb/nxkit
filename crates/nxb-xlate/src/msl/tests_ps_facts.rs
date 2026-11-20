//! Tests of the pixel facts and the pixel engine items: a discard inside a helper, the helper invocation, the
//! fragment shader interlock and the raster facts.

use super::tests::{B, F4, fragment};
use super::{Context, Translated};
use crate::attr::Stage;
use crate::driver::TexInfo;
use crate::ir::agg::AggType;
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::props::TextureDef;
use crate::structured::StructFunction;
use crate::structured::ast::{AstTex, BlockType};
use crate::types::SamplerType;

fn translated(b: &B) -> Translated {
    super::library::build(&b.s, &Context::default(), &Default::default()).expect("build")
}

/// A pixel shader that calls the function 1, which discards when `discard` is set.
fn with_helper(discard: bool) -> B {
    let mut b = fragment();
    let main = b.s.info.ast.new_block(BlockType::Main, None);
    if discard {
        let d = b.s.info.ast.new_simple_op(Inst::Discard, vec![]);
        b.s.info.ast.add(main, d).expect("add");
    }
    b.s.info.functions.push(StructFunction {
        main_block: main,
        name: "fn1".into(),
        return_type: AggType::VOID,
        in_arguments: vec![],
        out_arguments: vec![],
        locals: vec![],
    });
    let k = b.k(1);
    let call = b.op(Inst::Call, StorageKind::None, 0, vec![k]);
    b.put(call);
    b
}

#[test]
fn a_discard_in_a_helper_stops_the_main_function() {
    let t = translated(&with_helper(true));
    let msl = &t.libraries[0].msl;
    for want in [
        "static main0_out dm_body_main0(",
        "void fn1(constant spvDescriptorSetBuffer0& spvDescriptorSet0, thread main0_in& in, thread main0_out& out, thread bool& dm_kill)",
        "bool dm_kill = false;",
        "dm_kill = true;",
        "fn1(spvDescriptorSet0, in, out, dm_kill);",
        "if (dm_kill) {",
    ] {
        assert!(msl.contains(want), "missing {want}\n{msl}");
    }
    let r = t.libraries[0].reflection.as_ref().expect("r");
    assert_eq!(r.raster.discard, Some(true));
}

#[test]
fn a_helper_that_does_not_discard_gets_no_flag() {
    let t = translated(&with_helper(false));
    assert!(!t.libraries[0].msl.contains("dm_kill"));
}

#[test]
fn the_raster_facts_of_a_pixel_shader() {
    let mut b = fragment();
    b.io(StorageKind::Input, IoVariable::PrimitiveId, 0);
    let id = b.load_scalar(IoVariable::PrimitiveId, AggType::U32);
    b.store_io(IoVariable::FragmentOutputColor, Some(1), 1, id);
    let t = translated(&b);
    let r = &t.libraries[0].reflection.as_ref().expect("r").raster;
    assert_eq!(r.sample_mask, Some(false));
    assert!(r.primitive_id);
    assert_eq!(r.colors, Some(vec![0, 1]));
    let v = t.libraries[0].reflection.as_ref().expect("r").to_value();
    let raster = v.get("raster").expect("raster");
    assert_eq!(raster.get("sample_mask").and_then(|x| x.as_i64()), Some(0));
}

#[test]
fn a_shader_without_a_discard_or_write_may_not_discard() {
    let mut b = B::new(Stage::Fragment, "ps");
    b.s.props.definitions.fragment_output_types = vec![F4];
    b.io(StorageKind::Output, IoVariable::FragmentOutputColor, 0);
    let one = b.k(0x3f80_0000);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, one);
    let t = translated(&b);
    let r = &t.libraries[0].reflection.as_ref().expect("r").raster;
    assert_eq!(r.discard, Some(false));
}

#[test]
fn the_helper_invocation_is_a_metal_query() {
    let mut b = fragment();
    b.io(StorageKind::Input, IoVariable::HelperInvocation, 0);
    let h = b.load_scalar(IoVariable::HelperInvocation, AggType::BOOL);
    let blk = b.s.info.ast.new_block(BlockType::If, Some(h));
    let d = b.s.info.ast.new_simple_op(Inst::Discard, vec![]);
    b.s.info.ast.add(blk, d).expect("add");
    b.put(blk);
    let t = translated(&b);
    let msl = &t.libraries[0].msl;
    assert!(msl.contains("simd_is_helper_thread()"), "{msl}");
    assert!(!msl.contains("HelperInvocation"));
}

/// A pixel shader that writes the image `fp_i_0`, between a begin and an end of the interlock when `fsi` is set.
fn with_image(fsi: bool) -> B {
    let mut b = fragment();
    b.s.props.properties.images.push(TextureDef {
        set: 0,
        binding: 0,
        array_length: 1,
        separate: false,
        name: "fp_i_0".into(),
        ty: SamplerType::TEXTURE_2D,
        format: 0,
        flags: 0,
    });
    b.s.program.images.push(TexInfo {
        binding: 0,
        ty: SamplerType::TEXTURE_2D.0,
        format: 0,
        cbuf_slot: -1,
        handle: i64::from(super::abi::C2_IMG / 4),
        array_length: 1,
        separate: false,
        flags: 0,
    });
    let tex = AstTex {
        ty: SamplerType::TEXTURE_2D,
        format: 0,
        flags: 0,
        set: 0,
        binding: 0,
        sampler_set: 0,
        sampler_binding: -1,
    };
    if fsi {
        let begin = b.op(Inst::FSIBegin, StorageKind::None, 0, vec![]);
        b.put(begin);
    }
    let (x, y, one) = (b.k(0), b.k(0), b.k(0x3f80_0000));
    let st =
        b.s.info
            .ast
            .new_tex_op(Inst::ImageStore, tex, 15, vec![x, y, one, one, one, one]);
    b.put(st);
    if fsi {
        let end = b.op(Inst::FSIEnd, StorageKind::None, 0, vec![]);
        b.put(end);
    }
    b
}

#[test]
fn the_interlock_orders_the_images_a_pixel_shader_writes() {
    let t = translated(&with_image(true));
    let msl = &t.libraries[0].msl;
    assert!(
        msl.contains("fp_i_0 [[id(2), raster_order_group(0)]];"),
        "{msl}"
    );
    let r = t.libraries[0].reflection.as_ref().expect("r");
    assert!(r.bindings.iter().any(|x| x.msl_name == "fp_i_0"));
}

#[test]
fn an_image_outside_an_interlock_has_no_raster_order_group() {
    let t = translated(&with_image(false));
    assert!(!t.libraries[0].msl.contains("raster_order_group"));
}

#[test]
fn early_fragment_tests_mark_every_pixel_entry() {
    let mut b = fragment();
    b.s.props.definitions.early_z_force = true;
    let t = translated(&b);
    assert!(
        t.libraries[0]
            .msl
            .contains("[[early_fragment_tests]] fragment main0_out main0(")
    );
}

#[test]
#[ignore]
fn metal_builds_the_pixel_engine_items() {
    for (name, b) in [
        ("ps_helper_discard", with_helper(true)),
        ("ps_interlock", with_image(true)),
    ] {
        super::tests_ps::metal_text(name, &translated(&b).libraries[0].msl);
    }
    let mut b = fragment();
    b.io(StorageKind::Input, IoVariable::HelperInvocation, 0);
    let h = b.load_scalar(IoVariable::HelperInvocation, AggType::BOOL);
    let blk = b.s.info.ast.new_block(BlockType::If, Some(h));
    let d = b.s.info.ast.new_simple_op(Inst::Discard, vec![]);
    b.s.info.ast.add(blk, d).expect("add");
    b.put(blk);
    super::tests_ps::metal_text("ps_helper", &translated(&b).libraries[0].msl);
}
