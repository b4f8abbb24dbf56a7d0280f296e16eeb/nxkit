//! The parts of a VP9 stream that the decoder needs before the first frame: the size, the colour and the `vpcC` atom.
//! They come from the uncompressed header of a key frame (VP9 bitstream specification, section 6.2).

/// What a VP9 key frame says about the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyFrame {
    /// 0 to 3.
    pub profile: u8,
    /// 8, 10 or 12.
    pub bit_depth: u8,
    /// `color_space` of the header: 0 unknown, 1 BT.601, 2 BT.709, 3 SMPTE 170M, 4 SMPTE 240M, 5 BT.2020, 7 sRGB.
    pub color_space: u8,
    /// The samples use the full range 0 to 255.
    pub full_range: bool,
    /// Chroma is halved across.
    pub subsampling_x: bool,
    /// Chroma is halved down.
    pub subsampling_y: bool,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl KeyFrame {
    /// Whether the stream is 8-bit 4:2:0, the only layout the NV12 output carries.
    pub(crate) fn is_nv12(&self) -> bool {
        self.bit_depth == 8 && self.subsampling_x && self.subsampling_y
    }

    /// The `nv12-colorspace` of the format the SDK asks for: 1 BT.601 limited, 2 BT.601 full, 3 BT.709 limited,
    /// 4 BT.709 full. A stream that names no colour space is BT.709 from 720 lines up, as `dm/host/hle_movie.c` does.
    pub(crate) fn nv12_colorspace(&self) -> u32 {
        let bt709 = match self.color_space {
            2 => true,
            0 => self.height >= 720,
            _ => false,
        };
        (if bt709 { 3 } else { 1 }) + u32::from(self.full_range)
    }

    /// The payload of the `vpcC` atom (VP Codec ISO Media File Format Binding, section 2.2) that describes this stream.
    /// It starts with the version and flags of the full box and has no box header.
    pub(crate) fn vpcc(&self) -> Vec<u8> {
        let chroma: u8 = match (self.subsampling_x, self.subsampling_y) {
            (true, true) => 1,
            (true, false) => 2,
            _ => 3,
        };
        let (primaries, transfer, matrix) = match self.color_space {
            1 | 3 => (6, 6, 6),
            2 => (1, 1, 1),
            4 => (7, 7, 7),
            5 => (9, 14, 9),
            7 => (1, 13, 0),
            _ => (2, 2, 2),
        };
        vec![
            1, // version
            0,
            0,
            0, // flags
            self.profile,
            0, // level: not given by the header
            (self.bit_depth << 4) | (chroma << 1) | u8::from(self.full_range),
            primaries,
            transfer,
            matrix,
            0, // codecInitializationDataSize, high byte
            0, // low byte
        ]
    }
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
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }
}

/// Reads the header of a packet. `None` when the packet does not start with a key frame (a frame that is not a key
/// frame, a repeat of an earlier frame, a corrupt header or a reserved value).
pub(crate) fn parse_key_frame(data: &[u8]) -> Option<KeyFrame> {
    let mut r = Bits { data, pos: 0 };
    if r.bits(2)? != 2 {
        return None;
    }
    let low = r.bit()?;
    let high = r.bit()?;
    let profile = (high << 1) | low;
    if profile == 3 && r.bit()? != 0 {
        return None;
    }
    if r.bit()? != 0 {
        return None; // show_existing_frame
    }
    if r.bit()? != 0 {
        return None; // frame_type: not a key frame
    }
    r.bits(2)?; // show_frame, error_resilient_mode
    if r.bits(24)? != 0x0049_8342 {
        return None;
    }
    let bit_depth = if profile >= 2 {
        if r.bit()? != 0 { 12 } else { 10 }
    } else {
        8
    };
    let color_space = r.bits(3)? as u8;
    let (full_range, subsampling_x, subsampling_y);
    if color_space != 7 {
        full_range = r.bit()? != 0;
        if profile == 1 || profile == 3 {
            subsampling_x = r.bit()? != 0;
            subsampling_y = r.bit()? != 0;
            if r.bit()? != 0 {
                return None;
            }
        } else {
            subsampling_x = true;
            subsampling_y = true;
        }
    } else {
        full_range = true;
        if profile == 1 || profile == 3 {
            subsampling_x = false;
            subsampling_y = false;
            if r.bit()? != 0 {
                return None;
            }
        } else {
            return None; // sRGB needs profile 1 or 3
        }
    }
    let width = r.bits(16)? + 1;
    let height = r.bits(16)? + 1;
    Some(KeyFrame {
        profile: profile as u8,
        bit_depth,
        color_space,
        full_range,
        subsampling_x,
        subsampling_y,
        width,
        height,
    })
}

#[cfg(test)]
mod testing {
    /// Writes bits, most significant first.
    struct Writer {
        bytes: Vec<u8>,
        n: usize,
    }

    impl Writer {
        fn put(&mut self, v: u32, bits: u32) {
            for i in (0..bits).rev() {
                if self.n % 8 == 0 {
                    self.bytes.push(0);
                }
                let b = ((v >> i) & 1) as u8;
                let last = self.bytes.len() - 1;
                self.bytes[last] |= b << (7 - self.n % 8);
                self.n += 1;
            }
        }
    }

    pub(crate) fn key_frame_bytes(color_space: u32, full: u32, w: u32, h: u32) -> Vec<u8> {
        let mut w_ = Writer {
            bytes: vec![],
            n: 0,
        };
        w_.put(2, 2); // frame_marker
        w_.put(0, 2); // profile 0
        w_.put(0, 1); // show_existing_frame
        w_.put(0, 1); // key frame
        w_.put(1, 1); // show_frame
        w_.put(0, 1); // error_resilient_mode
        w_.put(0x49_8342, 24);
        w_.put(color_space, 3);
        w_.put(full, 1);
        w_.put(w - 1, 16);
        w_.put(h - 1, 16);
        w_.bytes
    }
}

#[cfg(test)]
pub(crate) use testing::key_frame_bytes;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_size_and_colour_of_a_profile_0_key_frame() {
        let k =
            parse_key_frame(&key_frame_bytes(2, 0, 1920, 1080)).unwrap_or_else(|| unreachable!());
        assert_eq!((k.width, k.height), (1920, 1080));
        assert_eq!((k.profile, k.bit_depth), (0, 8));
        assert!(k.is_nv12());
        assert!(!k.full_range);
        assert_eq!(k.nv12_colorspace(), 3);
    }

    #[test]
    fn an_undeclared_colour_space_follows_the_height() {
        let hd =
            parse_key_frame(&key_frame_bytes(0, 0, 1280, 720)).unwrap_or_else(|| unreachable!());
        let sd =
            parse_key_frame(&key_frame_bytes(0, 1, 640, 480)).unwrap_or_else(|| unreachable!());
        assert_eq!(hd.nv12_colorspace(), 3);
        assert_eq!(sd.nv12_colorspace(), 2);
    }

    #[test]
    fn a_frame_that_is_not_a_key_frame_gives_none() {
        let mut p = key_frame_bytes(2, 0, 64, 64);
        p[0] |= 0x04; // frame_type 1
        assert_eq!(parse_key_frame(&p), None);
        assert_eq!(parse_key_frame(&[]), None);
        assert_eq!(parse_key_frame(&[0x00, 0x00, 0x00]), None);
    }

    #[test]
    fn the_vpcc_payload_has_twelve_bytes_and_the_stream_values() {
        let k =
            parse_key_frame(&key_frame_bytes(2, 0, 1920, 1080)).unwrap_or_else(|| unreachable!());
        let v = k.vpcc();
        assert_eq!(v.len(), 12);
        assert_eq!(&v[..4], &[1, 0, 0, 0]);
        assert_eq!(v[4], 0);
        assert_eq!(v[6], (8 << 4) | (1 << 1));
        assert_eq!(&v[7..10], &[1, 1, 1]);
    }
}
