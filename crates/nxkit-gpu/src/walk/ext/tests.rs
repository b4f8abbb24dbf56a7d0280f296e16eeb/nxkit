//! The tests of the extension records.

use super::copy::{clear_detail, copy_detail};
use super::draw::{draw_args, mesh_args};
use super::*;
use nxkit_metal::AddressMap;
use nxkit_nvn::mem::VecMem;

#[test]
fn indirect_and_mesh_draw_records_give_their_arguments() {
    let draw = |kind: DrawKind| Ext::new(ExtCode::Draw, kind as u16);
    let x = draw(DrawKind::MultiElementsIndirectCount)
        .u32(4)
        .u32(2)
        .u64(0x1000)
        .u64(0x2000)
        .u64(0x3000)
        .u32(9)
        .u64(20);
    assert_eq!(
        draw_args(&x),
        Some((
            4,
            DrawArgs::MultiIndirectCount {
                index: Some((2, 0x1000)),
                indirect: 0x2000,
                count: 0x3000,
                max: 9,
                stride: 20
            }
        ))
    );
    let x = draw(DrawKind::MultiArraysIndirectCount)
        .u32(5)
        .u64(0x2000)
        .u64(0x3000)
        .u32(3)
        .u64(16);
    assert_eq!(
        draw_args(&x),
        Some((
            5,
            DrawArgs::MultiIndirectCount {
                index: None,
                indirect: 0x2000,
                count: 0x3000,
                max: 3,
                stride: 16
            }
        ))
    );
    let x = draw(DrawKind::ElementsIndirect)
        .u32(4)
        .u32(1)
        .u64(0x1000)
        .u64(0x2000);
    assert_eq!(
        draw_args(&x),
        Some((
            4,
            DrawArgs::IndexedIndirect {
                index_type: 1,
                address: 0x1000,
                indirect: 0x2000
            }
        ))
    );
    let x = draw(DrawKind::ArraysIndirect).u32(0).u64(0x2000);
    assert_eq!(
        draw_args(&x),
        Some((0, DrawArgs::ArraysIndirect { indirect: 0x2000 }))
    );
    let x = draw(DrawKind::MeshTasks).u32(7).u32(2048);
    assert_eq!(
        mesh_args(&x),
        Some(DrawArgs::MeshTasks {
            first: 7,
            count: 2048
        })
    );
    assert_eq!(draw_args(&x), None);
    let x = draw(DrawKind::MeshTasksIndirect).u64(0x4000);
    assert_eq!(
        mesh_args(&x),
        Some(DrawArgs::MeshTasksIndirect { indirect: 0x4000 })
    );
    assert_eq!(
        mesh_args(&draw(DrawKind::Arrays).u32(4).u32(0).u32(3)),
        None
    );
}

#[test]
fn uniform_snapshot_keeps_the_unchanged_tail() {
    let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
    let mut w = Walker::new(Default::default(), map);
    let mut mem = VecMem::new(0x1000, 128);
    mem.write(0x1070, &[1, 2, 3, 4]).unwrap();
    w.updated.push((0x2000, 0x2010));
    let a = w.snapshot(&mem, 0x2000, 128).unwrap();
    assert_eq!(a.len(), 128);
    assert_eq!(&a[112..116], &[1, 2, 3, 4]);
    assert!(Arc::ptr_eq(&a, &w.snapshot(&mem, 0x2000, 128).unwrap()));

    // CPU writes between queue submissions must not reuse the previous snapshot.
    mem.write(0x1070, &[5, 6, 7, 8]).unwrap();
    w.submit(&mut mem, 1, &[]);
    let b = w.snapshot(&mem, 0x2000, 128).unwrap();
    assert_eq!(&b[112..116], &[5, 6, 7, 8]);
    assert_eq!(&a[112..116], &[1, 2, 3, 4]);
}

#[test]
fn a_range_with_the_same_bytes_again_in_the_frame_shares_its_buffer() {
    let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
    let mut w = Walker::new(Default::default(), map);
    w.copy_all = true;
    let mut mem = VecMem::new(0x1000, 128);
    mem.write(0x1000, &[9, 9]).unwrap();
    let a = w.snapshot(&mem, 0x2000, 128).unwrap();
    // The next submit asks again with the same bytes: the same buffer.
    w.snaps.get_mut().clear();
    let b = w.snapshot(&mem, 0x2000, 128).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    // Other bytes give a new buffer, and the first one keeps its bytes.
    w.snaps.get_mut().clear();
    mem.write(0x1000, &[7, 7]).unwrap();
    let c = w.snapshot(&mem, 0x2000, 128).unwrap();
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!((&a[..2], &c[..2]), (&[9u8, 9][..], &[7u8, 7][..]));
    // A new frame starts again.
    w.take();
    w.snaps.get_mut().clear();
    let d = w.snapshot(&mem, 0x2000, 128).unwrap();
    assert!(!Arc::ptr_eq(&c, &d));
}

#[test]
fn the_buffer_of_a_dropped_frame_comes_back_in_order() {
    let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
    let mut w = Walker::new(Default::default(), map);
    w.copy_all = true;
    let mem = VecMem::new(0x1000, 128);
    let first = w.snapshot(&mem, 0x2000, 64).unwrap();
    let second = w.snapshot(&mem, 0x2040, 64).unwrap();
    let (p1, p2) = (first.as_ptr(), second.as_ptr());
    w.take();
    w.snaps.get_mut().clear();
    // The frame is still held, so these are new buffers.
    let busy = w.snapshot(&mem, 0x2000, 64).unwrap();
    assert!(busy.as_ptr() != p1 && busy.as_ptr() != p2);
    drop((first, second, busy));
    w.snaps.get_mut().clear();
    w.take();
    let again = w.snapshot(&mem, 0x2000, 64).unwrap();
    assert_eq!(again.as_ptr(), p1);
    let again = w.snapshot(&mem, 0x2040, 64).unwrap();
    assert_eq!(again.as_ptr(), p2);
}

#[test]
fn an_inline_update_invalidates_a_cached_unmodified_range() {
    let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
    let mut walker = Walker::new(Default::default(), map);
    let mut mem = VecMem::new(0x1000, 128);
    assert!(walker.snapshot(&mem, 0x2000, 128).is_none());
    assert!(walker.snaps.borrow().contains_key(&(0x2000, 128)));
    let update = Ext::new(ExtCode::UpdateUniform, 4)
        .u32(0x2000)
        .u32(0)
        .u32(128)
        .u32(0)
        .u32(16)
        .u32(0)
        .u32(0x12345678);
    walker.update_uniform(&mut mem, &update);
    let snapshot = walker.snapshot(&mem, 0x2000, 128).unwrap();
    assert_eq!(&snapshot[16..20], &0x12345678u32.to_le_bytes());
}

#[test]
fn unbound_vertex_inputs_get_a_constant_stream() {
    let mut d = empty_draw();
    d.attributes.push(AttributeDetail {
        location: 0,
        format: 0x0a,
        offset: 0,
        stream: 0,
    });
    d.streams.push(StreamDetail {
        index: 0,
        address: 0x2000,
        size: 128,
        stride: 4,
        divisor: 0,
    });
    let inputs = [
        (0, nxkit_metal::VertexFormat::Float4),
        (7, nxkit_metal::VertexFormat::UInt4),
    ];
    let layout = crate::translate::vertex_descriptor(&d, &inputs).unwrap();
    assert_eq!(layout.attributes.len(), 2);
    assert_eq!(
        layout.attributes[0].1.buffer,
        crate::translate::stream_slot(0)
    );
    assert_eq!(layout.attributes[1].0, 7);
    assert_eq!(
        layout.attributes[1].1.format,
        nxkit_metal::VertexFormat::UInt4
    );
    assert_eq!(
        layout.attributes[1].1.buffer,
        crate::translate::DEFAULT_VERTEX_SLOT
    );
    assert!(
        layout
            .layouts
            .iter()
            .any(|l| l.buffer == crate::translate::DEFAULT_VERTEX_SLOT
                && l.step == nxkit_metal::StepFunction::Constant)
    );
    // The declared attribute itself also needs the fallback when its stream is unbound.
    d.streams.clear();
    let layout = crate::translate::vertex_descriptor(&d, &inputs).unwrap();
    assert!(
        layout
            .attributes
            .iter()
            .all(|(_, a)| a.buffer == crate::translate::DEFAULT_VERTEX_SLOT)
    );
}

#[test]
fn a_stream_with_stride_0_gets_a_constant_step_with_a_stride() {
    let mut d = empty_draw();
    d.attributes.push(AttributeDetail {
        location: 0,
        format: 0x0a,
        offset: 8,
        stream: 0,
    });
    d.streams.push(StreamDetail {
        index: 0,
        address: 0x2000,
        size: 128,
        stride: 0,
        divisor: 0,
    });
    let inputs = [(0, nxkit_metal::VertexFormat::Float4)];
    let layout = crate::translate::vertex_descriptor(&d, &inputs).unwrap();
    let l = layout
        .layouts
        .iter()
        .find(|l| l.buffer == crate::translate::stream_slot(0))
        .unwrap();
    // Metal refuses a stride of 0 even for a constant step.
    assert_eq!(l.step, nxkit_metal::StepFunction::Constant);
    assert_eq!(l.stride, 24);
}

fn copy_ext(code: ExtCode, arg: u16, words: &[u32]) -> Ext {
    words.iter().fold(Ext::new(code, arg), |x, w| x.u32(*w))
}

#[test]
fn a_buffer_to_texture_copy_reads_the_buffer_view_and_region() {
    let st = State {
        copy_strides: [512, 4096],
        ..State::default()
    };
    // texture 0x2000, buffer 0x9_0000_1000, flags, view (levels, layers), region.
    let view = [1 | 2 | 3 << 16, 5 << 16, 0, 0];
    let region = [4u32, 8, 2, 120, 75, 3];
    let words: Vec<u32> = [0x2000, 0, 0x1000, 9, 0]
        .into_iter()
        .chain(view)
        .chain(region)
        .collect();
    let d = copy_detail(&st, &copy_ext(ExtCode::CopyBufferToTexture, 1, &words));
    assert_eq!(
        (d.kind, d.buffer),
        (CopyKind::BufferToTexture, 0x9_0000_1000)
    );
    assert_eq!((d.dst_level, d.dst_layer), (3, 5));
    assert_eq!(
        (d.dst_origin, d.size, d.dst_size),
        ([4, 8, 2], [120, 75, 3], [120, 75, 3])
    );
    assert_eq!((d.row_stride, d.image_stride), (512, 4096));
    // The addresses of both sides: the buffer is read, the texture (address 0x2000) is written.
    assert_eq!((d.src_addr, d.dst_addr), (0x9_0000_1000, 0x2000));

    // Without a view the region starts 4 words earlier and the copy uses level 0, layer 0.
    let words = [0x2000, 0, 0x1000, 9, 0, 0, 0, 0, 320, 180, 0];
    let d = copy_detail(&st, &copy_ext(ExtCode::CopyBufferToTexture, 0, &words));
    assert_eq!(
        (d.dst_level, d.dst_layer, d.dst_origin, d.size),
        (0, 0, [0; 3], [320, 180, 1])
    );
}

#[test]
fn a_texture_to_buffer_copy_keeps_the_texture_on_the_source_side() {
    let words = [0x2000, 0, 0x1000, 9, 0, 0, 0, 0, 16, 16, 1];
    let d = copy_detail(
        &State::default(),
        &copy_ext(ExtCode::CopyTextureToBuffer, 0, &words),
    );
    assert_eq!(d.kind, CopyKind::TextureToBuffer);
    assert_eq!(
        (d.src_origin, d.size, d.dst_origin),
        ([0; 3], [16, 16, 1], [0; 3])
    );
}

#[test]
fn a_texture_to_texture_copy_reads_both_views_and_regions() {
    let src_view = [1 | 2 << 16, 0, 0, 0];
    let dst_view = [1 | 2 | 1 << 16, 7 << 16, 0, 0];
    let words: Vec<u32> = [1, 0, 2, 0, 0]
        .into_iter()
        .chain(src_view)
        .chain([1, 2, 0, 64, 32, 1])
        .chain(dst_view)
        .chain([5, 6, 0, 64, 32, 1])
        .collect();
    let d = copy_detail(
        &State::default(),
        &copy_ext(ExtCode::CopyTextureToTexture, 3, &words),
    );
    assert_eq!(d.kind, CopyKind::TextureToTexture);
    assert_eq!((d.src_level, d.src_layer, d.src_origin), (2, 0, [1, 2, 0]));
    assert_eq!((d.dst_level, d.dst_layer, d.dst_origin), (1, 7, [5, 6, 0]));
    assert_eq!((d.size, d.dst_size), ([64, 32, 1], [64, 32, 1]));

    // Only the destination has a view.
    let words: Vec<u32> = [1, 0, 2, 0, 0, 0, 0, 0, 8, 8, 1]
        .into_iter()
        .chain(dst_view)
        .chain([0, 0, 0, 8, 8, 1])
        .collect();
    let d = copy_detail(
        &State::default(),
        &copy_ext(ExtCode::CopyTextureToTexture, 2, &words),
    );
    assert_eq!((d.src_level, d.dst_level, d.dst_layer), (0, 1, 7));
}

#[test]
fn a_clear_texture_record_reads_colour_mask_view_and_region() {
    let colour = [1.0f32.to_bits(), 0.5f32.to_bits(), 0, 0.25f32.to_bits()];
    let head = [0x2000, 0, 0xb];
    let words: Vec<u32> = head
        .into_iter()
        .chain(colour)
        .chain([1 | 2 | 2 << 16, 3 << 16, 0, 0])
        .chain([1, 2, 0, 30, 40, 1])
        .collect();
    let d = clear_detail(&copy_ext(ExtCode::ClearTexture, 1 << 8, &words));
    assert_eq!(d.kind, CopyKind::Clear);
    assert_eq!(
        (d.dst_level, d.dst_layer, d.dst_origin, d.size),
        (2, 3, [1, 2, 0], [30, 40, 1])
    );
    assert_eq!((d.clear, d.clear_kind, d.channel_mask), (colour, 0, 0xb));

    let words: Vec<u32> = head
        .into_iter()
        .chain([7, 0, 0, 0])
        .chain([0, 0, 0, 8, 8, 0])
        .collect();
    let d = clear_detail(&copy_ext(ExtCode::ClearTexture, 2, &words));
    assert_eq!(
        (d.clear_kind, d.clear[0], d.dst_level, d.size),
        (2, 7, 0, [8, 8, 1])
    );
    assert_eq!(
        clear_detail(&copy_ext(ExtCode::ClearTexture, 0, &head)),
        CopyDetail::default()
    );
}

#[test]
fn a_short_copy_record_keeps_an_empty_entry() {
    let d = copy_detail(
        &State::default(),
        &copy_ext(ExtCode::CopyBufferToTexture, 1, &[1, 0, 2, 0, 0]),
    );
    assert_eq!(d, CopyDetail::default());
}

#[test]
fn the_copy_strides_come_from_settings_and_reset_per_recording() {
    let mut st = State::default();
    let mut walker = Walker::new(
        Default::default(),
        AddressMap::new(0x1000, 0x2000, 128).unwrap(),
    );
    walker.setting(
        &mut st,
        &Ext::new(ExtCode::Setting, Setting::CopyRowStride as u16)
            .u32(256)
            .u32(0),
    );
    walker.setting(
        &mut st,
        &Ext::new(ExtCode::Setting, Setting::CopyImageStride as u16)
            .u32(0xffff_ffff)
            .u32(0xffff_ffff),
    );
    assert_eq!(st.copy_strides, [256, 0]);
}

/// The records that become `Cmd::Copy` or `Cmd::Downsample` in the decoder, with the entry that the walker keeps.
fn copy_like(tex: [u64; 2]) -> Vec<(Ext, CopyKind)> {
    let region = [0, 0, 0, 4, 4, 1];
    let t = |i: usize| [tex[i] as u32, (tex[i] >> 32) as u32];
    let join = |a: &[u32], b: &[u32]| a.iter().chain(b).copied().collect::<Vec<u32>>();
    vec![
        (
            copy_ext(ExtCode::ClearBuffer, 0, &[0x100, 0, 4, 0, 0]),
            CopyKind::Other,
        ),
        (
            copy_ext(
                ExtCode::CopyBufferToTexture,
                0,
                &join(&[t(0)[0], t(0)[1], 0x100, 0, 0], &region),
            ),
            CopyKind::BufferToTexture,
        ),
        (
            copy_ext(
                ExtCode::CopyBufferToBuffer,
                0,
                &[0x100, 0, 0x200, 0, 4, 0, 0],
            ),
            CopyKind::Other,
        ),
        (
            copy_ext(
                ExtCode::CopyTextureToBuffer,
                0,
                &join(&[t(0)[0], t(0)[1], 0x100, 0, 0], &region),
            ),
            CopyKind::TextureToBuffer,
        ),
        (
            copy_ext(ExtCode::UpdateUniform, 4, &[0x300, 0, 0, 0, 0, 0, 7]),
            CopyKind::Other,
        ),
        (
            copy_ext(
                ExtCode::CopyTextureToTexture,
                0,
                &join(
                    &[t(0)[0], t(0)[1], t(1)[0], t(1)[1], 0],
                    &[0, 0, 0, 4, 4, 1, 0, 0, 0, 4, 4, 1],
                ),
            ),
            CopyKind::TextureToTexture,
        ),
        // A downsample of an address that is no texture is dropped by the decoder and still has an entry.
        (
            copy_ext(ExtCode::DownsampleView, 0, &[0x7777, 0, 0x8888, 0]),
            CopyKind::Other,
        ),
        (
            copy_ext(
                ExtCode::DownsampleView,
                0,
                &[t(0)[0], t(0)[1], t(1)[0], t(1)[1]],
            ),
            CopyKind::Other,
        ),
        (
            copy_ext(
                ExtCode::ClearTexture,
                0,
                &join(&[t(1)[0], t(1)[1], 0xf, 0, 0, 0, 0], &region),
            ),
            CopyKind::Clear,
        ),
    ]
}

#[test]
fn the_walker_and_the_decoder_count_the_same_copies() {
    use nxkit_nvn::frame::Cmd;
    use nxkit_nvn::front::FrontEnd;
    use nxkit_nvn::layout::{Target, TextureDesc};
    use nxkit_nvn::record::Record;
    use nxkit_nvn::space::LinearSpace;

    const HOST: u64 = 0x7000_0000;
    const GPU: u64 = 0x15_0400_8000;
    const SIZE: u64 = 1 << 20;
    let mut f = FrontEnd::new(
        LinearSpace::new(HOST, GPU, SIZE).unwrap(),
        VecMem::new(HOST, SIZE as usize),
    );
    let desc = TextureDesc {
        target: Target::D2,
        format: 0x25,
        flags: 0,
        samples: 0,
        width: 4,
        height: 4,
        depth: 1,
        levels: 1,
        stride: 0,
        packaged_block: None,
    };
    let a = f.create_texture(desc, HOST + 0x1_0000).unwrap();
    let b = f.create_texture(desc, HOST + 0x2_0000).unwrap();
    let tex = [f.texture_address(a).unwrap(), f.texture_address(b).unwrap()];
    let list = copy_like(tex);

    f.add_command_memory(0, HOST + 0x8_0000, 4096).unwrap();
    f.begin_recording(0).unwrap();
    for (x, _) in &list {
        f.write(0, Record::Ext(*x)).unwrap();
    }
    let h = f.end_recording(0).unwrap();
    f.submit(h).unwrap();
    let cmds = f
        .frame()
        .cmds
        .iter()
        .filter(|c| matches!(c, Cmd::Copy { .. } | Cmd::Downsample { .. }))
        .count();

    let map = AddressMap::new(HOST, GPU, SIZE).unwrap();
    let mut walker = Walker::new(Default::default(), map);
    let mut mem = VecMem::new(HOST, SIZE as usize);
    let mut at = HOST + 0x8_0000;
    for rec in list
        .iter()
        .map(|(x, _)| Record::Ext(*x))
        .chain([Record::End])
    {
        let bytes = rec.encode().unwrap();
        mem.write(at, bytes.as_bytes()).unwrap();
        at += bytes.len() as u64;
    }
    walker.submit(
        &mut mem,
        1,
        &[nxkit_nvn::cmdbuf::CommandHandle(HOST + 0x8_0000)],
    );
    let detail = walker.take();
    assert_eq!(cmds, list.len());
    assert_eq!(detail.copies.len(), cmds);
    let kinds: Vec<_> = detail.copies.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, list.iter().map(|(_, k)| *k).collect::<Vec<_>>());
    let ords = crate::draws::ordinals(f.frame());
    assert_eq!(ords.last().map(|o| o.copy), Some(cmds as u32 - 1));
}
