//! Block-linear kernels with mip levels, layers and volumes, and the copy-back into block-linear.
//! The CPU reference is written here from the layout rules and shares no code with the crate.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use nxkit_metal::{
    ArchiveConfig, ComputeCommand, Context, Deswizzler, Format, FrameRing, GobOrder, GuestMemory,
    PipelineCache, SurfaceLayout, Swizzler, Texture, TextureDesc, TextureKind, TextureRegion,
};

const BPB: u32 = 4;

fn ctx() -> Option<Context> {
    match Context::system_default() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            None
        }
    }
}

fn pattern(n: usize, seed: u32) -> Vec<u8> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as u8
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Lv {
    w: u32,
    h: u32,
    d: u32,
    bh: u32,
    bd: u32,
    off: u64,
    bytes: u64,
}

// The rule of deko3d: the block shrinks while half of it still covers the level.
fn fit(mut g: u32, unit: u32, dim: u32) -> u32 {
    while g > 1 && unit * (g / 2) >= dim {
        g /= 2;
    }
    g
}

fn level(s: &SurfaceLayout, l: u32) -> Lv {
    let mut off = 0;
    for i in 0..=l {
        let (w, h, d) = (
            (s.width >> i).max(1),
            (s.height >> i).max(1),
            (s.depth >> i).max(1),
        );
        let (bh, bd) = (fit(s.block_height, 8, h), fit(s.block_depth, 1, d));
        let gobs = (w * BPB).div_ceil(64) as u64;
        let bytes =
            gobs * h.div_ceil(8 * bh) as u64 * d.div_ceil(bd) as u64 * 512 * (bh * bd) as u64;
        if i == l {
            return Lv {
                w,
                h,
                d,
                bh,
                bd,
                off,
                bytes,
            };
        }
        off += bytes;
    }
    unreachable!()
}

fn layer_stride(s: &SurfaceLayout) -> u64 {
    let last = level(s, s.mip_levels - 1);
    let total = last.off + last.bytes;
    let l0 = level(s, 0);
    let unit = 512 * (l0.bh * l0.bd) as u64;
    if s.layers > 1 {
        total.div_ceil(unit) * unit
    } else {
        total
    }
}

fn gob(x: u32, y: u32, order: GobOrder) -> u64 {
    let v = match order {
        GobOrder::Tegra => {
            (x % 64 / 32) * 256 + (y % 8 / 2) * 64 + (x % 32 / 16) * 32 + (y % 2) * 16 + x % 16
        }
        GobOrder::Switch2 => {
            (x % 64 / 32) * 256
                + (y % 8 / 4) * 128
                + (x % 32 / 16) * 64
                + (y % 4 / 2) * 32
                + (y % 2) * 16
                + x % 16
        }
    };
    v as u64
}

fn at(s: &SurfaceLayout, l: u32, layer: u32, (x, y, z): (u32, u32, u32)) -> usize {
    let lv = level(s, l);
    let xb = x * BPB;
    let gobs_x = (lv.w * BPB).div_ceil(64) as u64;
    let blocks_y = lv.h.div_ceil(8 * lv.bh) as u64;
    let block = 512 * (lv.bh * lv.bd) as u64;
    let o = (z / lv.bd) as u64 * gobs_x * blocks_y * block
        + (y / (8 * lv.bh)) as u64 * gobs_x * block
        + (xb / 64) as u64 * block
        + ((z % lv.bd) * lv.bh + (y % (8 * lv.bh)) / 8) as u64 * 512
        + gob(xb, y, s.order);
    (u64::from(layer) * layer_stride(s) + lv.off + o) as usize
}

fn texel(data: &[u8], s: &SurfaceLayout, l: u32, layer: u32, p: (u32, u32, u32)) -> [u8; 4] {
    let o = at(s, l, layer, p);
    data[o..o + 4].try_into().unwrap()
}

struct Setup {
    ctx: Context,
    guest: Arc<GuestMemory>,
    cache: PipelineCache,
}

fn setup() -> Option<Setup> {
    let ctx = ctx()?;
    let guest = Arc::new(GuestMemory::allocate(&ctx, 4 << 20).unwrap());
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    Some(Setup { ctx, guest, cache })
}

fn texture(ctx: &Context, kind: TextureKind, s: &SurfaceLayout) -> Texture {
    let mut d = TextureDesc::compute_2d(s.width, s.height, Format::Rgba8Unorm);
    d.kind = kind;
    d.depth = s.depth;
    d.mip_levels = s.mip_levels;
    d.array_layers = s.layers;
    d.allow_views = true;
    Texture::new(ctx, d).unwrap()
}

fn read_level(t: &Texture, l: u32, slice: u32, w: u32, h: u32, d: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * d * BPB) as usize];
    let r = TextureRegion {
        level: l,
        slice,
        origin: [0; 3],
        size: [w, h, d],
    };
    t.read_region(&r, (w * BPB) as usize, (w * h * BPB) as usize, &mut out)
        .unwrap();
    out
}

#[test]
fn array_with_mips_deswizzles_and_swizzles_back() {
    let Some(st) = setup() else { return };
    let s = SurfaceLayout {
        bytes_per_block: BPB,
        width: 70,
        height: 48,
        depth: 1,
        mip_levels: 5,
        layers: 3,
        block_height: 4,
        block_depth: 1,
        order: GobOrder::Tegra,
    };
    assert_eq!(s.layer_stride().unwrap(), layer_stride(&s));
    for l in 0..5 {
        let lv = level(&s, l);
        let got = s.level(l).unwrap();
        assert_eq!(
            (got.offset, got.bytes, got.block_height),
            (lv.off, lv.bytes, lv.bh),
            "level {l}"
        );
    }
    let total = s.total_bytes().unwrap() as usize;
    let (src_off, dst_off) = (0x4000u64, 0x4000 + (total as u64).next_multiple_of(0x4000));
    let data = pattern(total, 7);
    st.guest.write(src_off, &data).unwrap();
    st.guest.write(dst_off, &vec![0xEE; total]).unwrap();
    let tex = texture(&st.ctx, TextureKind::D2Array, &s);
    let (ds, sw) = (
        Deswizzler::new(&st.ctx, &st.cache).unwrap(),
        Swizzler::new(&st.ctx, &st.cache).unwrap(),
    );
    let mut ring = FrameRing::new(&st.ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    let mut pass = frame.commands().begin_compute_pass(ds.table()).unwrap();
    for l in 0..5 {
        for layer in 0..3 {
            let lv = level(&s, l);
            let r = TextureRegion::whole(l, layer, [lv.w, lv.h, 1]);
            ds.encode_region(&mut pass, &scratch, &st.guest, src_off, &s, &tex, &r)
                .unwrap();
        }
    }
    pass.end();
    let mut pass = frame.commands().begin_compute_pass(sw.table()).unwrap();
    pass.encode(&ComputeCommand::WaitForEarlierPasses).unwrap();
    for l in 0..5 {
        for layer in 0..3 {
            let lv = level(&s, l);
            let r = TextureRegion::whole(l, layer, [lv.w, lv.h, 1]);
            sw.encode_region(&mut pass, &scratch, &st.guest, dst_off, &s, &tex, &r)
                .unwrap();
        }
    }
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    let mut back = vec![0u8; total];
    st.guest.read(dst_off, &mut back).unwrap();
    for l in 0..5 {
        let lv = level(&s, l);
        for layer in 0..3 {
            let got = read_level(&tex, l, layer, lv.w, lv.h, 1);
            for y in 0..lv.h {
                for x in 0..lv.w {
                    let want = texel(&data, &s, l, layer, (x, y, 0));
                    let i = ((y * lv.w + x) * BPB) as usize;
                    assert_eq!(
                        got[i..i + 4],
                        want,
                        "deswizzle level {l} layer {layer} at {x},{y}"
                    );
                    assert_eq!(
                        texel(&back, &s, l, layer, (x, y, 0)),
                        want,
                        "swizzle level {l} layer {layer} at {x},{y}"
                    );
                }
            }
        }
    }
}

#[test]
fn volume_with_mips_deswizzles() {
    let Some(st) = setup() else { return };
    let s = SurfaceLayout {
        bytes_per_block: BPB,
        width: 24,
        height: 20,
        depth: 9,
        mip_levels: 3,
        layers: 1,
        block_height: 2,
        block_depth: 4,
        order: GobOrder::Switch2,
    };
    let total = s.total_bytes().unwrap() as usize;
    let data = pattern(total, 11);
    st.guest.write(0x4000, &data).unwrap();
    let tex = texture(&st.ctx, TextureKind::D3, &s);
    let ds = Deswizzler::new(&st.ctx, &st.cache).unwrap();
    let mut ring = FrameRing::new(&st.ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    let mut pass = frame.commands().begin_compute_pass(ds.table()).unwrap();
    for l in 0..3 {
        let lv = level(&s, l);
        let r = TextureRegion::whole(l, 0, [lv.w, lv.h, lv.d]);
        ds.encode_region(&mut pass, &scratch, &st.guest, 0x4000, &s, &tex, &r)
            .unwrap();
    }
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    for l in 0..3 {
        let lv = level(&s, l);
        let got = read_level(&tex, l, 0, lv.w, lv.h, lv.d);
        for z in 0..lv.d {
            for y in 0..lv.h {
                for x in 0..lv.w {
                    let i = (((z * lv.h + y) * lv.w + x) * BPB) as usize;
                    assert_eq!(
                        got[i..i + 4],
                        texel(&data, &s, l, 0, (x, y, z)),
                        "level {l} at {x},{y},{z}"
                    );
                }
            }
        }
    }
}

#[test]
fn sub_rectangle_copy_back_leaves_the_rest_alone() {
    let Some(st) = setup() else { return };
    let s = SurfaceLayout::single(BPB, 100, 70, 2, GobOrder::Tegra);
    let total = s.total_bytes().unwrap() as usize;
    let src = pattern(total, 3);
    st.guest.write(0x4000, &src).unwrap();
    st.guest.write(0x20000, &vec![0xEE; total]).unwrap();
    let tex = texture(&st.ctx, TextureKind::D2, &s);
    let (ds, sw) = (
        Deswizzler::new(&st.ctx, &st.cache).unwrap(),
        Swizzler::new(&st.ctx, &st.cache).unwrap(),
    );
    let mut ring = FrameRing::new(&st.ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    let mut pass = frame.commands().begin_compute_pass(ds.table()).unwrap();
    ds.encode_region(
        &mut pass,
        &scratch,
        &st.guest,
        0x4000,
        &s,
        &tex,
        &TextureRegion::whole(0, 0, [100, 70, 1]),
    )
    .unwrap();
    pass.end();
    let mut pass = frame.commands().begin_compute_pass(sw.table()).unwrap();
    pass.encode(&ComputeCommand::WaitForEarlierPasses).unwrap();
    let region = TextureRegion {
        level: 0,
        slice: 0,
        origin: [37, 13, 0],
        size: [29, 21, 1],
    };
    sw.encode_region(&mut pass, &scratch, &st.guest, 0x20000, &s, &tex, &region)
        .unwrap();
    assert!(
        sw.encode_region(
            &mut pass,
            &scratch,
            &st.guest,
            0x20000,
            &s,
            &tex,
            &TextureRegion {
                size: [64, 21, 1],
                ..region
            }
        )
        .is_err()
    );
    pass.end();
    frame.submit().unwrap().wait().unwrap();
    let mut back = vec![0u8; total];
    st.guest.read(0x20000, &mut back).unwrap();
    for y in 0..70 {
        for x in 0..100 {
            let o = at(&s, 0, 0, (x, y, 0));
            let inside = (37..66).contains(&x) && (13..34).contains(&y);
            let want = if inside {
                src[o..o + 4].to_vec()
            } else {
                vec![0xEE; 4]
            };
            assert_eq!(back[o..o + 4], want[..], "at {x},{y}");
        }
    }
}

#[test]
fn views_select_levels_and_slices() {
    let Some(st) = setup() else { return };
    let mut d = TextureDesc::compute_2d(16, 16, Format::Rgba8Unorm);
    d.kind = TextureKind::Cube;
    d.mip_levels = 3;
    d.allow_views = true;
    let cube = Texture::new(&st.ctx, d).unwrap();
    let face = cube
        .view(&nxkit_metal::ViewDesc {
            format: Format::R32Uint,
            kind: TextureKind::D2,
            first_level: 1,
            level_count: 1,
            first_slice: 4,
            slice_count: 1,
        })
        .unwrap();
    assert_eq!(
        (face.desc().width, face.desc().height, face.desc().kind),
        (8, 8, TextureKind::D2)
    );
    let bad = nxkit_metal::ViewDesc {
        format: Format::R32Uint,
        kind: TextureKind::D2,
        first_level: 3,
        level_count: 1,
        first_slice: 0,
        slice_count: 1,
    };
    assert!(cube.view(&bad).is_err());
    let mut square = TextureDesc::compute_2d(16, 8, Format::Rgba8Unorm);
    square.kind = TextureKind::Cube;
    assert!(Texture::new(&st.ctx, square).is_err());
}
