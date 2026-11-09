//! Tests of the pixel entries: the side units, the derived entries and the pixel facts.

use super::tests::{B, fragment};
use super::{Side, Translated};
use nxb_pyjson::Value;

fn reflection(t: &Translated) -> Value {
    t.libraries[0]
        .reflection
        .as_ref()
        .expect("reflection")
        .to_value()
}

fn names(v: &Value, key: &str) -> Vec<String> {
    let e = v.get(key).and_then(|a| a.get("entries")).expect(key);
    match e {
        Value::List(l) => l
            .iter()
            .map(|n| n.as_str().expect("name").to_string())
            .collect(),
        _ => panic!("entries are a list"),
    }
}

/// `fragment()` with the colour 0 pad as its only colour 0.
fn side_sample() -> (B, Translated) {
    let b = fragment();
    let t = b.translate();
    (b, t)
}

#[test]
fn side_entries_wrap_the_body() {
    let (_, t) = side_sample();
    let msl = &t.libraries[0].msl;
    assert!(msl.contains("static main0_out dm_body_main0("));
    assert!(msl.contains("fragment main0_out main0("));
    assert!(msl.contains("fragment main0_out main0_a("));
    assert!(msl.contains("[[buffer(30)]]"));
    assert!(
        msl.contains("if (!dm_alpha_pass(float(dm_r.c0_pad.w), dm_alpha)) discard_fragment();")
    );
    assert!(msl.contains("fragment main0_out main0_l("));
    assert!(msl.contains("constant bool dm_lfb0 [[function_constant(200)]];"));
    assert!(msl.contains("constant bool dm_lfb1 [[function_constant(201)]];"));
    assert!(msl.contains("float4 dm_fb1 [[color(1), function_constant(dm_lfb1)]]"));
    assert!(msl.contains(
        "if (dm_lfb1) dm_r.c1 = float4(dm_logic_norm(float4(dm_r.c1), dm_fb1, dm_lop, 1u));"
    ));
    assert!(msl.contains("constant dm_logic_state& dm_lop [[buffer(29)]]"));
}

#[test]
fn side_names_and_fields() {
    let (_, t) = side_sample();
    assert_eq!(
        t.side,
        Some(Side {
            alpha: vec!["main0_a".into()],
            logic: vec!["main0_l".into()],
        })
    );
    let r = reflection(&t);
    assert_eq!(names(&r, "alpha"), ["main0_a"]);
    assert_eq!(names(&r, "logic"), ["main0_l"]);
}

#[test]
fn a_shader_without_colours_has_no_side_entries() {
    let mut b = B::new(crate::attr::Stage::Fragment, "ps");
    b.s.props.definitions.imap_types = vec![[2, 2, 2, 2]];
    let t = b.translate();
    let msl = &t.libraries[0].msl;
    assert!(!msl.contains("dm_body_"));
    assert!(t.side.is_none());
    let r = reflection(&t);
    assert!(r.get("alpha").is_none());
    assert!(r.get("logic").is_none());
}

#[test]
fn the_early_attribute_goes_to_every_entry() {
    let mut b = fragment();
    b.s.props.definitions.early_z_force = true;
    let msl = b.msl();
    let n = msl.matches("[[early_fragment_tests]] fragment").count();
    assert_eq!(n, 3, "{msl}");
}

#[test]
fn the_side_entries_are_entry_functions() {
    let (_, t) = side_sample();
    let s = t.side.expect("side");
    let all: Vec<&str> = s.alpha.iter().chain(&s.logic).map(String::as_str).collect();
    for n in all {
        assert!(
            t.libraries[0]
                .msl
                .contains(&format!("fragment main0_out {n}("))
        );
    }
}

/// Compile a translation with metal and make a library of it. The bytes of the library.
fn metal_library(name: &str, t: &Translated) -> Vec<u8> {
    metal_text(name, &t.libraries[0].msl)
}

/// Compile MSL text with metal and make a library of it. The bytes of the library.
pub(super) fn metal_text(name: &str, msl: &str) -> Vec<u8> {
    use std::ffi::OsStr;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scratch");
    std::fs::create_dir_all(&dir).expect("scratch");
    let src = dir.join(format!("{name}.metal"));
    let air = dir.join(format!("{name}.air"));
    let lib = dir.join(format!("{name}.metallib"));
    std::fs::write(&src, msl).expect("write");
    let run = |args: &[&OsStr]| {
        let o = std::process::Command::new("xcrun")
            .args(["-sdk", "macosx"])
            .args(args)
            .output()
            .expect("run");
        assert!(
            o.status.success(),
            "{name}: {}\n{}",
            String::from_utf8_lossy(&o.stderr),
            msl
        );
    };
    run(&[
        OsStr::new("metal"),
        OsStr::new("-c"),
        OsStr::new("-O1"),
        OsStr::new("-std=metal3.0"),
        src.as_os_str(),
        OsStr::new("-o"),
        air.as_os_str(),
    ]);
    run(&[
        OsStr::new("metallib"),
        air.as_os_str(),
        OsStr::new("-o"),
        lib.as_os_str(),
    ]);
    std::fs::read(&lib).expect("lib")
}

#[test]
#[ignore]
fn metal_builds_the_side_entries() {
    let (_, t) = side_sample();
    // `nxb-shader` (the cut) is not a dependency of this crate: the library keeps every entry name for it.
    let bytes = metal_library("side_ps", &t);
    let side = t.side.expect("side");
    for n in side.alpha.iter().chain(&side.logic) {
        let at = |w: &[u8]| bytes.windows(w.len()).any(|x| x == w);
        assert!(at(n.as_bytes()), "{n} is a function of the library");
    }
}

use super::tests::F4;
use crate::attr::Stage;
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::structured::types::AggType;

/// A pixel shader with `n` float colours and no stores.
fn colors(n: usize) -> B {
    let mut b = B::new(Stage::Fragment, "ps");
    b.s.props.definitions.imap_types = vec![[2, 2, 2, 2]];
    b.s.props.definitions.fragment_output_types = vec![F4; n];
    for l in 0..n {
        b.io(
            StorageKind::Output,
            IoVariable::FragmentOutputColor,
            l as i32,
        );
    }
    b
}

fn float_in(b: &mut B) -> crate::structured::ast::A {
    b.io(StorageKind::Input, IoVariable::UserDefined, 0);
    b.load_io(
        StorageKind::Input,
        IoVariable::UserDefined,
        Some(0),
        0,
        AggType::FP32,
    )
}

/// An integer value computed from the primitive id.
fn int_value(b: &mut B) -> crate::structured::ast::A {
    b.io(StorageKind::Input, IoVariable::PrimitiveId, 0);
    let id = b.load_scalar(IoVariable::PrimitiveId, AggType::S32);
    let one = b.k(1);
    let add = b.op(Inst::Add, StorageKind::None, 0, vec![id, one]);
    b.def(AggType::S32, add)
}

fn entry_of(t: &Translated, key: &str) -> Option<Value> {
    reflection(t).get(key).cloned()
}

#[test]
fn a_plain_float_shader_has_no_derived_entry() {
    let mut b = colors(1);
    let f = float_in(&mut b);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, f);
    let t = b.translate();
    assert!(!t.libraries[0].msl.contains("main0_u"));
    assert!(entry_of(&t, "entry_u").is_none());
}

#[test]
fn integer_bits_give_the_integer_entry() {
    let mut b = colors(1);
    let v = int_value(&mut b);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, v);
    let t = b.translate();
    let msl = &t.libraries[0].msl;
    assert!(msl.contains("struct main0_out_u"), "{msl}");
    assert!(msl.contains("uint4 c0 [[color(0)]];"));
    assert!(msl.contains("out.c0 = as_type<uint4>(dm_r.c0);"));
    assert!(msl.contains("fragment main0_out_u main0_u("));
    assert!(msl.contains("fragment main0_out_u main0_u_a("));
    assert_eq!(entry_of(&t, "entry_u"), Some(Value::str("main0_u")));
    assert!(entry_of(&t, "entry_m").is_none());
    let side = t.side.expect("side");
    assert!(side.alpha.contains(&"main0_u_a".to_string()));
    assert!(msl.contains("as_type<float>(dm_r.c0.w)"));
}

#[test]
fn a_mixed_shader_gets_the_mixed_entry_and_its_mask() {
    let mut b = colors(2);
    let f = float_in(&mut b);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, f);
    let v = int_value(&mut b);
    b.store_io(IoVariable::FragmentOutputColor, Some(1), 0, v);
    let t = b.translate();
    let msl = &t.libraries[0].msl;
    assert!(msl.contains("struct main0_out_m"), "{msl}");
    assert_eq!(entry_of(&t, "entry_m"), Some(Value::str("main0_m")));
    assert_eq!(entry_of(&t, "uint_mask_m"), Some(Value::Int(2)));
    let logic = names(&reflection(&t), "logic");
    assert!(logic.contains(&"main0_m_l".to_string()));
    assert!(!logic.iter().any(|n| n == "main0_d_l"));
}

#[test]
fn constant_colours_give_the_integer_entry() {
    let mut b = colors(1);
    let k = b.k(0x3f80_0000);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, k);
    let t = b.translate();
    assert!(t.libraries[0].msl.contains("main0_u"));
    assert!(entry_of(&t, "entry_m").is_none());
}

#[test]
fn the_dual_entry_follows_the_option() {
    let mut b = colors(2);
    let f = float_in(&mut b);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, f);
    b.store_io(IoVariable::FragmentOutputColor, Some(1), 0, f);
    assert!(!b.msl().contains("main0_d"));
    let ctx = super::Context::default();
    let opt = super::Options {
        dual_source: true,
        ..super::Options::default()
    };
    let t = super::library::build(&b.s, &ctx, &opt).expect("build");
    let msl = &t.libraries[0].msl;
    assert!(msl.contains("float4 c1 [[color(0), index(1)]];"), "{msl}");
    assert!(msl.contains("fragment main0_out_d main0_d("));
    let r = t.libraries[0].reflection.as_ref().expect("r").to_value();
    assert_eq!(r.get("entry_d"), Some(&Value::str("main0_d")));
    assert!(
        t.side
            .expect("side")
            .alpha
            .contains(&"main0_d_a".to_string())
    );
}

#[test]
#[ignore]
fn metal_builds_the_derived_entries() {
    let mut b = colors(2);
    let f = float_in(&mut b);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, f);
    let v = int_value(&mut b);
    b.store_io(IoVariable::FragmentOutputColor, Some(1), 0, v);
    b.s.props.definitions.early_z_force = true;
    let opt = super::Options {
        dual_source: true,
        ..super::Options::default()
    };
    let t = super::library::build(&b.s, &super::Context::default(), &opt).expect("build");
    let bytes = metal_library("derived_ps", &t);
    for n in ["main0_u", "main0_m", "main0_d", "main0_m_a", "main0_u_l"] {
        let at = |w: &[u8]| bytes.windows(w.len()).any(|x| x == w);
        assert!(at(n.as_bytes()), "{n}");
    }
}

/// The text of the function that starts with `head`, up to its closing brace at the start of a line.
fn function_of<'a>(msl: &'a str, head: &str) -> &'a str {
    let at = msl
        .find(head)
        .unwrap_or_else(|| panic!("missing {head}\n{msl}"));
    let end = msl[at..].find("\n}\n").expect("end of function");
    &msl[at..at + end]
}

/// `colors(n)` that stores a float to each of the colours 0 to n - 1.
fn float_colors(n: usize) -> B {
    let mut b = colors(n);
    let f = float_in(&mut b);
    for l in 0..n {
        b.store_io(IoVariable::FragmentOutputColor, Some(l as i32), 0, f);
    }
    b
}

#[test]
fn the_blend_rule_wraps_main0() {
    let (_, t) = side_sample();
    let msl = &t.libraries[0].msl;
    assert!(msl.contains("constant uint* dm_bz [[id("), "{msl}");
    let r = t.libraries[0].reflection.as_ref().expect("reflection");
    let bz = r
        .bindings
        .iter()
        .find(|b| b.msl_name == "dm_bz")
        .expect("the blend word binding");
    assert_eq!(bz.kind, crate::msl::abi::Kind::BlendZ);
    assert_eq!(bz.kind.as_str(), "blendz");
    assert!(msl.contains("static main0_out dm_body_main0("));
    assert!(msl.contains("static inline __attribute__((always_inline)) bool dm_bz_zero("));
    let head = "static inline __attribute__((always_inline)) bool dm_bz_zero(";
    let at = msl.find(head).expect("helper");
    assert!(msl[..at].ends_with('\n'), "the helper is not a template");
    assert!(
        !msl[..at]
            .trim_end()
            .lines()
            .last()
            .unwrap_or("")
            .starts_with("template"),
        "the helper is not a template"
    );
    let main0 = function_of(msl, "fragment main0_out main0(");
    let rule = main0
        .find("if (dm_bz_zero((dm_bz >> 3u) & 7u, dm_r.c1.w, 0.0f))")
        .expect("the rule line of colour 1");
    let call = main0.find("main0_out dm_r = dm_body_main0(").expect("call");
    let ret = main0.find("return dm_r;").expect("return");
    assert!(call < rule && rule < ret, "{main0}");
    assert!(main0.contains("uint dm_bz = *spvDescriptorSet0.dm_bz;"));
    assert!(main0.contains("dm_r.c1.x = 0.0f; dm_r.c1.y = 0.0f; dm_r.c1.z = 0.0f;"));
    assert!(!main0.contains("dm_r.c1.w = 0.0f"));
}

#[test]
fn the_blend_rule_takes_the_body_form() {
    let mut b = colors(1);
    let f = float_in(&mut b);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, f);
    let msl = b.msl();
    assert!(msl.contains("static main0_out dm_body_main0("), "{msl}");
    let main0 = function_of(&msl, "fragment main0_out main0(");
    assert!(main0.contains("main0_out dm_r = dm_body_main0("));
    assert!(main0.contains("if (dm_bz_zero((dm_bz >> 0u) & 7u, dm_r.c0.w, 0.0f))"));
}

#[test]
fn each_target_has_its_own_kind() {
    let msl = float_colors(3).msl();
    let main0 = function_of(&msl, "fragment main0_out main0(");
    for (n, shift) in [(0, 0), (1, 3), (2, 6)] {
        let want = format!("if (dm_bz_zero((dm_bz >> {shift}u) & 7u, dm_r.c{n}.w, 0.0f))");
        assert!(main0.contains(&want), "missing {want}\n{main0}");
        assert!(main0.contains(&format!("dm_r.c{n}.z = 0.0f;")));
    }
}

#[test]
fn pads_and_integer_entries_get_no_rule() {
    // The pad of colour 0 gets no line, the float colour 1 does.
    let (_, t) = side_sample();
    let msl = &t.libraries[0].msl;
    assert!(!msl.contains("dm_r.c0_pad.x = 0.0f"), "{msl}");
    assert!(!msl.contains("dm_r.c0_pad.w, "));
    // An integer colour has an entry without a rule.
    let mut b = colors(1);
    let v = int_value(&mut b);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, v);
    let msl = b.msl();
    let u = function_of(&msl, "static main0_out_u dm_body_main0_u(");
    assert!(!u.contains("dm_bz"), "{u}");
    // A mixed shader: only the float member of `main0_m` has a line.
    let mut b = colors(2);
    let f = float_in(&mut b);
    b.store_io(IoVariable::FragmentOutputColor, Some(0), 0, f);
    let v = int_value(&mut b);
    b.store_io(IoVariable::FragmentOutputColor, Some(1), 0, v);
    let msl = b.msl();
    let m = function_of(&msl, "static main0_out_m dm_body_main0_m(");
    assert!(
        m.contains("if (dm_bz_zero((dm_bz >> 0u) & 7u, dm_r.c0.w, 0.0f))"),
        "{m}"
    );
    assert!(!m.contains("dm_r.c1.x = 0.0f"), "{m}");
    assert!(!m.contains(">> 3u"), "{m}");
}

#[test]
fn the_second_source_feeds_the_first_target() {
    let b = float_colors(2);
    let opt = super::Options {
        dual_source: true,
        ..super::Options::default()
    };
    let t = super::library::build(&b.s, &super::Context::default(), &opt).expect("build");
    let msl = &t.libraries[0].msl;
    let d = function_of(msl, "static main0_out_d dm_body_main0_d(");
    assert!(
        d.contains("if (dm_bz_zero((dm_bz >> 0u) & 7u, dm_r.c0.w, dm_r.c1.w))"),
        "{d}"
    );
    assert!(!d.contains("dm_r.c1.x = 0.0f"), "{d}");
    assert!(!d.contains(">> 3u"), "{d}");
    // The entry for the guest's own colours keeps a line per colour.
    let main0 = function_of(msl, "fragment main0_out main0(");
    assert!(main0.contains("dm_r.c0.w, 0.0f))") && main0.contains("dm_r.c1.w, 0.0f))"));
}

#[test]
fn a_shader_without_colours_reads_no_word() {
    let mut b = B::new(crate::attr::Stage::Fragment, "ps");
    b.s.props.definitions.imap_types = vec![[2, 2, 2, 2]];
    let msl = b.msl();
    assert!(msl.contains("constant uint* dm_bz [[id("), "{msl}");
    assert!(!msl.contains("dm_bz_zero"));
    assert!(!msl.contains("*spvDescriptorSet0.dm_bz"));
}

#[test]
#[ignore]
fn metal_builds_the_blend_rule() {
    let (_, t) = side_sample();
    metal_library("blendz_ps", &t);
    let mut b = float_colors(2);
    b.s.props.definitions.early_z_force = true;
    let opt = super::Options {
        dual_source: true,
        ..super::Options::default()
    };
    let t = super::library::build(&b.s, &super::Context::default(), &opt).expect("build");
    metal_library("blendz_dual_ps", &t);
}
