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
