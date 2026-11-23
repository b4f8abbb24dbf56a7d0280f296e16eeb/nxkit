//! AIR 2.8's integer 2D texture fetch operations, including ignored results.

use air_bc::ast::{Op, Ty, Val};
use air_front::{raw_library, Target};

const SOURCE: &str = "#include <metal_stdlib>
using namespace metal;
struct Args {
    texture2d<uint, access::read_write> image [[id(0)]];
};
kernel void k(constant Args& args [[buffer(0)]], uint lane [[thread_position_in_grid]]) {
    args.image.atomic_fetch_or(uint2(lane, 0u), uint4(5u));
}";
#[test]
fn texture_fetches_have_air28_operands_and_survive_dce() {
    for (source, intrinsic) in [
        (
            SOURCE.to_owned(),
            "air.atomic_fetch_or_explicit_texture_2d.u.v4i32",
        ),
        (
            SOURCE.replace("atomic_fetch_or", "atomic_fetch_add"),
            "air.atomic_fetch_add_explicit_texture_2d.u.v4i32",
        ),
        (
            SOURCE.replace("atomic_fetch_or", "atomic_fetch_max"),
            "air.atomic_fetch_max_explicit_texture_2d.u.v4i32",
        ),
    ] {
        check_texture_fetch(&source, intrinsic, "uint");
    }
}

#[test]
fn signed_texture_min_and_max_have_air28_operands_and_survive_dce() {
    for (method, intrinsic) in [
        (
            "atomic_fetch_min",
            "air.atomic_fetch_min_explicit_texture_2d.s.v4i32",
        ),
        (
            "atomic_fetch_max",
            "air.atomic_fetch_max_explicit_texture_2d.s.v4i32",
        ),
    ] {
        let source = SOURCE
            .replace("texture2d<uint", "texture2d<int")
            .replace("uint4(5u)", "int4(-5, 6, 7, 8)")
            .replace("atomic_fetch_or", method);
        check_texture_fetch(&source, intrinsic, "int");
    }
}

fn check_texture_fetch(source: &str, intrinsic: &str, elem: &str) {
    let target = Target::macos_for(26);
    let ir = air_emit::compile_source(source, &target, "t.metal").expect("emits");
    assert!(ir.contains(&format!(
        "!\"air.read_write\", !\"air.arg_type_name\", !\"texture2d<{elem}, read_write>\""
    )));
    let mut module = air_bc::parse::parse(&ir).expect("parse emitted IR");
    air_bc::opt::optimize(&mut module);
    let call = module
        .functions
        .iter()
        .flat_map(|f| &f.blocks)
        .flat_map(|(_, body)| body)
        .find_map(|i| match &i.op {
            Op::Call(_, _, ty, name, args, Some(group)) if name == intrinsic => {
                Some((ty, args, group))
            }
            _ => None,
        })
        .expect("dead result does not remove the image mutation");
    let (ty, args, group) = call;
    let ints = |lanes| Ty::Vector(lanes, Box::new(Ty::Int(32)));
    assert_eq!(*ty, ints(4));
    assert_eq!(args.len(), 6);
    assert_eq!(
        args[0].0,
        Ty::Ptr(Box::new(Ty::Named("struct._texture_2d_t".into())), 1)
    );
    assert_eq!(args[0].1, ["nocapture"]);
    assert_eq!(args[1].0, ints(2));
    assert_eq!(args[2], (ints(2), vec![], Val::Zero));
    assert_eq!(args[3].0, ints(4));
    assert_eq!(args[4], (Ty::Int(32), vec![], Val::Int(0)));
    assert_eq!(args[5], (Ty::Int(32), vec![], Val::Int(3)));
    let attrs = &module
        .attr_groups
        .iter()
        .find(|(id, _)| id == group)
        .expect("atomic call attr group")
        .1;
    assert_eq!(attrs, &["nounwind", "willreturn"]);

    let lib = raw_library(&module, &target).expect("raw library");
    let parsed = air_lib::parse(&lib).expect("metallib");
    assert_eq!(parsed.functions[0].name(), b"k");

    let live = source
        .replace(
            "uint lane [[thread_position_in_grid]]",
            &format!("device {elem}* dst [[buffer(1)]], uint lane [[thread_position_in_grid]]"),
        )
        .replace("    args.image.", "    dst[lane] = args.image.")
        .replace("));", ")).x;");
    let ir = air_emit::compile_source(&live, &target, "t.metal").expect("emits lane zero");
    assert!(ir.contains(intrinsic));
    assert!(ir.contains("extractelement <4 x i32>"));
}

#[test]
fn texture_fetches_reject_unproven_forms_and_targets() {
    for source in [
        SOURCE.replace("read_write", "read"),
        SOURCE.replace("read_write", "write"),
        SOURCE.replace("read_write", "sample"),
        SOURCE.replace("texture2d<uint", "texture2d_array<uint"),
        SOURCE
            .replace("texture2d<uint", "texture2d<int")
            .replace("uint4(5u)", "int4(5)"),
        SOURCE.replace("uint2(lane, 0u)", "int2(int(lane), 0)"),
        SOURCE.replace("uint4(5u)", "5u"),
        SOURCE.replace("atomic_fetch_or", "atomic_fetch_xor"),
        SOURCE.replace("atomic_fetch_or", "atomic_fetch_min"),
        SOURCE
            .replace("texture2d<uint", "texture2d<int")
            .replace("atomic_fetch_or", "atomic_fetch_max"),
        SOURCE
            .replace("atomic_fetch_or", "atomic_fetch_max")
            .replace("uint4(5u)", "5u"),
    ] {
        let error = air_emit::compile_source(&source, &Target::macos_for(26), "t.metal")
            .expect_err("unproven texture atomic");
        assert!(error.key().contains("method:"), "{error:?}");
    }
    let add = SOURCE.replace("atomic_fetch_or", "atomic_fetch_add");
    let add_wrong_access = add.replace("read_write", "read");
    let error = air_emit::compile_source(&add_wrong_access, &Target::macos_for(26), "t.metal")
        .expect_err("unproven texture atomic");
    assert!(error.key().contains("method:atomic_fetch_add"), "{error:?}");
    let max = SOURCE.replace("atomic_fetch_or", "atomic_fetch_max");
    for target in [Target::ios(), Target::macos()] {
        for (source, op) in [(SOURCE, "or"), (add.as_str(), "add"), (max.as_str(), "max")] {
            let error = air_emit::compile_source(source, &target, "t.metal")
                .expect_err("unproven AIR target");
            assert_eq!(
                error.key(),
                format!("unsupported:texture2d:atomic-fetch-{op}-target")
            );
        }
    }
}
