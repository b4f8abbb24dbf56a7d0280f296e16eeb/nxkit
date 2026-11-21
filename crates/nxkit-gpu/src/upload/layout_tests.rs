//! Tests of the uploads through a page table, of padded sparse layouts and of texture buffers.

use super::tests::{round_trip, whole};
use super::*;
use nxkit_metal::{ArchiveConfig, FrameRing};
use nxkit_nvn::{resource::ResourceTable, space::GpuAddr};

#[test]
fn a_texture_of_a_virtual_pool_is_uploaded_through_its_page_table() {
    let ctx = match Context::system_default() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            return;
        }
    };
    let guest = Arc::new(GuestMemory::allocate(&ctx, 1024 * 1024).unwrap());
    let va = |off: u64| GpuAddr::new(nxkit_metal::GUEST_VA_BASE + off).unwrap();
    let desc = nxkit_nvn::layout::TextureDesc {
        target: Target::D2,
        format: 0x06,
        flags: 0,
        samples: 0,
        width: 64,
        height: 64,
        depth: 1,
        levels: 1,
        stride: 0,
        packaged_block: None,
    };
    let (window, len) = virtual_window(nxkit_metal::GUEST_VA_BASE, guest.len()).unwrap();
    let mut space = nxkit_nvn::vspace::VirtualSpace::new(window, len).unwrap();
    let base = space.reserve(0x2_0000).unwrap();
    let mut resources = ResourceTable::new();
    let sparse = resources.create(desc, base).unwrap();
    let rec = *resources.get(sparse).unwrap();
    let size = rec.layout.size as usize;
    assert_eq!(size, 0x2000);
    // The first page of the texture comes from physical 0x10000, the second stays unmapped.
    let pattern: Vec<u8> = (0..0x1000).map(|i| (i * 13 + i / 97) as u8 | 1).collect();
    guest.write(0x1_0000, &pattern).unwrap();
    guest.write(0x1_1000, &[0xEE; 0x1000]).unwrap();
    space.map(base, 0, 0x1000, Some(va(0x1_0000))).unwrap();
    let pools = whole(&guest).with_vpools(space.snapshot());
    // The same texture over plain guest memory takes the copy-back of what the sparse one holds.
    let plain = resources.create(desc, va(0x4_0000)).unwrap();
    let plain = *resources.get(plain).unwrap();
    let mut store = crate::textures::TextureStore::new(&ctx);
    let res = store
        .ensure(
            &rec,
            1.0,
            crate::textures::Use::default(),
            &mut Default::default(),
        )
        .unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let transfer = Transfer::new(&ctx, &cache, GobOrder::Tegra).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    transfer
        .upload(frame.commands(), &scratch, &pools, &rec, &res, false)
        .unwrap();
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    // A copy-back of the sparse texture itself has no guest bytes to go to.
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    let results = transfer.copy_back_many(
        frame.commands(),
        &scratch,
        &pools,
        &[(&rec, &res, &Dirty::All), (&plain, &res, &Dirty::All)],
    );
    assert!(results[0].is_err(), "{results:?}");
    assert!(results[1].is_ok(), "{results:?}");
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    let mut out = vec![0xAA; size];
    guest.read(0x4_0000, &mut out).unwrap();
    assert_eq!(&out[..0x1000], &pattern[..]);
    assert!(
        out[0x1000..].iter().all(|&b| b == 0),
        "the unmapped page is not 0"
    );
}

#[test]
fn a_sparse_texture_is_read_with_its_tiled_levels_padded_to_whole_tiles() {
    let ctx = match Context::system_default() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: no usable Metal 4 device ({e})");
            return;
        }
    };
    let guest = Arc::new(GuestMemory::allocate(&ctx, 4 * 1024 * 1024).unwrap());
    let pools = whole(&guest);
    let va = |off: u64| GpuAddr::new(nxkit_metal::GUEST_VA_BASE + off).unwrap();
    // RGBA8: a 64 KiB tile is 128x128 texels (8 GOBs of 16-GOB blocks). 192 texels are 12 GOBs, padded to 16 in the
    // tiled level 0; level 1 (96 texels) is the mip tail and keeps its 6 GOBs.
    let sparse = nxkit_nvn::layout::TextureDesc {
        target: Target::D2,
        format: 0x25,
        flags: nxkit_nvn::layout::FLAG_SPARSE,
        samples: 0,
        width: 192,
        height: 256,
        depth: 1,
        levels: 2,
        stride: 0,
        packaged_block: None,
    };
    assert_eq!(sparse.level_gobs(0).unwrap(), 16);
    let plain = nxkit_nvn::layout::TextureDesc { flags: 0, ..sparse };
    let mut resources = ResourceTable::new();
    let id = resources.create(sparse, va(0x1_0000)).unwrap();
    let rec = *resources.get(id).unwrap();
    let id = resources.create(plain, va(0x20_0000)).unwrap();
    let back = *resources.get(id).unwrap();
    // The guest bytes: each level's texels, swizzled by the CPU reference of the guest layout.
    let texels = |level: u32| -> Vec<u8> {
        let (w, h) = (192 >> level, 256 >> level);
        (0..w * h * 4)
            .map(|i| (i * 7 + i / 509 + level * 3) as u8)
            .collect()
    };
    let mut storage = vec![0u8; rec.layout.size as usize];
    for level in 0..2 {
        let pitch = (192 >> level) * 4;
        nxkit_nvn::swizzle::swizzle_level(
            &sparse,
            nxkit_nvn::swizzle::GobOrder::Tegra,
            (level, 0),
            &texels(level),
            pitch as usize,
            &mut storage,
        )
        .unwrap();
    }
    guest.write(0x1_0000, &storage).unwrap();
    let mut store = crate::textures::TextureStore::new(&ctx);
    let res = store
        .ensure(
            &rec,
            1.0,
            crate::textures::Use::default(),
            &mut Default::default(),
        )
        .unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let transfer = Transfer::new(&ctx, &cache, GobOrder::Tegra).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    transfer
        .upload(frame.commands(), &scratch, &pools, &rec, &res, false)
        .unwrap();
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    // Copied back into the plain layout and read with the CPU reference, each level is the image it started as.
    let mut frame = ring.begin().unwrap();
    let scratch = frame.scratch();
    let results = transfer.copy_back_many(
        frame.commands(),
        &scratch,
        &pools,
        &[(&back, &res, &Dirty::All)],
    );
    assert!(results[0].is_ok(), "{results:?}");
    ctx.commit_residency();
    frame.submit().unwrap().wait().unwrap();
    let mut out = vec![0u8; back.layout.size as usize];
    guest.read(0x20_0000, &mut out).unwrap();
    for level in 0..2 {
        let pitch = (192 >> level) * 4;
        let mut lin = vec![0u8; texels(level).len()];
        nxkit_nvn::swizzle::deswizzle_level(
            &plain,
            nxkit_nvn::swizzle::GobOrder::Tegra,
            (level, 0),
            &out,
            &mut lin,
            pitch as usize,
        )
        .unwrap();
        assert!(lin == texels(level), "level {level} differs");
    }
}

#[test]
fn a_texture_buffer_goes_up_and_back_as_one_row() {
    texture_buffer_round_trip(300);
}

#[test]
fn a_long_texture_buffer_goes_up_and_back_in_rows_of_16384_texels() {
    // two full rows and a short one
    texture_buffer_round_trip(40_000);
}

fn texture_buffer_round_trip(width: u32) {
    let desc = nxkit_nvn::layout::TextureDesc {
        target: Target::Buffer,
        format: 0x25,
        flags: 0,
        samples: 0,
        width,
        height: 1,
        depth: 1,
        levels: 1,
        stride: 0,
        packaged_block: None,
    };
    let Some((pattern, out, rec)) = round_trip(desc, &Dirty::All) else {
        return;
    };
    assert_eq!(rec.layout.size, u64::from(width) * 4);
    assert!(out == pattern, "the texels differ");
}
