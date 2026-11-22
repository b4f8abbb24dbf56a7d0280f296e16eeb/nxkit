//! Tests of the front end: objects from guest structs end to end, over a test memory and the recording backend.

use std::sync::Arc;

use crate::consts::{descriptor_type as dt, image_flags, image_usage, view_type};
use crate::desc::{Descriptor, MemoryId, SamplerId, TextureId, TextureKind};
use crate::format::PixelFormat;
use crate::handle::CountingSlots;
use crate::info::{
    self, ImageInfo, ImageViewInfo, RenderPassInfo, SetLayoutBindingInfo, SubresourceRange,
    TemplateEntry, WriteInfo, WriteItem,
};
use crate::layout::{
    descriptor_buffer_info as dbi, descriptor_image_info as dii, image_create_info as ic,
};
use crate::mem::{Mem, StructBytes, TestMemory};
use crate::recording::{Call, Recording};
use crate::result::VkError;

use super::Vk;

pub(super) fn vk() -> (Vk, Arc<Recording>, Arc<CountingSlots>) {
    let backend = Arc::new(Recording::default());
    let slots = Arc::new(CountingSlots::new(0x5000_0000));
    (Vk::new(backend.clone(), slots.clone()), backend, slots)
}

pub(super) fn image_2d(format: u32, w: u32, h: u32, usage: u32) -> ImageInfo {
    ImageInfo {
        image_type: 1,
        format,
        extent: [w, h, 1],
        levels: 1,
        layers: 1,
        samples: 1,
        usage,
        ..ImageInfo::default()
    }
}

#[test]
fn create_image_end_to_end_from_guest_memory() {
    let (vk, rec, _) = vk();
    let m = TestMemory::new(0x1_0000, 4096);
    // VkImageCreateInfo as the game writes it: a 256x128 RGBA8 2D image with 9 levels, sampled and a colour target.
    let ci = m.put(
        &StructBytes::new(ic::SIZE)
            .u32(ic::S_TYPE, 14)
            .u32(ic::IMAGE_TYPE, 1)
            .u32(ic::FORMAT, 37)
            .u32(ic::EXTENT, 256)
            .u32(ic::EXTENT + 4, 128)
            .u32(ic::EXTENT + 8, 1)
            .u32(ic::MIP_LEVELS, 9)
            .u32(ic::ARRAY_LAYERS, 1)
            .u32(ic::SAMPLES, 1)
            .u32(
                ic::USAGE,
                image_usage::SAMPLED | image_usage::COLOR_ATTACHMENT,
            )
            .0,
    );
    let out = m.alloc(8);
    let info = info::image(&m, ci);
    let h = info.and_then(|i| vk.create_image(&i));
    let h = h.unwrap_or(0);
    assert!(m.put_u64(out, h).is_ok());
    assert_eq!(m.u64(out), Ok(h));
    let calls = rec.take();
    let Some(Call::CreateImage(id, desc)) = calls.first() else {
        panic!("no image made: {calls:?}");
    };
    assert_eq!(*id, TextureId(h));
    assert_eq!(
        (desc.pixel, desc.kind, desc.levels, desc.extent),
        (PixelFormat::Rgba8Unorm, TextureKind::D2, 9, [256, 128, 1])
    );
    let req = vk.image_requirements(h);
    assert_eq!(
        req.map(|r| (r.alignment, r.type_bits, r.size % 4096)),
        Ok((4096, 7, 0))
    );
    vk.destroy_image(h);
    vk.destroy_image(h);
    vk.destroy_image(0);
    assert_eq!(rec.take(), vec![Call::DestroyTexture(TextureId(h))]);
    assert_eq!(vk.image_requirements(h), Err(VkError::Unknown));
}

#[test]
fn image_kinds_follow_the_reference_rules() {
    let (vk, rec, _) = vk();
    let one_d = ImageInfo {
        image_type: 0,
        extent: [512, 1, 1],
        ..image_2d(37, 512, 1, image_usage::COLOR_ATTACHMENT)
    };
    let cube = ImageInfo {
        layers: 12,
        flags: image_flags::CUBE_COMPATIBLE,
        ..image_2d(37, 64, 64, image_usage::SAMPLED)
    };
    let ds = image_2d(130, 64, 64, image_usage::DEPTH_STENCIL_ATTACHMENT);
    for i in [one_d, cube, ds] {
        assert!(vk.create_image(&i).is_ok());
    }
    let kinds: Vec<(TextureKind, bool)> = rec
        .take()
        .into_iter()
        .filter_map(|c| match c {
            Call::CreateImage(_, d) => Some((d.kind, d.format_views)),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            (TextureKind::D2, false),
            (TextureKind::CubeArray, false),
            (TextureKind::D2, true)
        ]
    );
    assert_eq!(
        vk.create_image(&image_2d(15, 4, 4, 0)),
        Err(VkError::FormatNotSupported)
    );
    // A converted format is for linear images only.
    assert_eq!(
        vk.create_image(&image_2d(5, 4, 4, 0)),
        Err(VkError::FormatNotSupported)
    );
    rec.fail_images(true);
    assert_eq!(
        vk.create_image(&image_2d(37, 4, 4, 0)),
        Err(VkError::OutOfDeviceMemory)
    );
}

#[test]
fn a_linear_image_aliases_its_memory_from_the_bind() {
    let (vk, rec, _) = vk();
    let linear = ImageInfo {
        tiling: 1,
        ..image_2d(37, 4, 4, image_usage::SAMPLED)
    };
    let img = vk.create_image(&linear).unwrap_or(0);
    assert!(rec.take().is_empty());
    let layout = vk.subresource_layout(img, 0, 0);
    assert_eq!(layout.map(|l| (l.row_pitch, l.size)), Ok((256, 1024)));
    let mem = vk.allocate_memory(4096, 1).unwrap_or(0);
    assert!(vk.bind_image_memory(img, mem, 512).is_ok());
    let calls = rec.take();
    assert_eq!(
        calls.first(),
        Some(&Call::CreateMemory(MemoryId(mem), 4096, 1))
    );
    let Some(Call::CreateImage(_, d)) = calls.get(1) else {
        panic!("no linear texture: {calls:?}");
    };
    assert_eq!(
        d.linear.map(|l| (l.memory, l.offset, l.row_pitch)),
        Some((MemoryId(mem), 512, 256))
    );
    // Mapping reuses the storage that the bind made.
    let p = vk.map_memory(mem, 16);
    assert!(p.is_ok_and(|p| p % 0x1000 == 16));
    assert!(rec.take().is_empty());
}

#[test]
fn memory_is_made_on_first_use_and_bad_types_fail() {
    let (vk, rec, _) = vk();
    assert_eq!(vk.allocate_memory(64, 3), Err(VkError::OutOfDeviceMemory));
    let mem = vk.allocate_memory(1 << 20, 2).unwrap_or(0);
    let buf = vk.create_buffer(&info::BufferInfo {
        size: 1000,
        usage: 0x80,
        flags: 0,
    });
    assert_eq!(
        vk.buffer_requirements(buf).map(|r| (r.size, r.alignment)),
        Ok((1024, 256))
    );
    assert!(rec.take().is_empty());
    assert!(vk.bind_buffer_memory(buf, mem, 256).is_ok());
    assert!(vk.bind_buffer_memory(buf, mem, 256).is_ok());
    assert_eq!(rec.take().len(), 1);
    vk.free_memory(mem);
    vk.free_memory(mem);
    assert_eq!(rec.take(), vec![Call::DestroyMemory(MemoryId(mem))]);
}

#[test]
fn views_resolve_ranges_formats_and_swizzles() {
    let (vk, rec, _) = vk();
    let ds = vk
        .create_image(&ImageInfo {
            levels: 4,
            layers: 2,
            ..image_2d(130, 64, 64, image_usage::SAMPLED)
        })
        .unwrap_or(0);
    let bc1 = vk
        .create_image(&image_2d(131, 64, 64, image_usage::SAMPLED))
        .unwrap_or(0);
    let stencil = ImageViewInfo {
        image: ds,
        view_type: view_type::D2_ARRAY,
        format: 130,
        range: SubresourceRange {
            aspect: 4,
            base_level: 1,
            levels: u32::MAX,
            base_layer: 0,
            layers: u32::MAX,
        },
        ..ImageViewInfo::default()
    };
    let whole = ImageViewInfo {
        image: bc1,
        view_type: view_type::D2,
        format: 131,
        range: SubresourceRange {
            aspect: 1,
            base_level: 0,
            levels: 1,
            base_layer: 0,
            layers: 1,
        },
        ..ImageViewInfo::default()
    };
    rec.take();
    assert!(vk.create_image_view(&stencil).is_ok());
    assert!(vk.create_image_view(&whole).is_ok());
    let views: Vec<_> = rec
        .take()
        .into_iter()
        .filter_map(|c| match c {
            Call::CreateView(_, _, d) => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(views.len(), 2);
    assert_eq!(
        (
            views[0].pixel,
            views[0].levels,
            views[0].layers,
            views[0].whole
        ),
        (PixelFormat::X32Stencil8, 3, 2, false)
    );
    // BC1 RGB reads alpha as one, so the view is not the image as it is.
    assert_eq!(
        (views[1].swizzle[3], views[1].whole),
        (crate::desc::Swizzle::One, false)
    );
    assert!(matches!(
        vk.create_image_view(&ImageViewInfo { image: 77, ..whole }),
        Err(VkError::Unknown)
    ));
}

#[test]
fn render_passes_with_more_subpasses_are_refused() {
    let (vk, _, _) = vk();
    let mut rp = RenderPassInfo {
        subpasses: 2,
        ..RenderPassInfo::default()
    };
    assert_eq!(vk.create_render_pass(&rp), Err(VkError::FeatureNotPresent));
    rp.subpasses = 1;
    assert!(vk.create_render_pass(&rp).is_ok());
}

fn binding(binding: u32, ty: u32, count: u32, immutable: Vec<u64>) -> SetLayoutBindingInfo {
    SetLayoutBindingInfo {
        binding,
        ty,
        count,
        stages: 0x11,
        immutable,
    }
}

#[test]
fn set_layouts_sort_bindings_and_count_dynamic_offsets() {
    let (vk, _, _) = vk();
    let l = vk.create_set_layout(&[
        binding(3, dt::UNIFORM_BUFFER_DYNAMIC, 2, vec![]),
        binding(0, dt::STORAGE_BUFFER_DYNAMIC, 1, vec![]),
        binding(1, dt::SAMPLED_IMAGE, 4, vec![]),
    ]);
    let l = l.unwrap_or(0);
    let s = vk.lock();
    let d = s.set_layouts.get(l).cloned().unwrap_or_default();
    drop(s);
    let order: Vec<(u32, u32, Option<u32>)> = d
        .bindings
        .iter()
        .map(|b| (b.binding, b.base, b.dynamic))
        .collect();
    assert_eq!(order, vec![(0, 0, Some(0)), (1, 1, None), (3, 5, Some(1))]);
    assert_eq!((d.total, d.dynamic_count), (7, 3));
    // Element 5 of binding 1 goes on into binding 3.
    assert_eq!(d.element(1, 5).map(|(i, b)| (i, b.binding)), Some((6, 3)));
    assert!(d.element(2, 0).is_none());
}

#[test]
fn template_writes_read_guest_data_and_keep_immutable_samplers() {
    let (vk, _, _) = vk();
    let m = TestMemory::new(0x2_0000, 4096);
    let smp = vk
        .create_sampler(&crate::desc::SamplerDesc::default())
        .unwrap_or(0);
    let other = vk
        .create_sampler(&crate::desc::SamplerDesc::default())
        .unwrap_or(0);
    let img = vk
        .create_image(&image_2d(37, 8, 8, image_usage::SAMPLED))
        .unwrap_or(0);
    let view = vk
        .create_image_view(&ImageViewInfo {
            image: img,
            view_type: view_type::D2,
            format: 37,
            range: SubresourceRange {
                aspect: 1,
                base_level: 0,
                levels: 1,
                base_layer: 0,
                layers: 1,
            },
            ..ImageViewInfo::default()
        })
        .unwrap_or(0);
    let mem = vk.allocate_memory(4096, 1).unwrap_or(0);
    let buf = vk.create_buffer(&info::BufferInfo {
        size: 1024,
        usage: 0x10,
        flags: 0,
    });
    assert!(vk.bind_buffer_memory(buf, mem, 512).is_ok());
    let layout = vk
        .create_set_layout(&[
            binding(0, dt::COMBINED_IMAGE_SAMPLER, 1, vec![smp]),
            binding(1, dt::UNIFORM_BUFFER, 1, vec![]),
        ])
        .unwrap_or(0);
    let pool = vk.create_descriptor_pool();
    let set = vk
        .allocate_sets(pool, &[layout])
        .unwrap_or_default()
        .first()
        .copied()
        .unwrap_or(0);
    // The game's data: an image info at 0 (with a sampler that the immutable one overrides), a buffer info at 32.
    let data = m.put(
        &StructBytes::new(56)
            .u64(dii::SAMPLER, other)
            .u64(dii::IMAGE_VIEW, view)
            .u64(32 + dbi::BUFFER, buf)
            .u64(32 + dbi::OFFSET, 64)
            .u64(32 + dbi::RANGE, u64::MAX)
            .0,
    );
    let entries = vec![
        TemplateEntry {
            binding: 0,
            element: 0,
            count: 1,
            ty: dt::COMBINED_IMAGE_SAMPLER,
            offset: 0,
            stride: 24,
        },
        TemplateEntry {
            binding: 1,
            element: 0,
            count: 1,
            ty: dt::UNIFORM_BUFFER,
            offset: 32,
            stride: 24,
        },
    ];
    let t = vk.create_update_template(0, entries);
    assert!(vk.update_set_with_template(&m, set, t, data).is_ok());
    let got = vk
        .lock()
        .sets
        .get(set)
        .map(|s| s.descriptors.as_ref().clone())
        .unwrap_or_default();
    assert_eq!(
        got[0],
        Descriptor::Image {
            view: TextureId(view),
            sampler: Some(SamplerId(smp))
        }
    );
    let Descriptor::Buffer(b) = got[1] else {
        panic!("not a buffer: {:?}", got[1]);
    };
    assert_eq!(
        (b.memory, b.offset, b.size),
        (MemoryId(mem), 512 + 64, 1024 - 64)
    );
    // A reset pool frees the set. A free of it afterwards is ignored.
    assert!(vk.reset_descriptor_pool(pool).is_ok());
    vk.free_sets(&[set]);
    assert!(vk.lock().sets.get(set).is_none());
}

#[test]
fn copies_take_the_source_but_keep_the_destination_sampler() {
    let (vk, _, _) = vk();
    let a = vk
        .create_sampler(&crate::desc::SamplerDesc::default())
        .unwrap_or(0);
    let b = vk
        .create_sampler(&crate::desc::SamplerDesc::default())
        .unwrap_or(0);
    let plain = vk
        .create_set_layout(&[binding(0, dt::COMBINED_IMAGE_SAMPLER, 2, vec![])])
        .unwrap_or(0);
    let fixed = vk
        .create_set_layout(&[binding(0, dt::COMBINED_IMAGE_SAMPLER, 2, vec![b, b])])
        .unwrap_or(0);
    let pool = vk.create_descriptor_pool();
    let sets = vk.allocate_sets(pool, &[plain, fixed]).unwrap_or_default();
    let write = WriteInfo {
        set: sets[0],
        binding: 0,
        element: 1,
        ty: dt::COMBINED_IMAGE_SAMPLER,
        items: vec![WriteItem::Image {
            sampler: a,
            view: 0,
        }],
    };
    vk.update_sets(
        &[write],
        &[info::CopyInfo {
            src: (sets[0], 0, 1),
            dst: (sets[1], 0, 0),
            count: 1,
        }],
    );
    let got = vk.lock().sets.get(sets[1]).map(|s| s.descriptors[0]);
    assert_eq!(
        got,
        Some(Descriptor::Image {
            view: TextureId(0),
            sampler: Some(SamplerId(b))
        })
    );
}
