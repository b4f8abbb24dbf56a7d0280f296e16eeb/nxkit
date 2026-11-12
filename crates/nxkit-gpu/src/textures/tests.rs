//! Tests for the texture store.

use super::*;
use nxkit_nvn::{resource::ResourceTable, space::GpuAddr};

#[test]
fn attachment_views_write_distinct_mips_and_layers() {
    use nxkit_metal::{
        ArgumentTable, ColorAttachment, FrameRing, LoadAction, RenderPassDesc, StoreAction,
    };
    use nxkit_nvn::frame::TargetView;
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let desc = nxkit_nvn::layout::TextureDesc {
        target: Target::D2Array,
        format: 0x25,
        flags: 0,
        samples: 0,
        width: 16,
        height: 16,
        depth: 2,
        levels: 2,
        stride: 0,
        packaged_block: None,
    };
    let mut table = ResourceTable::new();
    let id = table.create(desc, GpuAddr::new(0x2000).unwrap()).unwrap();
    let mut store = TextureStore::new(&ctx);
    let res = store
        .ensure(
            table.get(id).unwrap(),
            1.0,
            Use {
                attachment: true,
                ..Use::default()
            },
            &mut EncodeStats::default(),
        )
        .unwrap();
    let first = store
        .target_view(&res, TargetView::from_words([3, 0, 1, 0]))
        .unwrap();
    let second = store
        .target_view(&res, TargetView::from_words([0x00010003, 1 << 16, 1, 0]))
        .unwrap();
    assert_eq!((first.desc().width, second.desc().width), (16, 8));
    let table = ArgumentTable::new(&ctx, 1, 1).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();
    let mut frame = ring.begin().unwrap();
    for (texture, clear) in [(&first, [1., 0., 0., 1.]), (&second, [0., 1., 0., 1.])] {
        frame
            .commands()
            .begin_render_pass(
                &RenderPassDesc {
                    colors: vec![Some(ColorAttachment {
                        depth_plane: 0,
                        texture: texture.clone(),
                        load: LoadAction::Clear,
                        store: StoreAction::Store,
                        clear,
                        memoryless: false,
                    })],
                    depth: None,
                    stencil: None,
                },
                &table,
            )
            .unwrap()
            .end();
    }
    frame.submit().unwrap().wait().unwrap();
    assert_eq!(
        first.debug_read(&ctx, &[[0, 0]]).unwrap()[0],
        [255, 0, 0, 255]
    );
    assert_eq!(
        second.debug_read(&ctx, &[[0, 0]]).unwrap()[0],
        [0, 255, 0, 255]
    );
    assert_eq!(
        store
            .target_view(&res, TargetView::from_words([3, 0, 1, 0]))
            .unwrap()
            .gpu_resource_id(),
        first.gpu_resource_id()
    );
}

#[test]
fn repeated_objects_over_the_same_storage_share_one_backing() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let desc = nxkit_nvn::layout::TextureDesc {
        target: Target::D2,
        format: 0x25,
        flags: 0,
        samples: 0,
        width: 64,
        height: 64,
        depth: 1,
        levels: 1,
        stride: 0,
        packaged_block: None,
    };
    let mut table = ResourceTable::new();
    let mut store = TextureStore::new(&ctx);
    let mut stats = EncodeStats::default();
    let mut first = None;
    let mut ids = Vec::new();
    for _ in 0..1000 {
        let id = table.create(desc, GpuAddr::new(0x2000).unwrap()).unwrap();
        let res = store
            .ensure(table.get(id).unwrap(), 1.0, Use::default(), &mut stats)
            .unwrap();
        let key = (res.tex.gpu_resource_id(), res.generation, res.barrier_id);
        if let Some(want) = first {
            assert_eq!(key, want);
        } else {
            first = Some(key);
        }
        ids.push(id);
    }
    assert_eq!(stats.textures_created, 1);
    assert_eq!(store.aliases.len(), 1);
    let different = table
        .create(
            nxkit_nvn::layout::TextureDesc {
                format: 0x27,
                ..desc
            },
            GpuAddr::new(0x2000).unwrap(),
        )
        .unwrap();
    let r = store
        .ensure(
            table.get(different).unwrap(),
            1.0,
            Use::default(),
            &mut stats,
        )
        .unwrap();
    assert_ne!(r.tex.gpu_resource_id(), first.unwrap().0);
    assert_eq!(stats.textures_created, 2);
    for id in ids {
        store.invalidate(id);
    }
    store.invalidate(different);
    store.trim_views();
    assert!(store.aliases.is_empty());
}

fn bc4_3d() -> TextureDesc {
    TextureDesc {
        kind: TextureKind::D3,
        width: 64,
        height: 64,
        depth: 64,
        mip_levels: 1,
        array_layers: 1,
        allow_views: true,
        format: Format::Bc(nxkit_metal::BcKind::Bc4Unorm),
        shader_read: true,
        shader_write: false,
        render_target: false,
        storage: Storage::Private,
        shader_atomic: false,
    }
}

#[test]
fn a_block_view_is_an_uncompressed_format_of_the_block_size() {
    let bc4 = Format::Bc(nxkit_metal::BcKind::Bc4Unorm);
    let bc7 = Format::Bc(nxkit_metal::BcKind::Bc7);
    // NVN 0x2C is RGBA16Uint (8 bytes, a BC4 block), 0x2F RGBA32Uint (16 bytes, a BC7 block).
    assert_eq!(
        blocks::block_view_format(bc4, Some(0x2C)),
        Some(Format::Rgba16Uint)
    );
    assert_eq!(
        blocks::block_view_format(bc7, Some(0x2F)),
        Some(Format::Rgba32Uint)
    );
    assert_eq!(blocks::block_view_format(bc4, Some(0x2F)), None);
    assert_eq!(blocks::block_view_format(bc4, None), None);
    assert_eq!(blocks::block_view_format(bc4, Some(0x49)), None);
    assert_eq!(
        blocks::block_view_format(Format::Rg32Uint, Some(0x2C)),
        None
    );
}

#[test]
fn a_block_shadow_has_one_texel_per_block_of_one_plane() {
    let d = bc4_3d();
    let s = blocks::shadow_desc(&d, 0, Format::Rgba16Uint);
    assert_eq!(
        (s.kind, s.width, s.height, s.depth),
        (TextureKind::D2, 16, 16, 1)
    );
    assert_eq!((s.mip_levels, s.array_layers), (1, 1));
    assert!(s.render_target && !s.format.is_compressed());
    let r = blocks::block_region(&d, 0, 5);
    assert_eq!(
        (r.level, r.slice, r.origin, r.size),
        (0, 0, [0, 0, 5], [64, 64, 1])
    );
    let mut a = d;
    a.kind = TextureKind::D2Array;
    a.depth = 1;
    a.array_layers = 3;
    a.mip_levels = 2;
    let s = blocks::shadow_desc(&a, 1, Format::Rgba16Uint);
    assert_eq!((s.width, s.height), (8, 8));
    let r = blocks::block_region(&a, 1, 2);
    assert_eq!(
        (r.level, r.slice, r.origin, r.size),
        (1, 2, [0; 3], [32, 32, 1])
    );
}

#[test]
fn compressed_textures_are_never_render_targets() {
    let desc = nxkit_nvn::layout::TextureDesc {
        target: Target::D3,
        format: 0x49,
        flags: 0,
        samples: 0,
        width: 64,
        height: 64,
        depth: 64,
        levels: 1,
        stride: 0,
        packaged_block: None,
    };
    let mut table = ResourceTable::new();
    let id = table.create(desc, GpuAddr::new(0x2000).unwrap()).unwrap();
    let u = Use {
        attachment: true,
        ..Use::default()
    };
    let (d, _) = describe(table.get(id).unwrap(), 1.0, u).unwrap();
    assert!(!d.render_target);
    assert_eq!(d.kind, TextureKind::D3);
}

#[test]
fn a_texture_that_another_one_overwrote_is_superseded() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let small = nxkit_nvn::layout::TextureDesc {
        target: Target::D2,
        format: 0x25,
        flags: 0,
        samples: 0,
        width: 64,
        height: 64,
        depth: 1,
        levels: 1,
        stride: 0,
        packaged_block: None,
    };
    let big = nxkit_nvn::layout::TextureDesc {
        width: 128,
        height: 128,
        ..small
    };
    let mut table = ResourceTable::new();
    let mut store = TextureStore::new(&ctx);
    let mut stats = EncodeStats::default();
    let addr = GpuAddr::new(0x2000).unwrap();
    let first = table.create(small, addr).unwrap();
    let second = table.create(big, addr).unwrap();
    let old = store
        .ensure(table.get(first).unwrap(), 1.0, Use::default(), &mut stats)
        .unwrap();
    store.end_frame();
    // The second texture covers the range of the first and a frame writes it after the last use of the first.
    store
        .ensure(table.get(second).unwrap(), 1.0, Use::default(), &mut stats)
        .unwrap();
    store.end_frame();
    store.mark_written(second);
    let mut dropped = 0;
    while store.frame < evict::SCAN_EVERY {
        dropped += store.end_frame();
    }
    assert_eq!(dropped, 1);
    assert!(store.get(first).is_none());
    assert!(store.get(second).is_some());
    assert_eq!(store.take_retired().len(), 1);
    assert_eq!(store.sup.dropped.count, 1);
    // No usage of a generation that no entry holds.
    let live: FastSet<u64> = store.entries.values().map(|r| r.generation).collect();
    assert!(store.usage.keys().all(|g| live.contains(g)));
    assert!(!store.usage.contains_key(&old.generation));
    // The first id gets a new texture when it is used again, and the store counts it.
    let again = store
        .ensure(table.get(first).unwrap(), 1.0, Use::default(), &mut stats)
        .unwrap();
    assert_ne!(again.generation, old.generation);
    assert_eq!(store.sup.remade, 1);
}

#[test]
fn a_texture_that_is_only_used_as_gpu_newer_supersedes_nothing() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let small = nxkit_nvn::layout::TextureDesc {
        target: Target::D2,
        format: 0x25,
        flags: 0,
        samples: 0,
        width: 64,
        height: 64,
        depth: 1,
        levels: 1,
        stride: 0,
        packaged_block: None,
    };
    let big = nxkit_nvn::layout::TextureDesc {
        width: 128,
        height: 128,
        ..small
    };
    let mut table = ResourceTable::new();
    let mut store = TextureStore::new(&ctx);
    let mut stats = EncodeStats::default();
    let addr = GpuAddr::new(0x2000).unwrap();
    let first = table.create(small, addr).unwrap();
    let second = table.create(big, addr).unwrap();
    store
        .ensure(table.get(first).unwrap(), 1.0, Use::default(), &mut stats)
        .unwrap();
    table.gpu_wrote(second).unwrap();
    // The state "the GPU copy is newer" is no write event: `mark_written` is never called.
    let mut dropped = 0;
    while store.frame < evict::SCAN_EVERY {
        store
            .ensure(table.get(second).unwrap(), 1.0, Use::default(), &mut stats)
            .unwrap();
        dropped += store.end_frame();
    }
    assert_eq!(dropped, 0);
    assert!(store.get(first).is_some() && store.get(second).is_some());
    assert_eq!(store.sup.dropped.count, 0);
}

/// A pitch-linear RGBA8 texture of 16 x 8 texels with rows `stride` bytes apart.
fn linear_rgba8(stride: u32) -> nxkit_nvn::layout::TextureDesc {
    nxkit_nvn::layout::TextureDesc {
        target: Target::D2,
        format: 0x25,
        flags: nxkit_nvn::layout::FLAG_LINEAR,
        samples: 0,
        width: 16,
        height: 8,
        depth: 1,
        levels: 1,
        stride,
        packaged_block: None,
    }
}

/// A store, guest memory of 1 MiB and the pools of the whole mode over it, set as the pools of the frame.
fn store_over_guest(ctx: &Context) -> (TextureStore, std::sync::Arc<nxkit_metal::GuestMemory>) {
    let guest = std::sync::Arc::new(nxkit_metal::GuestMemory::allocate(ctx, 1024 * 1024).unwrap());
    let pools = std::sync::Arc::new(crate::guestpools::GuestPools::whole(
        nxkit_metal::GUEST_VA_BASE,
        guest.len(),
        guest.clone(),
    ));
    let mut store = TextureStore::new(ctx);
    store.set_pools(&pools);
    (store, guest)
}

#[test]
fn a_linear_texture_reads_the_bytes_the_cpu_writes_later_without_an_upload() {
    // Super Mario Bros. Wonder rasterizes glyphs on the CPU into a linear R8 atlas in a CPU-visible pool and never
    // flushes the pool: with an upload only at the first use, glyphs drawn later stayed empty.
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let (mut store, guest) = store_over_guest(&ctx);
    let mut table = ResourceTable::new();
    let id = table
        .create(linear_rgba8(64), GpuAddr::new(nxkit_metal::GUEST_VA_BASE + 0x1000).unwrap())
        .unwrap();
    let mut stats = EncodeStats::default();
    let res = store
        .ensure(table.get(id).unwrap(), 1.0, Use::default(), &mut stats)
        .unwrap();
    assert_eq!(res.in_place, Some(InPlace { real: guest.real_base() + 0x1000, stride: 64 }));
    assert_eq!(stats.textures_in_place, 1);
    assert!(!TextureStore::needs_upload(&res, table.get(id).unwrap()));
    // The CPU writes texel (3, 2) after the texture was made; the record stays `GuestNewer` or `InSync` as it was.
    guest.write(0x1000 + 2 * 64 + 3 * 4, &[1, 2, 3, 4]).unwrap();
    let got = res.tex.debug_read_raw(&ctx, &[[3, 2]]).unwrap();
    assert_eq!(got[0], [1, 2, 3, 4]);
    guest.write(0x1000 + 2 * 64 + 3 * 4, &[9, 8, 7, 6]).unwrap();
    let got = res.tex.debug_read_raw(&ctx, &[[3, 2]]).unwrap();
    assert_eq!(got[0], [9, 8, 7, 6]);
    // The next frame keeps the same texture.
    table.synced(id).unwrap();
    let again = store
        .ensure(table.get(id).unwrap(), 1.0, Use::default(), &mut stats)
        .unwrap();
    assert_eq!(again.generation, res.generation);
    assert!(!TextureStore::needs_upload(&again, table.get(id).unwrap()));
}

#[test]
fn a_linear_texture_with_a_copy_is_uploaded_in_every_frame_until_the_gpu_writes_it() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    // A stride Metal cannot take for a linear texture (not a multiple of 16), and a store without pools.
    let (mut misaligned, _guest) = store_over_guest(&ctx);
    let mut plain = TextureStore::new(&ctx);
    for (store, stride) in [(&mut misaligned, 72), (&mut plain, 64)] {
        let mut table = ResourceTable::new();
        let id = table
            .create(linear_rgba8(stride), GpuAddr::new(nxkit_metal::GUEST_VA_BASE + 0x1000).unwrap())
            .unwrap();
        let mut stats = EncodeStats::default();
        let res = store
            .ensure(table.get(id).unwrap(), 1.0, Use::default(), &mut stats)
            .unwrap();
        assert_eq!((res.in_place, res.reupload, stats.textures_in_place), (None, true, 0), "stride {stride}");
        table.synced(id).unwrap();
        let res = store
            .ensure(table.get(id).unwrap(), 1.0, Use::default(), &mut stats)
            .unwrap();
        assert!(!res.fresh);
        assert!(TextureStore::needs_upload(&res, table.get(id).unwrap()), "stride {stride}");
        table.gpu_wrote(id).unwrap();
        assert!(!TextureStore::needs_upload(&res, table.get(id).unwrap()), "stride {stride}");
    }
    // A block-linear texture in the same state keeps its copy until the CPU tells.
    let mut table = ResourceTable::new();
    let id = table
        .create(
            nxkit_nvn::layout::TextureDesc { flags: 0, stride: 0, ..linear_rgba8(0) },
            GpuAddr::new(nxkit_metal::GUEST_VA_BASE + 0x1000).unwrap(),
        )
        .unwrap();
    let mut stats = EncodeStats::default();
    plain.ensure(table.get(id).unwrap(), 1.0, Use::default(), &mut stats).unwrap();
    table.synced(id).unwrap();
    let res = plain.ensure(table.get(id).unwrap(), 1.0, Use::default(), &mut stats).unwrap();
    assert!(!res.reupload && !TextureStore::needs_upload(&res, table.get(id).unwrap()));
}

#[test]
fn a_linear_attachment_keeps_its_copy() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let (mut store, _guest) = store_over_guest(&ctx);
    let mut table = ResourceTable::new();
    let id = table
        .create(linear_rgba8(64), GpuAddr::new(nxkit_metal::GUEST_VA_BASE + 0x1000).unwrap())
        .unwrap();
    let u = Use { attachment: true, ..Use::default() };
    store.note(id, u);
    let res = store
        .ensure(table.get(id).unwrap(), 1.0, u, &mut EncodeStats::default())
        .unwrap();
    assert_eq!((res.in_place, res.reupload), (None, false));
}
