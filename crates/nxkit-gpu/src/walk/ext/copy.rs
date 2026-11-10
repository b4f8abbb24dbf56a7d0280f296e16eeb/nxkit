//! The detail of copies and clears.

use nxkit_nvn::decode::untag_address;

use super::*;

/// Calls `set` with the group of each face that an `NVNface` value names, starting at `first`.
pub(super) fn faces(face: u32, first: usize, set: &mut impl FnMut(usize)) {
    let bits = StencilValues::face_bits(face);
    for i in 0..2 {
        if bits >> i & 1 != 0 {
            set(first + i);
        }
    }
}

/// The level and first layer of a `NVNtextureView` in four words. Without a view the copy uses level 0 and layer 0.
pub(super) fn view_of(w: [u32; 4]) -> (u32, u32) {
    let flags = w[0] & 0xff;
    let level = if flags & 1 != 0 { w[0] >> 16 & 31 } else { 0 };
    let layer = if flags & 2 != 0 { w[1] >> 16 } else { 0 };
    (level, layer)
}

/// The origin and size of a `NVNcopyRegion` in six words. A negative value counts as 0 and a depth of 0 as 1.
pub(super) fn region_of(w: [u32; 6]) -> ([u32; 3], [u32; 3]) {
    let v = |i: usize| (w[i] as i32).max(0) as u32;
    ([v(0), v(1), v(2)], [v(3), v(4), v(5).max(1)])
}

/// The detail of a `ClearTexture` record: the argument is `kind | has_view << 8` and the words are the texture
/// address (2), the channel mask, the colour (4), the view (4, if there is one) and the region (6). A record that is
/// too short keeps the default entry.
pub(super) fn clear_detail(x: &Ext) -> CopyDetail {
    let has_view = x.arg >> 8 & 1 != 0;
    let at = 7 + if has_view { 4 } else { 0 };
    if x.words().len() < at + 6 {
        return CopyDetail::default();
    }
    let (level, layer) = if has_view {
        view_of([7, 8, 9, 10].map(|i| x.word(i)))
    } else {
        (0, 0)
    };
    let (origin, size) = region_of([0, 1, 2, 3, 4, 5].map(|i| x.word(at + i)));
    CopyDetail {
        kind: CopyKind::Clear,
        dst_level: level,
        dst_layer: layer,
        dst_origin: origin,
        size,
        dst_size: size,
        clear: [3, 4, 5, 6].map(|i| x.word(i)),
        clear_kind: u32::from(x.arg & 0xff),
        channel_mask: x.word(2),
        ..CopyDetail::default()
    }
}

/// The detail of a copy record with a texture side. The words are described at [`ExtCode::CopyBufferToTexture`] and
/// [`ExtCode::CopyTextureToTexture`]. A record that is too short for its words keeps the default entry.
pub(super) fn copy_detail(st: &State, x: &Ext) -> CopyDetail {
    let view = |at: usize| view_of([x.word(at), x.word(at + 1), x.word(at + 2), x.word(at + 3)]);
    let region = |at: usize| region_of([0, 1, 2, 3, 4, 5].map(|i| x.word(at + i)));
    let mut d = CopyDetail {
        row_stride: st.copy_strides[0],
        image_stride: st.copy_strides[1],
        ..CopyDetail::default()
    };
    if x.code == ExtCode::CopyTextureToTexture {
        let (has_src, has_dst) = (x.arg & 1 != 0, x.arg & 2 != 0);
        let src_at = 5 + if has_src { 4 } else { 0 };
        let dst_at = src_at + 6 + if has_dst { 4 } else { 0 };
        if x.words().len() < dst_at + 6 {
            return d;
        }
        let (src_level, src_layer) = if has_src { view(5) } else { (0, 0) };
        let (dst_level, dst_layer) = if has_dst { view(dst_at - 4) } else { (0, 0) };
        let (src_origin, size) = region(src_at);
        let (dst_origin, dst_size) = region(dst_at);
        return CopyDetail {
            kind: CopyKind::TextureToTexture,
            src_level,
            src_layer,
            src_origin,
            dst_level,
            dst_layer,
            dst_origin,
            size,
            dst_size,
            src_addr: untag_address(x.word64(0)),
            dst_addr: untag_address(x.word64(2)),
            ..d
        };
    }
    let (level, layer) = if x.arg & 1 != 0 { view(5) } else { (0, 0) };
    let at = if x.arg & 1 != 0 { 9 } else { 5 };
    if x.words().len() < at + 6 {
        return d;
    }
    let (origin, size) = region(at);
    d.buffer = x.word64(2);
    d.size = size;
    d.dst_size = size;
    if x.code == ExtCode::CopyBufferToTexture {
        d.kind = CopyKind::BufferToTexture;
        (d.src_addr, d.dst_addr) = (d.buffer, untag_address(x.word64(0)));
        (d.dst_level, d.dst_layer, d.dst_origin) = (level, layer, origin);
    } else {
        d.kind = CopyKind::TextureToBuffer;
        (d.src_addr, d.dst_addr) = (untag_address(x.word64(0)), d.buffer);
        (d.src_level, d.src_layer, d.src_origin) = (level, layer, origin);
    }
    d
}

/// The value of a stride setting. A negative stride is no stride.
pub(super) fn stride(x: &Ext) -> u64 {
    u64::try_from(x.word64(0) as i64).unwrap_or(0)
}

/// The detail of a `CopyBufferToBuffer` record (words: source (2), destination (2), size (2), flags). It has no box:
/// the addresses and the byte count. The flags change nothing the encoder does. A record that is too short for the
/// size keeps the default entry.
pub(super) fn buffer_copy_detail(x: &Ext) -> CopyDetail {
    if x.words().len() < 6 {
        return CopyDetail::default();
    }
    CopyDetail {
        kind: CopyKind::BufferToBuffer,
        src_addr: x.word64(0),
        dst_addr: x.word64(2),
        bytes: x.word64(4),
        ..CopyDetail::default()
    }
}

/// The detail of a `ClearBuffer` record (words: address (2), size (2), value). A record that is too short for the
/// value keeps the default entry.
pub(super) fn buffer_fill_detail(x: &Ext) -> CopyDetail {
    if x.words().len() < 5 {
        return CopyDetail::default();
    }
    CopyDetail {
        kind: CopyKind::BufferFill,
        dst_addr: x.word64(0),
        bytes: x.word64(2),
        value: x.word(4),
        ..CopyDetail::default()
    }
}
