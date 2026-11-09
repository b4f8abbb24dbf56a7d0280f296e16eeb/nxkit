//! Tests of the state objects.

use super::*;
use crate::nvn::abi::testing;

fn obj() -> u64 {
    testing::alloc(0x40)
}

#[test]
fn blend_defaults_are_one_zero_add() {
    let s = obj();
    blend_set_defaults(s);
    let b = blend(s).expect("blend");
    assert_eq!(
        (b.src_rgb, b.dst_rgb, b.src_alpha, b.dst_alpha),
        (2, 1, 2, 1)
    );
    assert_eq!((b.eq_rgb, b.eq_alpha, b.advanced_mode), (1, 1, 0));
    assert!(b.premultiplied_src && b.normalized_dst);
}

#[test]
fn blend_fields_do_not_overlap() {
    let s = obj();
    blend_set_defaults(s);
    blend_set_target(s, 7);
    blend_set_func(s, 0x7f, 0x55, 0x2a, 0x7f);
    blend_set_equation(s, 7, 5);
    blend_set_advanced_mode(s, 0x32);
    blend_set_premultiplied(s, 0);
    blend_set_normalized(s, 0);
    blend_set_overlap(s, 3);
    let b = blend(s).expect("blend");
    assert_eq!(
        (b.target, b.src_rgb, b.dst_rgb, b.src_alpha, b.dst_alpha),
        (7, 0x7f, 0x55, 0x2a, 0x7f)
    );
    assert_eq!(
        (b.eq_rgb, b.eq_alpha, b.advanced_mode, b.overlap),
        (7, 5, 0x32, 3)
    );
    assert!(!b.premultiplied_src && !b.normalized_dst);
    assert_eq!(blend_get_premultiplied(s), 0);
    blend_set_advanced_mode(s, 0x33);
    assert_eq!(blend_get_advanced_mode(s), 0);
}

#[test]
fn color_blend_enable_ignores_a_bad_index() {
    let s = obj();
    color_set_defaults(s);
    color_set_blend_enable(s, 2, 1);
    color_set_blend_enable(s, 8, 1);
    color_set_blend_enable(s, -1, 1);
    let c = color(s).expect("color");
    assert_eq!(c.blend_enable, 0b100);
    assert_eq!((c.logic_op, c.alpha_func), (3, 8));
    assert_eq!(color_get_blend_enable(s, 2), 1);
    assert_eq!(color_get_blend_enable(s, 9), 0);
    color_set_blend_enable(s, 2, 0);
    assert!(!color(s).expect("color").any_blend());
}

#[test]
fn channel_mask_packs_four_bits_per_target() {
    let s = obj();
    mask_set_defaults(s);
    mask_set(s, 1, 1, 0, 1, 0);
    mask_set(s, 8, 0, 0, 0, 0);
    let m = channel_mask(s).expect("mask");
    assert_eq!(m.channels(1), 0b0101);
    assert_eq!(m.channels(0), 0xf);
    assert_eq!(m.written_targets(), 0xff);
    let out = obj();
    mask_get(s, 1, out, out + 1, out + 2, out + 3);
    let mut b = [0u8; 4];
    Obj(out).read(0, &mut b).expect("read");
    assert_eq!(b, [1, 0, 1, 0]);
    mask_get(s, 1, 0, 0, 0, 0);
}

#[test]
fn depth_stencil_sets_the_faces_the_mask_names() {
    let s = obj();
    ds_set_defaults(s);
    let d = depth_stencil(s).expect("ds");
    assert!(!d.depth_test && d.depth_write && !d.stencil_test);
    assert_eq!((d.depth_func, d.front.func, d.back.zpass), (2, 8, 1));
    ds_set_stencil_func(s, 2, 3);
    ds_set_stencil_op(s, 1, 4, 5, 6);
    let d = depth_stencil(s).expect("ds");
    assert_eq!((d.front.func, d.back.func), (8, 3));
    assert_eq!((d.front.fail, d.front.zfail, d.front.zpass), (4, 5, 6));
    assert_eq!((d.back.fail, d.back.zfail, d.back.zpass), (1, 1, 1));
    ds_set_stencil_func(s, 3, 9);
    assert_eq!(ds_get_stencil_func(s, 1), 9);
    assert_eq!(ds_get_stencil_func(s, 2), 9);
}

#[test]
fn multisample_defaults_enable_and_keep_each_byte() {
    let s = obj();
    ms_set_defaults(s);
    assert_eq!(ms_get_enable(s), 1);
    ms_set_samples(s, 4);
    ms_set_a2c(s, 1);
    ms_set_sl_grid(s, 1);
    let m = multisample(s).expect("ms");
    assert!(m.enable && m.alpha_to_coverage && m.sample_locations_grid);
    assert_eq!((m.samples, m.raster_samples), (4, 0));
}

#[test]
fn polygon_defaults_and_fields() {
    let s = obj();
    poly_set_defaults(s);
    let p = polygon(s).expect("polygon");
    assert_eq!(
        (p.cull_face, p.front_face, p.mode, p.offset_enables),
        (0, 1, 2, 0)
    );
    poly_set_cull(s, 2);
    poly_set_front(s, 0);
    poly_set_mode(s, 1);
    poly_set_offset(s, 7);
    let p = polygon(s).expect("polygon");
    assert_eq!(
        (p.cull_face, p.front_face, p.mode, p.offset_enables),
        (2, 0, 1, 7)
    );
}

#[test]
fn vertex_attrib_and_stream_round_trip() {
    let s = obj();
    va_set_defaults(s);
    va_set_format(s, 0x2a, 0x7_ffff);
    va_set_stream(s, 31);
    let a = vertex_attrib(s).expect("attrib");
    assert_eq!((a.format, a.offset, a.stream), (0x2a, 0x7_ffff, 31));
    let out = obj();
    va_get_format(s, out, out + 8);
    assert_eq!(Obj(out).i32(0).expect("format"), 0x2a);
    assert_eq!(Obj(out).i64(8).expect("offset"), 0x7_ffff);
    va_get_format(s, 0, 0);
    vs_set_stride(s + 0x20, 0x1_0000_0010);
    vs_set_divisor(s + 0x20, 3);
    assert_eq!(vs_get_stride(s + 0x20), 0x10);
    let v = vertex_stream(s + 0x20).expect("stream");
    assert_eq!((v.stride, v.divisor), (0x10, 3));
}

#[test]
fn the_group_exports_every_name_of_its_row() {
    let got: Vec<&str> = procs().iter().map(|(n, _)| *n).collect();
    let set: std::collections::HashSet<&str> = got.iter().copied().collect();
    assert_eq!(set.len(), got.len(), "a name is exported twice");
    let want: std::collections::HashSet<&str> = nxkit_nvn::entries::FFI_GROUPS
        .iter()
        .filter(|(g, _)| *g == "state")
        .flat_map(|(_, names)| names.iter().copied())
        .collect();
    assert_eq!(set, want);
}
