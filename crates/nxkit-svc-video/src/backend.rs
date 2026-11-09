//! The stream decoders that the video service opens: VideoToolbox on macOS, and rav1d for AV1 where the Mac has no AV1
//! decoder in hardware.
//!
//! VideoToolbox decodes VP9 on this Mac after `VTRegisterSupplementalVideoDecoderIfAvailable`. Its pictures stay in
//! `CVPixelBuffer`s that Metal can map (R20, R44). VideoToolbox decodes AV1 in hardware only (Apple M3 and A17 Pro on).
//! On an older chip, for a stream that VideoToolbox refuses, and with `NXKIT_AV1_SOFTWARE=1`, rav1d decodes AV1 on the
//! CPU and the pictures are in memory.

use crate::decoder::Backend;
use crate::vp9::KeyFrame;

#[cfg(target_os = "macos")]
mod apple {
    use std::sync::Arc;

    use crate::av1::{Sequence, find_sequence, nv12_colorspace};
    use crate::decoder::{Backend, Decoded};
    use crate::frame::Frame;
    use crate::sys::dav1d::{Dav1d, Picture};
    use crate::sys::vt::{self, Session};
    use crate::vp9::KeyFrame;

    pub(super) struct VideoToolbox {
        session: Session,
        width: u32,
        height: u32,
        colorspace: u32,
    }

    impl VideoToolbox {
        pub(super) fn open(key: &KeyFrame) -> Result<VideoToolbox, String> {
            Ok(VideoToolbox {
                session: Session::vp9(key)?,
                width: key.width,
                height: key.height,
                colorspace: key.nv12_colorspace(),
            })
        }
    }

    impl Backend for VideoToolbox {
        fn decode(&mut self, packet: &[u8], ts: i64) -> Result<Vec<Decoded>, String> {
            let picture = self.session.decode(packet)?;
            Ok(picture
                .map(|b| Decoded {
                    frame: Arc::new(Frame::from_buffer(
                        self.width,
                        self.height,
                        self.colorspace,
                        b,
                    )),
                    ts,
                })
                .into_iter()
                .collect())
        }
    }

    /// Whether the environment asks for the software decoder.
    fn software_requested() -> bool {
        std::env::var("NXKIT_AV1_SOFTWARE").is_ok_and(|v| !v.is_empty() && v != "0")
    }

    /// The AV1 decoder of one stream. A temporal unit goes in whole, its OBUs in the low-overhead format that WebM and
    /// the demuxer of the SDK keep them in. A unit shows at most one picture. VideoToolbox gives it at once; rav1d decodes
    /// several units at the same time (frame threads) and gives the picture of a unit with a later one, in display order
    /// and with the timestamp of its own unit.
    ///
    /// The hardware decoder of VideoToolbox is used when the Mac has one and the stream is 8-bit 4:2:0. Anything else
    /// goes to rav1d, from the unit that VideoToolbox refused on.
    pub(super) struct Av1 {
        /// The sequence header in use. A changed one opens a new session.
        seq: Option<Sequence>,
        hardware: Option<Session>,
        software: Option<Dav1d>,
        /// rav1d decodes from now on.
        soft: bool,
        logged: bool,
    }

    impl Av1 {
        /// A decoder that uses rav1d, whatever the Mac has.
        #[cfg(test)]
        pub(super) fn software() -> Av1 {
            Av1 {
                seq: None,
                hardware: None,
                software: None,
                soft: true,
                logged: false,
            }
        }

        pub(super) fn new() -> Av1 {
            Av1 {
                seq: None,
                hardware: None,
                software: None,
                soft: software_requested() || !vt::av1_hardware(),
                logged: false,
            }
        }

        /// A sequence header that differs from the one in use: opens the decoder for it.
        fn start(&mut self, seq: Sequence) {
            self.hardware = None;
            if !self.soft {
                match Session::av1(&seq) {
                    Ok(s) => {
                        self.hardware = Some(s);
                        eprintln!(
                            "[codec] AV1: VideoToolbox decodes {}x{}, profile {} level {}",
                            seq.max_width, seq.max_height, seq.profile, seq.level
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "[codec] AV1: VideoToolbox does not take the stream ({e}), rav1d decodes it on the CPU"
                        );
                        self.soft = true;
                    }
                }
            }
            if self.soft && !self.logged {
                self.logged = true;
                let why = if software_requested() {
                    "NXKIT_AV1_SOFTWARE"
                } else {
                    "no AV1 decoder in hardware"
                };
                eprintln!(
                    "[codec] AV1: rav1d decodes {}x{} on the CPU ({why})",
                    seq.max_width, seq.max_height
                );
            }
            self.seq = Some(seq);
        }

        fn decode_software(&mut self, packet: &[u8], ts: i64) -> Result<Vec<Decoded>, String> {
            if self.software.is_none() {
                self.software = Some(Dav1d::open()?);
            }
            let Some(dav1d) = self.software.as_mut() else {
                return Ok(Vec::new());
            };
            dav1d.decode(packet, ts)?.into_iter().map(convert).collect()
        }
    }

    /// A picture of rav1d as a frame in NV12.
    fn convert(pic: Picture) -> Result<Decoded, String> {
        let (matrix, full_range) = pic.matrix_and_range();
        let nv12 = pic.with_planar(|p| (p.width, p.height, p.to_nv12()));
        let Some((w, h, nv12)) = nv12 else {
            return Err("rav1d gave a picture in a layout that has no NV12 form".to_string());
        };
        let (w, h) = (w as u32, h as u32);
        Ok(Decoded {
            frame: Arc::new(Frame::from_nv12(
                w,
                h,
                nv12_colorspace(matrix, full_range, h),
                nv12,
            )),
            ts: pic.timestamp(),
        })
    }

    impl Backend for Av1 {
        fn decode(&mut self, packet: &[u8], ts: i64) -> Result<Vec<Decoded>, String> {
            if let Some(seq) = find_sequence(packet).filter(|s| self.seq.as_ref() != Some(s)) {
                // A changed sequence header: the pictures of the old stream come out first.
                let rest = match self.software.as_mut() {
                    Some(d) if self.seq.is_some() => d.drain()?.into_iter().map(convert).collect(),
                    _ => Ok(Vec::new()),
                };
                self.start(seq);
                let mut out: Vec<Decoded> = rest?;
                out.extend(self.decode_unit(packet, ts)?);
                return Ok(out);
            }
            self.decode_unit(packet, ts)
        }

        fn drain(&mut self) -> Result<Vec<Decoded>, String> {
            match self.software.as_mut() {
                Some(d) => d.drain()?.into_iter().map(convert).collect(),
                None => Ok(Vec::new()),
            }
        }

        fn flush(&mut self) {
            if let Some(d) = self.software.as_mut() {
                d.flush();
            }
            self.hardware = None;
            self.seq = None;
        }

        fn in_flight(&self) -> usize {
            self.software.as_ref().map_or(0, Dav1d::in_flight)
        }
    }

    impl Av1 {
        fn decode_unit(&mut self, packet: &[u8], ts: i64) -> Result<Vec<Decoded>, String> {
            if self.soft {
                // A stream cannot start in the middle: until the first sequence header, rav1d is not asked.
                if self.seq.is_none() {
                    return Ok(Vec::new());
                }
                return self.decode_software(packet, ts);
            }
            let (Some(session), Some(seq)) = (self.hardware.as_mut(), self.seq.as_ref()) else {
                return Ok(Vec::new());
            };
            let Some(buffer) = session.decode(packet)? else {
                return Ok(Vec::new());
            };
            let (w, h) = buffer.size();
            Ok(vec![Decoded {
                frame: Arc::new(Frame::from_buffer(w, h, seq.nv12_colorspace(h), buffer)),
                ts,
            }])
        }
    }
}

/// Opens a VP9 decoder for a stream that starts with `key`.
#[cfg(target_os = "macos")]
pub(crate) fn open(key: &KeyFrame) -> Result<Box<dyn Backend>, String> {
    Ok(Box::new(apple::VideoToolbox::open(key)?))
}

/// Opens an AV1 decoder. It waits for the sequence header of the stream, which tells it what to open.
#[cfg(target_os = "macos")]
pub(crate) fn open_av1() -> Result<Box<dyn Backend>, String> {
    Ok(Box::new(apple::Av1::new()))
}

/// Opens a VP9 decoder for a stream that starts with `key`.
#[cfg(not(target_os = "macos"))]
pub(crate) fn open(_key: &KeyFrame) -> Result<Box<dyn Backend>, String> {
    Err("VP9 decoding uses VideoToolbox and is only available on macOS".to_string())
}

/// Opens an AV1 decoder.
#[cfg(not(target_os = "macos"))]
pub(crate) fn open_av1() -> Result<Box<dyn Backend>, String> {
    Err("AV1 decoding is only available on macOS".to_string())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::apple::Av1;
    use crate::decoder::{Backend, Decoded};
    use crate::frame::Frame;

    /// Two AV1 streams of four frames at 64x48 that show one flat colour, written with `aomenc` from a synthetic picture
    /// (`color` source of ffmpeg with fixed Y, Cb and Cr). They hold no third-party data. The first is 8-bit with Y 100,
    /// Cb 150 and Cr 90. The second is 10-bit with Y 400, Cb 600 and Cr 360. Both are BT.709 limited range. Each is an IVF
    /// file: a 32 byte header, then frames of a 12 byte header (size, timestamp) and the temporal unit.
    const FLAT_8_BIT: [u8; 200] = [
        0x44, 0x4b, 0x49, 0x46, 0x00, 0x00, 0x20, 0x00, 0x41, 0x56, 0x30, 0x31, 0x40, 0x00, 0x30,
        0x00, 0x0a, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x26, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12,
        0x00, 0x0a, 0x0d, 0x00, 0x00, 0x00, 0x02, 0xaf, 0xf7, 0x9b, 0x5f, 0x32, 0x02, 0x02, 0x02,
        0x08, 0x32, 0x13, 0x10, 0x00, 0xa8, 0x00, 0x00, 0x02, 0x40, 0x00, 0x00, 0x03, 0xce, 0x22,
        0x12, 0x63, 0xea, 0x87, 0x1c, 0x3e, 0xb4, 0x16, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x12, 0x00, 0x32, 0x12, 0x30, 0x03, 0xc0, 0x80, 0x00, 0x00, 0x46,
        0xa8, 0x00, 0x00, 0x02, 0x40, 0x00, 0x00, 0x80, 0x00, 0x99, 0x10, 0x26, 0x00, 0x00, 0x00,
        0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x00, 0x0a, 0x0d, 0x00, 0x00, 0x00,
        0x02, 0xaf, 0xf7, 0x9b, 0x5f, 0x32, 0x02, 0x02, 0x02, 0x08, 0x32, 0x13, 0x10, 0x00, 0xa8,
        0x00, 0x00, 0x02, 0x40, 0x00, 0x00, 0x03, 0xce, 0x22, 0x12, 0x63, 0xea, 0x87, 0x1c, 0x3e,
        0xb4, 0x16, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x00,
        0x32, 0x12, 0x30, 0x03, 0xc0, 0x80, 0x00, 0x00, 0x46, 0xa8, 0x00, 0x00, 0x02, 0x40, 0x00,
        0x00, 0x80, 0x00, 0x99, 0x10,
    ];
    const FLAT_10_BIT: [u8; 202] = [
        0x44, 0x4b, 0x49, 0x46, 0x00, 0x00, 0x20, 0x00, 0x41, 0x56, 0x30, 0x31, 0x40, 0x00, 0x30,
        0x00, 0x0a, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x27, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12,
        0x00, 0x0a, 0x0d, 0x00, 0x00, 0x00, 0x02, 0xaf, 0xf7, 0x9b, 0x5f, 0x3a, 0x02, 0x02, 0x02,
        0x08, 0x32, 0x14, 0x10, 0x00, 0x9c, 0x80, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0xeb, 0xca,
        0x48, 0x05, 0x60, 0x7d, 0x8d, 0x89, 0xc7, 0xb0, 0x16, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x00, 0x32, 0x12, 0x30, 0x03, 0xc0, 0x80, 0x00, 0x00,
        0x46, 0xa8, 0x00, 0x00, 0x02, 0x40, 0x00, 0x00, 0x80, 0x00, 0x99, 0x10, 0x27, 0x00, 0x00,
        0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x00, 0x0a, 0x0d, 0x00, 0x00,
        0x00, 0x02, 0xaf, 0xf7, 0x9b, 0x5f, 0x3a, 0x02, 0x02, 0x02, 0x08, 0x32, 0x14, 0x10, 0x00,
        0x9c, 0x80, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0xeb, 0xca, 0x48, 0x05, 0x60, 0x7d, 0x8d,
        0x89, 0xc7, 0xb0, 0x16, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x12, 0x00, 0x32, 0x12, 0x30, 0x03, 0xc0, 0x80, 0x00, 0x00, 0x46, 0xa8, 0x00, 0x00, 0x02,
        0x40, 0x00, 0x00, 0x80, 0x00, 0x99, 0x10,
    ];

    /// The temporal units of an IVF file.
    fn units(ivf: &[u8]) -> Vec<&[u8]> {
        let mut out = Vec::new();
        let mut at = 32;
        while let Some(head) = ivf.get(at..at + 12) {
            let size = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
            let Some(unit) = ivf.get(at + 12..at + 12 + size) else {
                break;
            };
            out.push(unit);
            at += 12 + size;
        }
        out
    }

    fn software() -> Av1 {
        Av1::software()
    }

    /// The timestamp of unit `i` in these tests.
    fn ts(i: usize) -> i64 {
        1_000 + 16_667 * i as i64
    }

    /// Sends the units, each with its timestamp, and gives the pictures that came out while sending, then the ones that
    /// the end of the stream gave.
    fn feed(d: &mut Av1, units: &[&[u8]]) -> (Vec<Decoded>, Vec<Decoded>) {
        let mut during = Vec::new();
        for (i, unit) in units.iter().enumerate() {
            during.extend(
                d.decode(unit, ts(i))
                    .unwrap_or_else(|e| unreachable!("decode failed: {e}")),
            );
        }
        let after = d
            .drain()
            .unwrap_or_else(|e| unreachable!("drain failed: {e}"));
        (during, after)
    }

    /// The Y, Cb and Cr of the first pixel.
    fn first_pixel(f: &Frame) -> (u8, u8, u8) {
        let n = f.to_nv12().unwrap_or_else(|_| unreachable!());
        (n.y[0], n.uv[0], n.uv[1])
    }

    fn near(a: u8, b: u8) -> bool {
        a.abs_diff(b) <= 3
    }

    fn frame_threads() -> bool {
        std::thread::available_parallelism().is_ok_and(|n| n.get() > 1)
    }

    #[test]
    fn the_stream_has_four_units() {
        assert_eq!(units(&FLAT_8_BIT).len(), 4);
        assert_eq!(units(&FLAT_10_BIT).len(), 4);
    }

    #[test]
    fn rav1d_decodes_a_flat_8_bit_stream() {
        let mut d = software();
        let (during, after) = feed(&mut d, &units(&FLAT_8_BIT));
        assert_eq!(during.len() + after.len(), 4);
        for p in during.iter().chain(&after) {
            let f = &p.frame;
            assert_eq!((f.width(), f.height()), (64, 48));
            // BT.709 limited range.
            assert_eq!(f.colorspace(), 3);
            let (y, u, v) = first_pixel(f);
            assert!(
                near(y, 100) && near(u, 150) && near(v, 90),
                "pixel {y} {u} {v}"
            );
            // The last pixel of the last row too.
            let n = f.to_nv12().unwrap_or_else(|_| unreachable!());
            assert!(near(n.y[64 * 48 - 1], 100));
        }
    }

    #[test]
    fn rav1d_gives_a_10_bit_stream_as_8_bit_nv12() {
        let mut d = software();
        let (during, after) = feed(&mut d, &units(&FLAT_10_BIT));
        assert_eq!(during.len() + after.len(), 4);
        for p in during.iter().chain(&after) {
            let (y, u, v) = first_pixel(&p.frame);
            assert!(
                near(y, 100) && near(u, 150) && near(v, 90),
                "pixel {y} {u} {v}"
            );
        }
    }

    #[test]
    fn pictures_come_in_order_with_their_own_timestamps_and_later_than_their_packets() {
        let mut d = software();
        let (during, after) = feed(&mut d, &units(&FLAT_8_BIT));
        let all: Vec<i64> = during.iter().chain(&after).map(|p| p.ts).collect();
        assert_eq!(all, (0..4).map(ts).collect::<Vec<_>>());
        if frame_threads() {
            // The picture of a unit comes out with a later unit, so some are still inside when the data ends.
            assert!(during.len() < 4, "{} pictures before the end", during.len());
            assert!(!after.is_empty());
            // The first picture to come out is not the one of the unit that was just sent.
            if let Some(first) = during.first() {
                assert_eq!(first.ts, ts(0));
            }
        }
    }

    #[test]
    fn the_end_of_the_stream_gives_the_rest_and_then_nothing() {
        let mut d = software();
        let all = units(&FLAT_8_BIT);
        let mut got = 0;
        for (i, unit) in all.iter().enumerate() {
            got += d.decode(unit, ts(i)).map_or(0, |v| v.len());
            assert!(d.in_flight() <= 8);
        }
        let rest = d.drain().unwrap_or_else(|_| unreachable!());
        assert_eq!(got + rest.len(), 4);
        assert_eq!(d.in_flight(), 0);
        assert!(d.drain().is_ok_and(|v| v.is_empty()));
    }

    #[test]
    fn a_flush_drops_the_pictures_inside() {
        let mut d = software();
        let all = units(&FLAT_8_BIT);
        let mut got = 0;
        for (i, unit) in all.iter().enumerate() {
            got += d.decode(unit, ts(i)).map_or(0, |v| v.len());
        }
        d.flush();
        assert_eq!(d.in_flight(), 0);
        assert!(d.drain().is_ok_and(|v| v.is_empty()));
        if frame_threads() {
            assert!(got < 4);
        }
        // The stream starts over with a key frame.
        let (during, after) = feed(&mut d, &all);
        assert_eq!(during.len() + after.len(), 4);
        assert_eq!(
            during.iter().chain(&after).next().map(|p| p.ts),
            Some(ts(0))
        );
    }

    #[test]
    fn a_changed_sequence_header_is_followed() {
        let mut d = software();
        let first = units(&FLAT_8_BIT);
        let second = units(&FLAT_10_BIT);
        let mut n = d.decode(first[0], 0).map_or(0, |v| v.len());
        n += d.decode(second[0], 1).map_or(0, |v| v.len());
        n += d.decode(second[1], 2).map_or(0, |v| v.len());
        n += d.drain().map_or(0, |v| v.len());
        assert_eq!(n, 3);
    }

    #[test]
    fn units_before_the_first_sequence_header_show_nothing() {
        let mut d = software();
        let all = units(&FLAT_8_BIT);
        // The second unit is an inter frame without a sequence header.
        assert!(d.decode(all[1], 0).is_ok_and(|v| v.is_empty()));
        assert!(d.decode(&[], 0).is_ok_and(|v| v.is_empty()));
    }

    #[test]
    fn a_corrupt_unit_is_an_error_or_nothing_and_the_stream_goes_on() {
        let mut d = software();
        let all = units(&FLAT_8_BIT);
        assert!(d.decode(all[0], 0).is_ok());
        // Garbage with a plausible OBU header, a truncated unit and a unit with flipped bytes.
        let mut cut = all[1].to_vec();
        cut.truncate(cut.len() / 2);
        let mut flipped = all[2].to_vec();
        let n = flipped.len();
        for b in &mut flipped[n / 2..] {
            *b ^= 0xA5;
        }
        for bad in [
            &[0x32u8, 0x03, 0xff, 0xff, 0xff][..],
            &cut[..],
            &flipped[..],
        ] {
            let _ = d.decode(bad, 1);
        }
        // A key frame still decodes after them.
        d.flush();
        let (during, after) = feed(&mut d, &all[..1]);
        assert_eq!(during.len() + after.len(), 1);
    }
}
