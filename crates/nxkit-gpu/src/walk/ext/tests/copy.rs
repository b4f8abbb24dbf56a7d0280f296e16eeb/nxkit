//! Tests of the copy, clear and fill records and of the copy strides.

use super::*;

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
fn the_texture_addresses_of_a_copy_carry_no_id_tag() {
    // The record word of a texture has its id plus one above bit 40 (0x693 is texture 1682).
    let word = nxkit_nvn::decode::tag_address(0x0015_12c9_f000, nxkit_nvn::resource::TexId(1682));
    assert_eq!(word, 0x0006_9315_12c9_f000);
    let (lo, hi) = (word as u32, (word >> 32) as u32);
    let st = State::default();
    let words = [lo, hi, 0x1000, 0x15, 0, 0, 0, 0, 16, 16, 1];
    let d = copy_detail(&st, &copy_ext(ExtCode::CopyBufferToTexture, 0, &words));
    assert_eq!(
        (d.src_addr, d.dst_addr),
        (0x0015_0000_1000, 0x0015_12c9_f000)
    );
    let d = copy_detail(&st, &copy_ext(ExtCode::CopyTextureToBuffer, 0, &words));
    assert_eq!(
        (d.src_addr, d.dst_addr),
        (0x0015_12c9_f000, 0x0015_0000_1000)
    );
    let words = [lo, hi, lo, hi, 0, 0, 0, 0, 16, 16, 1, 0, 0, 0, 16, 16, 1];
    let d = copy_detail(&st, &copy_ext(ExtCode::CopyTextureToTexture, 0, &words));
    assert_eq!(
        (d.src_addr, d.dst_addr),
        (0x0015_12c9_f000, 0x0015_12c9_f000)
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
fn a_buffer_copy_record_reads_addresses_and_size() {
    // source 0x1_0000_1000, destination 0x2_0000_2000, size 0x1_0000_0010, flags 3.
    let words = [0x1000, 1, 0x2000, 2, 0x10, 1, 3];
    let d = buffer_copy_detail(&copy_ext(ExtCode::CopyBufferToBuffer, 0, &words));
    assert_eq!(d.kind, CopyKind::BufferToBuffer);
    assert_eq!(
        (d.src_addr, d.dst_addr, d.bytes),
        (0x1_0000_1000, 0x2_0000_2000, 0x1_0000_0010)
    );
    // Without the size the entry stays empty.
    let d = buffer_copy_detail(&copy_ext(ExtCode::CopyBufferToBuffer, 0, &words[..5]));
    assert_eq!(d, CopyDetail::default());
}

#[test]
fn a_clear_buffer_record_keeps_its_address_size_and_value() {
    let words = [0x3000, 4, 0x40, 0, 0xdead_beef];
    let d = buffer_fill_detail(&copy_ext(ExtCode::ClearBuffer, 0, &words));
    assert_eq!(d.kind, CopyKind::BufferFill);
    assert_eq!(
        (d.dst_addr, d.bytes, d.value),
        (0x4_0000_3000, 0x40, 0xdead_beef)
    );
    assert_eq!(d.src_addr, 0);
    let d = buffer_fill_detail(&copy_ext(ExtCode::ClearBuffer, 0, &words[..4]));
    assert_eq!(d, CopyDetail::default());
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
            CopyKind::BufferFill,
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
            CopyKind::BufferToBuffer,
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
        .filter(|c| {
            matches!(
                c,
                Cmd::Copy { .. } | Cmd::BufferCopy | Cmd::Downsample { .. }
            )
        })
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
