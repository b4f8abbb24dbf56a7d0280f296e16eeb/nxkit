//! Tests of the `_bf` library: the framebuffer fetch variant of a pixel shader.

use super::emit::Options;
use super::library::build;
use super::tests::{B, F4, fragment};
use super::{Context, Translated};
use crate::attr::Stage;
use crate::driver::TexInfo;
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::props::{BufferDef, Field, TextureDef};
use crate::structured::ast::{A, AstTex};
use crate::structured::types::AggType;
use crate::types::SamplerType;
use nxb_pyjson::Value;

const ONE: i32 = 0x3f80_0000;

fn fp(i: Inst) -> Inst {
    Inst(i.0 | Inst::FP32)
}

/// A pixel shader with one colour, the texture `fp_t2d_3` and the varying 0.
fn base() -> B {
    let mut b = B::new(Stage::Fragment, "ps");
    b.s.props.definitions.imap_types = vec![[2, 2, 2, 2]];
    b.s.props.definitions.fragment_output_types = vec![F4];
    b.io(StorageKind::Input, IoVariable::UserDefined, 0);
    b.io(StorageKind::Output, IoVariable::FragmentOutputColor, 0);
    b.s.props.properties.textures.push(TextureDef {
        set: 0,
        binding: 0,
        array_length: 1,
        separate: false,
        name: "fp_t2d_3".into(),
        ty: SamplerType::TEXTURE_2D,
        format: 0,
        flags: 0,
    });
    b.s.program.textures.push(TexInfo {
        binding: 0,
        ty: SamplerType::TEXTURE_2D.0,
        format: 0,
        cbuf_slot: -1,
        handle: i64::from((crate::msl::abi::C2_TEX + 3 * crate::msl::abi::C2_ROW) / 4),
        array_length: 1,
        separate: false,
        flags: 0,
    });
    b
}

fn varying(b: &mut B, comp: i32) -> A {
    b.load_io(
        StorageKind::Input,
        IoVariable::UserDefined,
        Some(0),
        comp,
        AggType::FP32,
    )
}

/// A sample of the texture, stored to the colour.
fn sample(b: &mut B, x: A, y: A, flags: u32) {
    let tex = AstTex {
        ty: SamplerType::TEXTURE_2D,
        format: 0,
        flags,
        set: 0,
        binding: 0,
        sampler_set: 0,
        sampler_binding: -1,
    };
    let s =
        b.s.info
            .ast
            .new_tex_op(Inst::TextureSample, tex, 15, vec![x, y]);
    let _ = b.def(F4, s);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, x);
}

fn bin(b: &mut B, inst: Inst, x: A, y: A) -> A {
    let op = b.op(fp(inst), StorageKind::None, 0, vec![x, y]);
    b.def(AggType::FP32, op)
}

fn translated(b: &B, opt: &Options) -> Translated {
    build(&b.s, &Context::default(), opt).expect("build")
}

fn fetch_meta(t: &Translated) -> Value {
    let r = t.libraries[0].reflection.as_ref().expect("reflection");
    r.to_value().get("fetch").expect("fetch field").clone()
}

fn calls(t: &Translated) -> Vec<Value> {
    match fetch_meta(t).get("calls") {
        Some(Value::List(l)) => l.clone(),
        other => panic!("calls: {other:?}"),
    }
}

fn int_of(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).expect(k)
}

fn tex_reg(t: &Translated) -> i64 {
    let r = t.libraries[0].reflection.as_ref().expect("reflection");
    r.bindings
        .iter()
        .find(|b| b.msl_name == "fp_t2d_3")
        .and_then(|b| b.reg)
        .expect("reg")
}

#[test]
fn a_varying_sample_is_rewritten() {
    let t = translated(&fragment(), &Options::default());
    assert_eq!(t.libraries.len(), 2);
    let l = &t.libraries[1];
    assert_eq!(l.suffix, "_bf");
    assert!(l.reflection.is_none());
    for want in [
        "constant int FX_0 [[function_constant(0)]];",
        "constant bool FB_7 [[function_constant(39)]];",
        "constant bool OM_0_7 [[function_constant(71)]];",
        "FX_0 == 0 ? fb0 : FX_0 == 1 ? fb1 :",
        "static main0_out dm_body_main0(main0_in in, constant spvDescriptorSetBuffer0& spvDescriptorSet0, float4 fb0,",
        "struct main0_out_f",
        "float4 c1_at3 [[color(3), function_constant(OM_0_3)]];",
        "fragment main0_out_f main0_f(main0_in in [[stage_in]], constant spvDescriptorSetBuffer0& spvDescriptorSet0 [[buffer(0)]], float4 fb0 [[color(0), function_constant(FB_0)]],",
        "dm_body_main0(in, spvDescriptorSet0, (FB_0 ? fb0 : float4(0.0)),",
        "if (OM_0_3) out.c1_at3 = dm_r.c1;",
    ] {
        assert!(l.msl.contains(want), "missing {want}\n{}", l.msl);
    }
    let c = calls(&t);
    assert_eq!(c.len(), 1);
    assert_eq!(int_of(&c[0], "fc"), 0);
    assert_eq!(int_of(&c[0], "tex_reg"), tex_reg(&t));
    assert_eq!(int_of(&c[0], "varying_locn"), 0);
    assert!(c[0].get("proj").is_none());
    let m = fetch_meta(&t);
    assert_eq!(m.get("mixed_regs"), Some(&Value::List(vec![])));
    let o = m.get("outputs").and_then(Value::as_list).expect("outputs");
    assert_eq!(o.len(), 1);
    assert_eq!(int_of(&o[0], "color"), 1);
    assert_eq!(o[0].get("type").and_then(Value::as_str), Some("float4"));
}

#[test]
fn the_base_library_keeps_the_plain_sample() {
    let t = translated(&fragment(), &Options::default());
    assert!(!t.libraries[0].msl.contains("FX_0"));
    assert!(!t.libraries[0].msl.contains("main0_out_f"));
}

#[test]
fn a_perspective_divided_varying_is_marked_proj() {
    let mut b = base();
    let (u, v, w) = (varying(&mut b, 0), varying(&mut b, 1), varying(&mut b, 3));
    let one = b.k(ONE);
    let r = bin(&mut b, Inst::Divide, one, w);
    let x = bin(&mut b, Inst::Multiply, u, r);
    let y = bin(&mut b, Inst::Multiply, r, v);
    sample(&mut b, x, y, 0);
    let t = translated(&b, &Options::default());
    let c = calls(&t);
    assert_eq!(c.len(), 1, "{}", t.libraries[0].msl);
    assert_eq!(int_of(&c[0], "varying_locn"), 0);
    assert_eq!(int_of(&c[0], "proj"), 1);
}

#[test]
fn a_division_by_w_is_marked_proj() {
    let mut b = base();
    let (u, v, w) = (varying(&mut b, 0), varying(&mut b, 1), varying(&mut b, 3));
    let x = bin(&mut b, Inst::Divide, u, w);
    let y = bin(&mut b, Inst::Divide, v, w);
    sample(&mut b, x, y, 0);
    let c = calls(&translated(&b, &Options::default()));
    assert_eq!(c.len(), 1);
    assert_eq!(int_of(&c[0], "proj"), 1);
}

fn uniform(b: &mut B, vec: i32, comp: i32) -> A {
    let src = vec![b.k(3), b.k(0), b.k(vec), b.k(comp)];
    let op = b.op(Inst::Load, StorageKind::ConstantBuffer, 0, src);
    b.def(AggType::FP32, op)
}

fn frag_pos_program(scaled: bool) -> B {
    let mut b = base();
    b.io(StorageKind::Input, IoVariable::FragmentCoord, 0);
    b.s.props.properties.constant_buffers.push(BufferDef {
        layout: 0,
        set: 0,
        binding: 3,
        name: "fp_c3".into(),
        fields: vec![Field {
            ty: AggType::ARRAY | AggType::VECTOR4 | AggType::FP32,
            array_length: 4096,
            name: "f".into(),
        }],
    });
    let mut pix = Vec::new();
    for comp in 0..2 {
        let p = b.load_io(
            StorageKind::Input,
            IoVariable::FragmentCoord,
            None,
            comp,
            AggType::FP32,
        );
        pix.push(if scaled {
            let src = vec![b.k(0), b.k(5), b.k(0)];
            let op = b.op(Inst::Load, StorageKind::ConstantBuffer, 0, src);
            let sc = b.def(AggType::FP32, op);
            bin(&mut b, Inst::Divide, p, sc)
        } else {
            p
        });
    }
    let (ux, uy) = (uniform(&mut b, 1, 2), uniform(&mut b, 1, 3));
    let x = bin(&mut b, Inst::Multiply, pix[0], ux);
    let y = bin(&mut b, Inst::Multiply, uy, pix[1]);
    sample(&mut b, x, y, 0);
    b
}

fn support_buffer(b: &mut B) {
    let scalar = |n: &str, ty: AggType| Field {
        ty,
        array_length: 1,
        name: n.into(),
    };
    b.s.props.properties.constant_buffers.push(BufferDef {
        layout: 0,
        set: 0,
        binding: 0,
        name: "support_buffer".into(),
        fields: vec![
            scalar("alpha_test", AggType::U32),
            scalar("is_bgra", AggType::ARRAY | AggType::U32),
            scalar("viewport_inverse", AggType::VECTOR4 | AggType::FP32),
            scalar("viewport_size", AggType::VECTOR4 | AggType::FP32),
            scalar("frag_scale_count", AggType::S32),
            Field {
                ty: AggType::ARRAY | AggType::FP32,
                array_length: 73,
                name: "render_scale".into(),
            },
        ],
    });
}

fn frag_pos_value(x: [i64; 3], y: [i64; 3]) -> Value {
    let l = |v: [i64; 3]| Value::List(v.iter().map(|n| Value::Int(*n)).collect());
    Value::List(vec![l(x), l(y)])
}

#[test]
fn a_pixel_position_times_uniforms_gives_frag_pos() {
    let t = translated(&frag_pos_program(false), &Options::default());
    let c = calls(&t);
    assert_eq!(c.len(), 1, "{}", t.libraries[0].msl);
    assert_eq!(
        c[0].get("frag_pos"),
        Some(&frag_pos_value([0, 1, 2], [0, 1, 3]))
    );
}

#[test]
fn a_render_scaled_pixel_position_gives_frag_pos() {
    let mut b = frag_pos_program(true);
    support_buffer(&mut b);
    let t = translated(&b, &Options::default());
    let c = calls(&t);
    assert_eq!(c.len(), 1, "{}", t.libraries[0].msl);
    assert_eq!(
        c[0].get("frag_pos"),
        Some(&frag_pos_value([0, 1, 2], [0, 1, 3]))
    );
}

#[test]
fn other_coordinates_are_not_rewritten() {
    let mut b = base();
    let (u, v) = (varying(&mut b, 0), varying(&mut b, 1));
    let x = bin(&mut b, Inst::Add, u, v);
    sample(&mut b, x, v, 0);
    let t = translated(&b, &Options::default());
    assert_eq!(t.libraries.len(), 1);
    assert!(
        t.libraries[0]
            .reflection
            .as_ref()
            .expect("r")
            .to_value()
            .get("fetch")
            .is_none()
    );
}

#[test]
fn a_sample_with_bias_is_not_rewritten_and_marks_the_register_mixed() {
    let mut b = base();
    let (u, v) = (varying(&mut b, 0), varying(&mut b, 1));
    sample(&mut b, u, v, 0);
    let bias = bin(&mut b, Inst::Add, u, v);
    let tex = AstTex {
        ty: SamplerType::TEXTURE_2D,
        format: 0,
        flags: 1 << 4,
        set: 0,
        binding: 0,
        sampler_set: 0,
        sampler_binding: -1,
    };
    let s =
        b.s.info
            .ast
            .new_tex_op(Inst::TextureSample, tex, 15, vec![u, v, bias]);
    let _ = b.def(F4, s);
    let t = translated(&b, &Options::default());
    assert_eq!(calls(&t).len(), 1);
    let m = fetch_meta(&t);
    assert_eq!(
        m.get("mixed_regs"),
        Some(&Value::List(vec![Value::Int(tex_reg(&t))]))
    );
}

#[test]
fn a_shader_without_a_sample_gets_a_variant_only_when_listed() {
    let mut b = base();
    let u = varying(&mut b, 0);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, u);
    assert_eq!(translated(&b, &Options::default()).libraries.len(), 1);
    let opt = Options {
        fetch_outputs_only: true,
        ..Options::default()
    };
    let t = translated(&b, &opt);
    assert_eq!(t.libraries.len(), 2);
    assert!(calls(&t).is_empty());
    assert!(t.libraries[1].msl.contains("struct main0_out_f"));
}

#[test]
fn depth_output_refuses_the_variant_with_a_note() {
    let mut b = fragment();
    b.io(StorageKind::Output, IoVariable::FragmentOutputDepth, 0);
    let u = varying(&mut b, 0);
    b.store_io(IoVariable::FragmentOutputDepth, None, 0, u);
    let t = translated(&b, &Options::default());
    assert_eq!(t.libraries.len(), 1);
    let r = t.libraries[0].reflection.as_ref().expect("r");
    assert!(r.notes.iter().any(|n| n.starts_with("no fetch variant")));
}

#[test]
fn the_vertex_and_compute_stages_have_no_fetch_variant() {
    let t = translated(&super::tests::vertex(), &Options::default());
    assert!(t.libraries.iter().all(|l| l.suffix != "_bf"));
}

#[test]
#[ignore]
fn metal_builds_the_fetch_variant() {
    let t = translated(&fragment(), &Options::default());
    super::tests_ps::metal_text("fetch_bf", &t.libraries[1].msl);
    let b = frag_pos_program(false);
    let t = translated(&b, &Options::default());
    super::tests_ps::metal_text("fetch_bf_pos", &t.libraries[1].msl);
}

#[test]
fn a_scaled_pixel_position_marks_the_pass_scale() {
    let mut b = frag_pos_program(true);
    support_buffer(&mut b);
    let t = translated(&b, &Options::default());
    let r = t.libraries[0].reflection.as_ref().expect("reflection");
    assert!(r.pass_scale, "{}", t.libraries[0].msl);
    assert_eq!(r.to_value().get("pass_scale"), Some(&Value::Bool(true)));
    let plain = translated(&frag_pos_program(false), &Options::default());
    let r = plain.libraries[0].reflection.as_ref().expect("reflection");
    assert!(!r.pass_scale);
    assert!(r.to_value().get("pass_scale").is_none());
}

#[test]
fn a_scaled_texture_binding_names_its_render_scale_entry() {
    let mut b = base();
    let (u, v) = (varying(&mut b, 0), varying(&mut b, 1));
    sample(&mut b, u, v, 0);
    b.s.program.scaled.push(crate::xform::helper::ScaleUse {
        image: false,
        binding: 0,
        index: 2,
    });
    let t = translated(&b, &Options::default());
    let r = t.libraries[0].reflection.as_ref().expect("reflection");
    let tex = r.bindings.iter().find(|b| b.msl_name == "fp_t2d_3").expect("tex");
    assert_eq!(tex.rscale, Some(3));
    assert_eq!(tex.to_value().get("rscale").and_then(Value::as_i64), Some(3));
}

#[test]
fn the_support_buffer_constants_match_the_front_end() {
    use crate::msl::abi::{SUPPORT_RENDER_SCALE, SUPPORT_RENDER_SCALES, SUPPORT_SIZE};
    use crate::translation::props::support_buffer as sb;
    assert_eq!(SUPPORT_RENDER_SCALE as i32, sb::GRAPHICS_RENDER_SCALE_OFFSET);
    assert_eq!(SUPPORT_RENDER_SCALES as i32, sb::RENDER_SCALE_MAX_COUNT);
    assert_eq!(SUPPORT_SIZE as i32, sb::REQUIRED_SIZE);
}
