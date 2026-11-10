//! Tests of the texture binding gaps: 1D samples, offsets in registers, 3D texture arrays and cube images.

use super::abi::{C2_IMG, C2_ROW, C2_TEX};
use super::gaps_tests::{bare_compute, msl, translate};
use super::inst_tex::{DERIVATIVES, INT_COORDS, LOD_LEVEL, OFFSET};
use super::tests::{B, F4};
use crate::attr::Stage;
use crate::driver::TexInfo;
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::props::TextureDef;
use crate::structured::ast::{A, AstTex};
use crate::structured::types::AggType;
use crate::types::SamplerType;

const NAME: &str = "fp_t_3";

fn texture(b: &mut B, ty: SamplerType, image: bool) -> AstTex {
    let def = TextureDef {
        set: 0,
        binding: 0,
        array_length: 1,
        separate: false,
        name: if image { "fp_i_2" } else { NAME }.into(),
        ty,
        format: if image { 10 } else { 0 },
        flags: 0,
    };
    let (base, slot) = if image { (C2_IMG, 2) } else { (C2_TEX, 3) };
    let info = TexInfo {
        binding: 0,
        ty: ty.0,
        format: def.format,
        cbuf_slot: -1,
        handle: i64::from((base + slot * C2_ROW) / 4),
        array_length: 1,
        separate: false,
        flags: 0,
    };
    let props = &mut b.s.props.properties;
    let prog = &mut b.s.program;
    if image {
        props.images.push(def.clone());
        prog.images.push(info);
    } else {
        props.textures.push(def.clone());
        prog.textures.push(info);
    }
    AstTex {
        ty,
        format: def.format,
        flags: 0,
        set: 0,
        binding: 0,
        sampler_set: 0,
        sampler_binding: -1,
    }
}

fn pixel() -> (B, A, A) {
    let mut b = B::new(Stage::Fragment, "ps");
    b.s.props.definitions.imap_types = vec![[2, 2, 2, 2]];
    b.s.props.definitions.fragment_output_types = vec![F4, F4];
    b.io(StorageKind::Input, IoVariable::UserDefined, 0);
    b.io(StorageKind::Output, IoVariable::FragmentOutputColor, 1);
    let mut load = |c| {
        b.load_io(
            StorageKind::Input,
            IoVariable::UserDefined,
            Some(0),
            c,
            AggType::FP32,
        )
    };
    let (u, v) = (load(0), load(1));
    (b, u, v)
}

/// A pixel shader with one sample of a texture of type `ty`. `srcs` makes the sources from two inputs.
pub(super) fn sample(ty: SamplerType, flags: u32, srcs: impl Fn(&mut B, A, A) -> Vec<A>) -> B {
    let (mut b, u, v) = pixel();
    let mut tex = texture(&mut b, ty, false);
    tex.flags = flags;
    let s = srcs(&mut b, u, v);
    let op = b.s.info.ast.new_tex_op(Inst::TextureSample, tex, 15, s);
    let _ = b.def(F4, op);
    b.store_io(IoVariable::FragmentOutputColor, Some(1), 0, u);
    b
}

fn notes(b: &B) -> Vec<String> {
    translate(b).libraries[0]
        .reflection
        .clone()
        .expect("reflection")
        .notes
}

const T1D: SamplerType = SamplerType::TEXTURE_1D;

#[test]
fn a_1d_sample_with_a_level_reads_the_default_level() {
    let b = sample(T1D, LOD_LEVEL, |_, u, v| vec![u, v]);
    let m = msl(&b);
    assert!(
        m.contains("fp_t_3.sample(spvDescriptorSet0.fp_t_3Smplr, "),
        "{m}"
    );
    assert!(!m.contains("level("), "{m}");
    assert!(notes(&b).iter().any(|n| n.starts_with("1D texture sample")));
}

#[test]
fn a_1d_gradient_is_a_plain_sample() {
    let b = sample(T1D, DERIVATIVES, |_, u, v| vec![u, v, u]);
    let m = msl(&b);
    assert!(!m.contains("gradient"), "{m}");
    assert!(m.contains(".sample("), "{m}");
}

#[test]
fn a_1d_array_sample_keeps_its_layer() {
    let ty = SamplerType(T1D.0 | SamplerType::ARRAY);
    let m = msl(&sample(ty, 0, |b, u, _| vec![u, b.k(2)]));
    assert!(m.contains(", uint(max(2, 0)))"), "{m}");
}

#[test]
fn a_constant_offset_stays_an_argument() {
    let b = sample(SamplerType::TEXTURE_2D, OFFSET, |b, u, v| {
        vec![u, v, b.k(1), b.k(-2)]
    });
    let m = msl(&b);
    assert!(m.contains(", int2(1, (-2)))"), "{m}");
    assert!(!m.contains("get_width"), "{m}");
}

#[test]
fn a_register_offset_moves_the_coordinate_by_the_level_size() {
    let b = sample(SamplerType::TEXTURE_2D, OFFSET, |_, u, v| vec![u, v, u, v]);
    let m = msl(&b);
    assert!(m.contains("/ float2(spvDescriptorSet0.fp_t_3.get_width(uint(spvDescriptorSet0.fp_t_3.calculate_clamped_lod("), "{m}");
    assert!(!m.contains("unported"), "{m}");
}

#[test]
fn a_register_offset_with_a_level_uses_that_level() {
    let b = sample(SamplerType::TEXTURE_2D, OFFSET | LOD_LEVEL, |b, u, v| {
        vec![u, v, b.k(0), u, v]
    });
    let m = msl(&b);
    assert!(m.contains("get_width(uint(max("), "{m}");
    assert!(m.contains("level("), "{m}");
}

#[test]
fn a_register_offset_of_a_fetch_adds_to_the_texel() {
    let b = sample(
        SamplerType::TEXTURE_2D,
        OFFSET | INT_COORDS | LOD_LEVEL,
        |b, _, _| vec![b.k(4), b.k(5), b.k(0), b.k(1), b.k(2)],
    );
    let m = msl(&b);
    assert!(m.contains(".read(uint2(int2(uint2("), "{m}");
    assert!(m.contains("+ int2(1, 2)"), "{m}");
}

#[test]
fn a_3d_texture_array_is_a_3d_texture() {
    let ty = SamplerType(SamplerType::TEXTURE_3D.0 | SamplerType::ARRAY);
    let b = sample(ty, 0, |b, u, v| vec![u, v, u, b.k(1)]);
    let m = msl(&b);
    assert!(m.contains("texture3d<float> fp_t_3"), "{m}");
    assert!(!m.contains("array"), "{m}");
    assert!(notes(&b).iter().any(|n| n.starts_with("3D texture array")));
}

/// A compute shader that stores to an image of type `ty` and loads from it.
pub(super) fn image(ty: SamplerType, coords: usize) -> B {
    let mut b = bare_compute();
    let tex = texture(&mut b, ty, true);
    let mut srcs: Vec<A> = (0..coords).map(|k| b.k(k as i32 + 1)).collect();
    let load =
        b.s.info
            .ast
            .new_tex_op(Inst::ImageLoad, tex.clone(), 15, srcs.clone());
    let _ = b.def(F4, load);
    srcs.extend((0..4).map(|k| b.k(k + 7)));
    let store = b.s.info.ast.new_tex_op(Inst::ImageStore, tex, 15, srcs);
    b.put(store);
    b
}

#[test]
fn a_cube_image_reads_and_writes_with_a_face() {
    let m = msl(&image(SamplerType::TEXTURE_CUBE, 3));
    assert!(
        m.contains("texturecube<float, access::read_write> fp_i_2"),
        "{m}"
    );
    assert!(m.contains(".read(uint2(1, 2), uint(3))"), "{m}");
    assert!(m.contains(".write(float4("), "{m}");
}

#[test]
fn a_cube_array_image_adds_its_layer_after_the_face() {
    let ty = SamplerType(SamplerType::TEXTURE_CUBE.0 | SamplerType::ARRAY);
    let m = msl(&image(ty, 4));
    assert!(
        m.contains("texturecube_array<float, access::read_write>"),
        "{m}"
    );
    assert!(m.contains(".read(uint2(1, 2), uint(3), uint(4))"), "{m}");
}

#[test]
fn a_plain_2d_image_is_unchanged() {
    let m = msl(&image(SamplerType::TEXTURE_2D, 2));
    assert!(m.contains(".read(uint2(1, 2))"), "{m}");
}

#[test]
fn a_3d_image_array_drops_its_layer() {
    let ty = SamplerType(SamplerType::TEXTURE_3D.0 | SamplerType::ARRAY);
    let b = image(ty, 4);
    let m = msl(&b);
    assert!(m.contains(".read(uint3(1, 2, 3))"), "{m}");
    assert!(notes(&b).iter().any(|n| n.starts_with("3D image array")));
}

/// A compute shader with one image atomic on an image of `ty` and `format`. `flags` are the atomic flags.
pub(super) fn image_atomic(ty: SamplerType, format: usize, flags: u32, coords: usize) -> B {
    let mut b = bare_compute();
    b.s.props.host.metal_image_atomics = true;
    let mut tex = texture(&mut b, ty, true);
    tex.format = format;
    tex.flags = flags;
    b.s.props.properties.images[0].format = format;
    b.s.program.images[0].format = format;
    let mut srcs: Vec<A> = (0..coords).map(|k| b.k(k as i32 + 1)).collect();
    srcs.push(b.k(5));
    if flags & (15 << 16) == 9 << 16 {
        srcs.push(b.k(6));
    }
    let ty = if format == 12 {
        AggType::S32
    } else {
        AggType::U32
    };
    let op = b.s.info.ast.new_tex_op(Inst::ImageAtomic, tex, 0, srcs);
    let _ = b.def(ty, op);
    b
}

#[test]
fn an_image_atomic_stays_inside_the_image() {
    let m = msl(&image_atomic(SamplerType::TEXTURE_2D, 11, 6 << 16, 2));
    assert!(
        m.contains("texture2d<uint, access::read_write> fp_i_2"),
        "{m}"
    );
    assert!(
        m.contains("if (all(t0c < uint2(spvDescriptorSet0.fp_i_2.get_width(), spvDescriptorSet0.fp_i_2.get_height()))) { t0 = spvDescriptorSet0.fp_i_2.atomic_fetch_or(t0c, uint4(5u)).x; }"),
        "{m}"
    );
    assert!(m.contains("uint t0 = 0u;"), "{m}");
}

#[test]
fn a_signed_atomic_uses_the_signed_image_and_a_compare_swaps() {
    let m = msl(&image_atomic(SamplerType::TEXTURE_BUFFER, 12, 9 << 16, 1));
    assert!(m.contains("texture_buffer<int, access::read_write>"), "{m}");
    assert!(
        m.contains("if (t0c < spvDescriptorSet0.fp_i_2.get_width())"),
        "{m}"
    );
    assert!(
        m.contains("while (!") && m.contains(".atomic_compare_exchange_weak(t0c, &t0x, int4(5)) && all(t0x == t0c)) {}"),
        "{m}"
    );
}

#[test]
fn an_array_image_atomic_tests_its_layer() {
    let ty = SamplerType(SamplerType::TEXTURE_2D.0 | SamplerType::ARRAY);
    let m = msl(&image_atomic(ty, 11, 1 << 16, 3));
    assert!(
        m.contains("t0l < spvDescriptorSet0.fp_i_2.get_array_size()"),
        "{m}"
    );
    assert!(m.contains("atomic_fetch_min(t0c, t0l, uint4(5u))"), "{m}");
}
