//! The parts of an AV1 stream that the decoder needs before the first frame: the sequence header and the `av1C` atom.
//! The sequence header comes from the stream (AV1 specification, section 5.5), because the decoder API of the SDK takes
//! no configuration for AV1 and every key frame carries one. The pictures of the software decoder become NV12 in
//! [`crate::planar`].

/// What the sequence header of an AV1 stream says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sequence {
    /// 0 main, 1 high, 2 professional.
    pub profile: u8,
    /// `seq_level_idx[0]`.
    pub level: u8,
    /// `seq_tier[0]`.
    pub tier: u8,
    /// 8, 10 or 12.
    pub bit_depth: u8,
    /// The stream has no chroma.
    pub mono: bool,
    /// Chroma is halved across.
    pub subsampling_x: bool,
    /// Chroma is halved down.
    pub subsampling_y: bool,
    /// `chroma_sample_position`.
    pub chroma_position: u8,
    /// `matrix_coefficients`: 1 BT.709, 2 unspecified, 5 and 6 BT.601, 9 and 10 BT.2020.
    pub matrix: u8,
    /// The samples use the full range.
    pub full_range: bool,
    /// The largest frame width.
    pub max_width: u32,
    /// The largest frame height.
    pub max_height: u32,
    /// The OBU of the header as it came, with its header and its size field.
    pub obu: Vec<u8>,
}

impl Sequence {
    /// Whether the stream is 8-bit 4:2:0, the layout that the hardware decoder gives as NV12.
    pub(crate) fn is_nv12(&self) -> bool {
        self.bit_depth == 8 && !self.mono && self.subsampling_x && self.subsampling_y
    }

    /// The `nv12-colorspace` of the SDK: 1 BT.601 limited, 2 BT.601 full, 3 BT.709 limited, 4 BT.709 full. A stream that
    /// leaves the matrix unspecified is BT.709 from 720 lines up, as for VP9.
    pub(crate) fn nv12_colorspace(&self, height: u32) -> u32 {
        nv12_colorspace(self.matrix, self.full_range, height)
    }

    /// The payload of the `av1C` atom (AV1 Codec ISO Media File Format Binding, section 2.3): four bytes of the header,
    /// then the sequence header OBU.
    pub(crate) fn av1c(&self) -> Vec<u8> {
        let mut v = vec![
            0x81, // marker and version
            (self.profile << 5) | (self.level & 31),
            (self.tier << 7)
                | (u8::from(self.bit_depth > 8) << 6)
                | (u8::from(self.bit_depth == 12) << 5)
                | (u8::from(self.mono) << 4)
                | (u8::from(self.subsampling_x) << 3)
                | (u8::from(self.subsampling_y) << 2)
                | self.chroma_position,
            0, // no presentation delay
        ];
        v.extend_from_slice(&self.obu);
        v
    }
}

/// The `nv12-colorspace` of the SDK for a matrix of the AV1 specification.
pub(crate) fn nv12_colorspace(matrix: u8, full_range: bool, height: u32) -> u32 {
    let bt709 = match matrix {
        1 => true,
        2 => height >= 720,
        _ => false,
    };
    (if bt709 { 3 } else { 1 }) + u32::from(full_range)
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let b = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(u32::from(b))
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }

    fn flag(&mut self) -> Option<bool> {
        Some(self.bit()? != 0)
    }

    /// `uvlc()`.
    fn uvlc(&mut self) -> Option<()> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros >= 32 {
                return Some(());
            }
        }
        self.bits(zeros)?;
        Some(())
    }
}

/// Reads a `leb128()` value. Gives the value and the bytes it took.
fn leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for (i, &b) in data.iter().take(8).enumerate() {
        v |= u64::from(b & 0x7f) << (i * 7);
        if b & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

/// The OBU type of the first byte of an OBU header: 1 is a sequence header.
const OBU_SEQUENCE_HEADER: u8 = 1;

/// Walks the OBUs of a temporal unit in the low-overhead format (each OBU has a size field, as in WebM and in the SDK
/// demuxer). Calls `f` with the type and the whole OBU. Stops at the first OBU that cannot be read.
fn for_each_obu(data: &[u8], mut f: impl FnMut(u8, &[u8]) -> bool) {
    let mut at = 0;
    while at < data.len() {
        let first = data[at];
        if first & 2 == 0 {
            return; // no size field: not the low-overhead format
        }
        let header = if first & 4 != 0 { 2 } else { 1 };
        let Some(rest) = data.get(at + header..) else {
            return;
        };
        let Some((size, n)) = leb128(rest) else {
            return;
        };
        let Some(end) = usize::try_from(size)
            .ok()
            .and_then(|s| (at + header + n).checked_add(s))
        else {
            return;
        };
        let Some(obu) = data.get(at..end) else { return };
        if !f((first >> 3) & 15, obu) {
            return;
        }
        at = end;
    }
}

/// The sequence header of a temporal unit. `None` when the unit has none or the header cannot be read.
pub(crate) fn find_sequence(unit: &[u8]) -> Option<Sequence> {
    let mut found = None;
    for_each_obu(unit, |kind, obu| {
        if kind == OBU_SEQUENCE_HEADER {
            found = parse_sequence(obu);
            return false;
        }
        true
    });
    found
}

/// Reads a sequence header OBU (with its header and size field).
fn parse_sequence(obu: &[u8]) -> Option<Sequence> {
    let header = if obu.first()? & 4 != 0 { 2 } else { 1 };
    let (size, n) = leb128(obu.get(header..)?)?;
    let payload = obu.get(header + n..)?.get(..usize::try_from(size).ok()?)?;
    let mut r = Bits {
        data: payload,
        pos: 0,
    };

    let profile = r.bits(3)? as u8;
    r.bit()?; // still_picture
    let reduced = r.flag()?;
    let (mut level, mut tier) = (0u8, 0u8);
    if reduced {
        level = r.bits(5)? as u8;
    } else {
        let mut decoder_model = false;
        let mut delay_len = 0;
        if r.flag()? {
            // timing_info
            r.bits(32)?;
            r.bits(32)?;
            if r.flag()? {
                r.uvlc()?;
            }
            decoder_model = r.flag()?;
            if decoder_model {
                delay_len = r.bits(5)? + 1;
                r.bits(32)?;
                r.bits(5)?;
                r.bits(5)?;
            }
        }
        let display_delay = r.flag()?;
        let points = r.bits(5)? + 1;
        for i in 0..points {
            r.bits(12)?; // operating_point_idc
            let l = r.bits(5)? as u8;
            let t = if l > 7 { r.bit()? as u8 } else { 0 };
            if i == 0 {
                level = l;
                tier = t;
            }
            if decoder_model && r.flag()? {
                r.bits(delay_len)?;
                r.bits(delay_len)?;
                r.bit()?;
            }
            if display_delay && r.flag()? {
                r.bits(4)?;
            }
        }
    }
    let width_bits = r.bits(4)? + 1;
    let height_bits = r.bits(4)? + 1;
    let max_width = r.bits(width_bits)? + 1;
    let max_height = r.bits(height_bits)? + 1;
    if !reduced && r.flag()? {
        r.bits(4)?; // delta_frame_id_length_minus_2
        r.bits(3)?; // additional_frame_id_length_minus_1
    }
    r.bits(3)?; // 128x128 superblocks, filter intra, intra edge filter
    if !reduced {
        r.bits(4)?; // inter-intra, masked compound, warped motion, dual filter
        let order_hint = r.flag()?;
        if order_hint {
            r.bits(2)?; // jnt_comp, ref_frame_mvs
        }
        // seq_choose_screen_content_tools: 1 gives "select" (2), else the next bit is the value.
        let screen_tools = if r.flag()? { 2 } else { r.bit()? };
        if screen_tools > 0 && !r.flag()? {
            r.bit()?; // seq_force_integer_mv
        }
        if order_hint {
            r.bits(3)?; // order_hint_bits_minus_1
        }
    }
    r.bits(3)?; // superres, cdef, restoration

    // color_config()
    let high = r.flag()?;
    let twelve = profile == 2 && high && r.flag()?;
    let bit_depth = if twelve {
        12
    } else if high {
        10
    } else {
        8
    };
    let mono = profile != 1 && r.flag()?;
    let (mut primaries, mut transfer, mut matrix) = (2, 2, 2);
    if r.flag()? {
        primaries = r.bits(8)?;
        transfer = r.bits(8)?;
        matrix = r.bits(8)?;
    }
    let (full_range, ssx, ssy, position);
    if mono {
        full_range = r.flag()?;
        (ssx, ssy, position) = (true, true, 0);
    } else if primaries == 1 && transfer == 13 && matrix == 0 {
        // sRGB
        (full_range, ssx, ssy, position) = (true, false, false, 0);
    } else {
        full_range = r.flag()?;
        let (x, y) = match profile {
            0 => (true, true),
            1 => (false, false),
            _ if bit_depth == 12 => {
                let x = r.flag()?;
                (x, x && r.flag()?)
            }
            _ => (true, false),
        };
        (ssx, ssy) = (x, y);
        position = if ssx && ssy { r.bits(2)? as u8 } else { 0 };
    }
    Some(Sequence {
        profile,
        level,
        tier,
        bit_depth,
        mono,
        subsampling_x: ssx,
        subsampling_y: ssy,
        chroma_position: position,
        matrix: matrix as u8,
        full_range,
        max_width,
        max_height,
        obu: obu.get(..header + n + payload.len())?.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes bits, most significant first.
    struct Writer {
        bytes: Vec<u8>,
        n: usize,
    }

    impl Writer {
        fn put(&mut self, v: u32, bits: u32) {
            for i in (0..bits).rev() {
                if self.n.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                let last = self.bytes.len() - 1;
                self.bytes[last] |= (((v >> i) & 1) as u8) << (7 - self.n % 8);
                self.n += 1;
            }
        }
    }

    /// A sequence header OBU of a main-profile 8-bit 4:2:0 stream of `w` by `h` with BT.709 limited colour, as an
    /// encoder writes it (no timing info, one operating point, every tool on).
    fn sequence_obu(w: u32, h: u32) -> Vec<u8> {
        let mut b = Writer {
            bytes: vec![],
            n: 0,
        };
        b.put(0, 3); // profile
        b.put(0, 1); // still_picture
        b.put(0, 1); // reduced_still_picture_header
        b.put(0, 1); // timing_info_present
        b.put(0, 1); // initial_display_delay_present
        b.put(0, 5); // operating_points_cnt_minus_1
        b.put(0, 12); // operating_point_idc
        b.put(8, 5); // seq_level_idx: level 4.0, so a tier bit follows
        b.put(0, 1); // seq_tier
        b.put(15, 4); // frame_width_bits_minus_1
        b.put(15, 4); // frame_height_bits_minus_1
        b.put(w - 1, 16);
        b.put(h - 1, 16);
        b.put(0, 1); // frame_id_numbers_present
        b.put(0, 3); // superblock, filter intra, intra edge
        b.put(0, 4); // tools
        b.put(1, 1); // enable_order_hint
        b.put(0, 2); // jnt_comp, ref_frame_mvs
        b.put(1, 1); // seq_choose_screen_content_tools
        b.put(1, 1); // seq_choose_integer_mv
        b.put(6, 3); // order_hint_bits_minus_1
        b.put(0, 3); // superres, cdef, restoration
        b.put(0, 1); // high_bitdepth
        b.put(0, 1); // mono_chrome
        b.put(1, 1); // color_description_present
        b.put(1, 8); // BT.709 primaries
        b.put(1, 8); // BT.709 transfer
        b.put(1, 8); // BT.709 matrix
        b.put(0, 1); // color_range
        b.put(0, 2); // chroma_sample_position
        b.put(0, 1); // separate_uv_delta_q
        b.put(0, 1); // film_grain_params_present
        b.put(1, 1); // trailing bit
        let mut obu = vec![(OBU_SEQUENCE_HEADER << 3) | 2, b.bytes.len() as u8];
        obu.extend_from_slice(&b.bytes);
        obu
    }

    #[test]
    fn reads_the_size_and_colour_of_a_sequence_header() {
        let obu = sequence_obu(2560, 1440);
        let s = find_sequence(&obu).unwrap_or_else(|| unreachable!());
        assert_eq!((s.max_width, s.max_height), (2560, 1440));
        assert_eq!((s.profile, s.level, s.tier, s.bit_depth), (0, 8, 0, 8));
        assert!(s.is_nv12());
        assert_eq!(s.matrix, 1);
        assert!(!s.full_range);
        assert_eq!(s.nv12_colorspace(1440), 3);
        assert_eq!(s.obu, obu);
    }

    #[test]
    fn the_sequence_header_is_found_after_another_obu() {
        // A temporal delimiter (type 2, size 0) comes first in a temporal unit.
        let mut unit = vec![(2 << 3) | 2, 0];
        unit.extend_from_slice(&sequence_obu(64, 48));
        unit.extend_from_slice(&[(6 << 3) | 2, 1, 0xAA]);
        let s = find_sequence(&unit).unwrap_or_else(|| unreachable!());
        assert_eq!((s.max_width, s.max_height), (64, 48));
    }

    #[test]
    fn the_av1c_payload_has_four_bytes_and_the_header() {
        let obu = sequence_obu(64, 48);
        let s = find_sequence(&obu).unwrap_or_else(|| unreachable!());
        let c = s.av1c();
        assert_eq!(&c[..4], &[0x81, 8, (1 << 3) | (1 << 2), 0]);
        assert_eq!(&c[4..], &obu[..]);
    }

    #[test]
    fn a_unit_without_a_readable_header_gives_none() {
        assert_eq!(find_sequence(&[]), None);
        assert_eq!(find_sequence(&[(2 << 3) | 2, 0]), None);
        // A size that runs past the data.
        assert_eq!(
            find_sequence(&[(OBU_SEQUENCE_HEADER << 3) | 2, 40, 1, 2]),
            None
        );
        // No size field.
        assert_eq!(find_sequence(&[OBU_SEQUENCE_HEADER << 3, 1, 2, 3]), None);
        // A header that stops in the middle.
        let obu = sequence_obu(64, 48);
        let mut short = obu[..4].to_vec();
        short[1] = 2;
        assert_eq!(find_sequence(&short), None);
    }

    #[test]
    fn an_unspecified_matrix_follows_the_height() {
        assert_eq!(nv12_colorspace(2, false, 1080), 3);
        assert_eq!(nv12_colorspace(2, true, 480), 2);
        assert_eq!(nv12_colorspace(6, false, 1080), 1);
    }
}
