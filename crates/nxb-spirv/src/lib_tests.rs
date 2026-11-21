#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The modules in `tests/data` are the GLSL files beside them, compiled with `glslangValidator -V -o <file>.spv <file>`.

use super::*;

const ENTRY: &str = "vk_0123456789abcdef";
const DUAL: &str = "vk_0123456789abcdef_d";

pub(crate) const VERT: &[u8] = include_bytes!("../tests/data/basic.vert.spv");
pub(crate) const FRAG: &[u8] = include_bytes!("../tests/data/basic.frag.spv");
const ARRAYS: &[u8] = include_bytes!("../tests/data/arrays.frag.spv");
const DESCRIPTOR_ARRAY: &[u8] = include_bytes!("../tests/data/descriptor_array.frag.spv");
const RUNTIME_ARRAY: &[u8] = include_bytes!("../tests/data/runtime_array.frag.spv");
pub(crate) const IMAGE1D: &[u8] = include_bytes!("../tests/data/image1d.frag.spv");
pub(crate) const IMAGE1D_ARRAY: &[u8] = include_bytes!("../tests/data/image1d_array.frag.spv");
const KERNEL: &[u8] = include_bytes!("../tests/data/kernel.comp.spv");

fn tr(bytes: &[u8]) -> Result<Outcome, String> {
    translate(bytes, ENTRY, DUAL)
}

fn shader(bytes: &[u8]) -> Shader {
    match tr(bytes).unwrap() {
        Outcome::Shader(s) => s,
        Outcome::Skipped(st) => panic!("skipped {st}"),
    }
}

fn res(kind: Kind, set: u32, binding: u32, slot: u32, count: u32) -> Resource {
    Resource {
        kind,
        set,
        binding,
        slot,
        count,
    }
}

/// The resources as the old engine's table writes them, `kind:set.binding=slot`.
fn tokens(s: &Shader) -> String {
    let t: Vec<String> = s
        .resources
        .iter()
        .map(|r| {
            let k = match r.kind {
                Kind::Uniform => 'u',
                Kind::Storage => 'b',
                Kind::Texture => 't',
                Kind::Sampler => 's',
                Kind::Combined => 'c',
                Kind::Push => 'p',
            };
            format!("{k}:{}.{}={}", r.set, r.binding, r.slot)
        })
        .collect();
    t.join(" ")
}

#[test]
fn the_key_is_the_start_of_the_sha256() {
    // sha256("abc") = ba7816bf8f01cfea...
    assert_eq!(
        key_of(b"abc"),
        [0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea]
    );
}

#[test]
fn vertex_shader_has_buffer_and_push_constant_slots() {
    let s = shader(VERT);
    assert_eq!(s.stage, Stage::Vertex);
    assert_eq!(tokens(&s), "u:0.1=0 p:0.0=1");
    let name = ENTRY;
    assert!(
        s.msl.contains(&format!("vertex {name}Output {name}(")),
        "{}",
        s.msl
    );
    assert!(s.msl_dual.is_none());
}

#[test]
fn fragment_shader_has_combined_sampler_and_dual_source_variant() {
    let s = shader(FRAG);
    assert_eq!(s.stage, Stage::Fragment);
    assert_eq!(s.resources, vec![res(Kind::Combined, 0, 2, 0, 1)]);
    // the image and the sampler of the pair are at texture 0 and sampler 0
    assert!(
        s.msl.contains("[[texture(0)]]") && s.msl.contains("[[sampler(0)]]"),
        "{}",
        s.msl
    );
    assert!(s.msl.contains("[[color(1)]]"), "{}", s.msl);
    assert!(s.msl.contains("metal::precise::min"), "{}", s.msl);
    assert!(s.msl.contains("nxb_min("), "{}", s.msl);
    let d = s.msl_dual.expect("dual-source variant");
    assert!(d.contains("[[color(0), index(1)]]"), "{d}");
    assert!(d.contains(&format!("{}(", DUAL)), "{d}");
}

#[test]
fn storage_buffers_separate_samplers_and_unused_resources() {
    let s = shader(ARRAYS);
    // the combined image sampler at 0.5 is not used: it keeps texture and sampler slot 1, and is not listed
    assert_eq!(tokens(&s), "b:0.0=0 t:0.1=0 s:0.2=0 c:1.0=2");
    assert!(
        s.msl.contains("[[texture(2)]]") && s.msl.contains("[[sampler(2)]]"),
        "{}",
        s.msl
    );
    assert!(!s.msl.contains("[[texture(1)]]"), "{}", s.msl);
}

#[test]
fn an_array_of_descriptors_is_an_error() {
    // naga writes it as an argument buffer, which the table has no kind for
    let e = tr(DESCRIPTOR_ARRAY).unwrap_err();
    assert!(e.contains("array of descriptors"), "{e}");
}

#[test]
fn a_runtime_sized_storage_buffer_is_an_error() {
    // naga passes the lengths in a buffer of their own, which the table has no kind for
    let e = tr(RUNTIME_ARRAY).unwrap_err();
    assert!(e.contains("sizes buffer"), "{e}");
}

#[test]
fn one_d_images_are_sampled_as_rows_of_a_2d_texture() {
    let s = shader(IMAGE1D);
    // the combined image sampler after a separate sampler takes the next free slot of both classes
    assert_eq!(tokens(&s), "s:0.24=0 c:1.3=1 t:1.4=2 t:1.21=3");
    assert!(!s.msl.contains("texture1d"), "{}", s.msl);
    assert!(
        s.msl
            .contains("metal::texture2d<float, metal::access::sample>"),
        "{}",
        s.msl
    );
    assert!(
        s.msl
            .contains("metal::texture2d<float, metal::access::read_write>"),
        "{}",
        s.msl
    );
    assert!(s.msl.contains("0.5"), "{}", s.msl);
}

#[test]
fn an_array_of_1d_images_is_an_error() {
    let e = tr(IMAGE1D_ARRAY).unwrap_err();
    assert!(e.contains("1D"), "{e}");
}

#[test]
fn compute_and_other_stages_are_skipped() {
    assert_eq!(tr(KERNEL).unwrap(), Outcome::Skipped("comp"));
}

#[test]
fn bytes_that_are_not_spirv_are_an_error() {
    assert!(tr(b"MDB1").is_err());
    assert!(tr(&FRAG[..FRAG.len() - 4]).is_err());
}

#[test]
fn only_the_entry_point_has_external_linkage() {
    let s = shader(FRAG);
    let name = ENTRY;
    assert!(s.msl.contains("static void main_("), "{}", s.msl);
    assert!(
        s.msl.contains(&format!("\nfragment {name}Output {name}(")),
        "{}",
        s.msl
    );
    assert!(!s.msl.contains("static static"), "{}", s.msl);
}

#[test]
fn helpers_of_two_shaders_do_not_collide() {
    let src = "struct S {\n};\nmetal::float3 f_(\n    int a\n) {\n}\nvertex V v(\n  S s [[stage_in]]\n, metal::sampler x [[sampler(0)]]\n) {\n}\nconstant int k = int(1);\nint naga_f2i32(float value) {\n}\n";
    let want = src
        .replace("metal::float3 f_(", "static metal::float3 f_(")
        .replace("int naga_f2i32(", "static int naga_f2i32(");
    assert_eq!(crate::msl::internal(src), want);
}
