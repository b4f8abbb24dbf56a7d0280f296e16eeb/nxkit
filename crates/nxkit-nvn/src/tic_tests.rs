//! The tests of `tic`.

use super::*;

fn desc() -> TextureDesc {
    TextureDesc {
        target: Target::D2Array,
        format: 0x25,
        flags: 0x200,
        samples: 0,
        width: 1920,
        height: 1080,
        depth: 6,
        levels: 11,
        stride: 0,
        packaged_block: None,
    }
}

#[test]
fn block_linear_header_round_trips() {
    let addr = GpuAddr::new(0x15_0400_8000).unwrap();
    let mut view = TextureView::identity();
    view.base_level = 2;
    view.levels = 3;
    view.base_layer = 1;
    view.layers = 4;
    view.format = Some(0x41);
    view.target = Some(Target::D2);
    view.swizzle = [5, 4, 3, 1];
    let tic = Tic::for_texture(&desc(), addr, view).unwrap();
    assert!(matches!(tic.layout, TicLayout::BlockLinear { .. }));
    assert_eq!(Tic::decode(&tic.encode().unwrap()).unwrap(), tic);
}

#[test]
fn pitch_buffer_and_multisample_headers_round_trip() {
    let addr = GpuAddr::new((1 << 40) - 1).unwrap();
    let mut d = desc();
    d.flags = 0x10;
    d.target = Target::D2;
    d.depth = 1;
    d.levels = 1;
    let tic = Tic::for_texture(&d, addr, TextureView::identity()).unwrap();
    assert_eq!(tic.layout, TicLayout::Pitch { stride: 7680 });
    assert_eq!(Tic::decode(&tic.encode().unwrap()).unwrap(), tic);
    d.target = Target::Buffer;
    d.flags = 0;
    let tic = Tic::for_texture(&d, addr, TextureView::identity()).unwrap();
    assert_eq!(tic.layout, TicLayout::Buffer);
    assert_eq!(Tic::decode(&tic.encode().unwrap()).unwrap(), tic);
    // One texel per pixel of a 1920x1088 target is above the 2^20 of a 2D width.
    d.width = 1920 * 1088;
    let big = Tic::for_texture(&d, addr, TextureView::identity()).unwrap();
    assert_eq!(Tic::decode(&big.encode().unwrap()).unwrap(), big);
    d.width = 1 << 27;
    let top = Tic::for_texture(&d, addr, TextureView::identity()).unwrap();
    assert_eq!(Tic::decode(&top.encode().unwrap()).unwrap().width, 1 << 27);
    d.width = desc().width;
    d.target = Target::D2Multisample;
    d.samples = 8;
    let tic = Tic::for_texture(&d, addr, TextureView::identity()).unwrap();
    assert_eq!(Tic::decode(&tic.encode().unwrap()).unwrap().samples, 8);
}

#[test]
fn the_address_sits_in_words_1_and_2() {
    let addr = GpuAddr::new(0x15_0400_8000).unwrap();
    let w = Tic::for_texture(&desc(), addr, TextureView::identity())
        .unwrap()
        .encode()
        .unwrap();
    assert_eq!(w[1], 0x0400_8000);
    assert_eq!(w[2] & 0xff, 0x15);
    assert_eq!(
        Tic::for_texture(&desc(), addr, TextureView::identity())
            .unwrap()
            .to_bytes()
            .unwrap()[4..8],
        w[1].to_le_bytes()
    );
}

#[test]
fn values_that_do_not_fit_are_errors() {
    let addr = GpuAddr::new(0x1000).unwrap();
    let mut d = desc();
    d.width = 1 << 20;
    d.height = 1;
    d.depth = 1;
    d.target = Target::D2;
    d.levels = 1;
    let tic = Tic::for_texture(&d, addr, TextureView::identity()).unwrap();
    assert!(tic.encode().is_ok());
    let mut big = tic;
    big.width += 1;
    assert_eq!(big.encode(), Err(NvnError::FieldRange("width")));
    let mut bad = tic;
    bad.view.swizzle = [8, 0, 0, 0];
    assert!(bad.encode().is_err());
    let mut zero = [0u32; 8];
    zero[0] = 2 << 29;
    assert!(Tic::decode(&zero).is_err());
}

#[test]
fn sampler_header_round_trips() {
    let tsc = Tsc {
        min_filter: 5,
        mag_filter: 1,
        wrap_s: 2,
        wrap_t: 4,
        wrap_r: 0,
        compare_mode: 1,
        compare_func: 7,
        reduction: 2,
        lod_min: -1.5,
        lod_max: 13.0,
        lod_bias: 0.25,
        aniso: 16.0,
        border: [0.0, 0.5, 1.0, 1.0],
    };
    assert_eq!(Tsc::decode(&tsc.encode().unwrap()), tsc);
    assert_eq!(tsc.to_bytes().unwrap().len(), 32);
    let mut bad = tsc;
    bad.aniso = 40.0;
    assert!(bad.encode().is_err());
    bad = tsc;
    bad.compare_func = 16;
    assert!(bad.encode().is_err());
}

#[test]
fn the_fields_do_not_overlap_and_hold_the_largest_values() {
    let tic = Tic {
        address: GpuAddr::new((1 << 40) - 1).unwrap(),
        format: 156,
        target: Target::Buffer,
        layout: TicLayout::BlockLinear {
            tile_h: 7,
            tile_d: 7,
        },
        samples: 128,
        width: 1 << 20,
        height: 1 << 20,
        depth: 1 << 20,
        levels: 63,
        view: TextureView {
            base_level: 31,
            levels: 63,
            base_layer: u16::MAX,
            layers: u16::MAX,
            format: Some(156),
            target: Some(Target::Buffer),
            swizzle: [5, 5, 5, 5],
            stencil: true,
        },
        flags: 0xfff,
    };
    assert_eq!(Tic::decode(&tic.encode().unwrap()).unwrap(), tic);
    let pitch = Tic {
        layout: TicLayout::Pitch {
            stride: (1 << 24) - 1,
        },
        ..tic
    };
    assert_eq!(Tic::decode(&pitch.encode().unwrap()).unwrap(), pitch);
    let tsc = Tsc {
        min_filter: 7,
        mag_filter: 3,
        wrap_s: 7,
        wrap_t: 7,
        wrap_r: 7,
        compare_mode: 3,
        compare_func: 15,
        reduction: 3,
        lod_min: f32::MIN,
        lod_max: f32::MAX,
        lod_bias: -0.0,
        aniso: 31.75,
        border: [f32::MAX, f32::MIN, 1.0, 0.0],
    };
    assert_eq!(Tsc::decode(&tsc.encode().unwrap()), tsc);
}
