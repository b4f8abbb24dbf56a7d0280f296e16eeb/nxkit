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
    use crate::decoder::{Backend, Pic, Pipeline};
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
        fn decode(&mut self, packet: &[u8]) -> Result<Option<Arc<Frame>>, String> {
            let picture = self.session.decode(packet)?;
            Ok(picture.map(|b| {
                Arc::new(Frame::from_buffer(
                    self.width,
                    self.height,
                    self.colorspace,
                    b,
                ))
            }))
        }
    }

    /// Whether the environment asks for the software decoder.
    fn software_requested() -> bool {
        std::env::var("NXKIT_AV1_SOFTWARE").is_ok_and(|v| !v.is_empty() && v != "0")
    }

    /// The AV1 decoder of one stream. A temporal unit goes in whole, its OBUs in the low-overhead format that WebM and
    /// the demuxer of the SDK keep them in. A unit shows at most one picture, in order, so no queue is needed.
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

        fn decode_software(&mut self, packet: &[u8]) -> Result<Option<Arc<Frame>>, String> {
            if self.software.is_none() {
                self.software = Some(Dav1d::open()?);
            }
            let Some(dav1d) = self.software.as_mut() else {
                return Ok(None);
            };
            let Some(pic) = dav1d.decode(packet)? else {
                return Ok(None);
            };
            to_frame(&pic).map(Some)
        }

        /// Follows the sequence header of a packet: a changed one opens the decoder for it.
        fn follow(&mut self, packet: &[u8]) {
            if let Some(seq) = find_sequence(packet).filter(|s| self.seq.as_ref() != Some(s)) {
                self.start(seq);
            }
        }
    }

    /// A rav1d picture as a frame of NV12.
    fn to_frame(pic: &Picture) -> Result<Arc<Frame>, String> {
        let (matrix, full_range) = pic.matrix_and_range();
        let nv12 = pic.with_planar(|p| (p.width, p.height, p.to_nv12()));
        let Some((w, h, nv12)) = nv12 else {
            return Err("rav1d gave a picture in a layout that has no NV12 form".to_string());
        };
        let (w, h) = (w as u32, h as u32);
        Ok(Arc::new(Frame::from_nv12(
            w,
            h,
            nv12_colorspace(matrix, full_range, h),
            nv12,
        )))
    }

    /// rav1d on a worker thread, with frame threading. It opens at the first packet that the worker sends, on the
    /// worker. A stream starts at a sequence header: packets before one are dropped.
    struct Dav1dPipeline {
        dav1d: Option<Dav1d>,
        started: bool,
        lag: usize,
    }

    impl Dav1dPipeline {
        fn collect(pics: Vec<Picture>, out: &mut Vec<Pic>) -> Result<(), String> {
            for pic in pics {
                out.push((to_frame(&pic)?, pic.timestamp()));
            }
            Ok(())
        }
    }

    impl Pipeline for Dav1dPipeline {
        fn lag(&self) -> usize {
            self.lag
        }

        fn push(&mut self, packet: &[u8], ts: i64, out: &mut Vec<Pic>) -> Result<bool, String> {
            if packet.is_empty() {
                return Ok(false);
            }
            if !self.started {
                if find_sequence(packet).is_none() {
                    return Ok(false);
                }
                self.started = true;
            }
            if self.dav1d.is_none() {
                self.dav1d = Some(Dav1d::open_streaming()?);
            }
            let Some(d) = self.dav1d.as_mut() else {
                return Ok(false);
            };
            let mut pics = Vec::new();
            let sent = d.send(packet, ts, &mut pics);
            let first = out.len();
            Dav1dPipeline::collect(pics, out)?;
            // One frame at a time: a picture that comes out of this send is the one this packet shows, so it carries
            // the packet's time, as on the console. rav1d's own timestamp is the one of the packet that decoded the
            // frame, and a `show_existing_frame` unit (Ocarina of Time's 3-byte units) shows a frame decoded earlier:
            // its picture came out with an old time, and the movie player waited on it.
            for pic in out.iter_mut().skip(first) {
                pic.1 = ts;
            }
            sent.map(|()| true)
        }

        fn wait(&mut self, out: &mut Vec<Pic>) -> Result<(), String> {
            let Some(d) = self.dav1d.as_mut() else {
                return Ok(());
            };
            let pic = d.wait()?;
            Dav1dPipeline::collect(pic.into_iter().collect(), out)
        }

        fn flush(&mut self) {
            if let Some(d) = self.dav1d.as_mut() {
                d.flush();
            }
            // The next packet must carry a sequence header again.
            self.started = false;
        }
    }

    impl Backend for Av1 {
        fn pipeline(&mut self, packet: &[u8]) -> Option<Box<dyn Pipeline>> {
            self.follow(packet);
            // Hardware decoding, or no sequence header yet: the calling thread decodes.
            if !self.soft || self.seq.is_none() {
                return None;
            }
            self.software = None;
            Some(Box::new(Dav1dPipeline {
                dav1d: None,
                started: false,
                lag: Dav1d::streaming_delay(),
            }))
        }

        fn decode(&mut self, packet: &[u8]) -> Result<Option<Arc<Frame>>, String> {
            self.follow(packet);
            if self.soft {
                // A stream cannot start in the middle: until the first sequence header, rav1d is not asked.
                if self.seq.is_none() {
                    return Ok(None);
                }
                return self.decode_software(packet);
            }
            let (Some(session), Some(seq)) = (self.hardware.as_mut(), self.seq.as_ref()) else {
                return Ok(None);
            };
            let Some(buffer) = session.decode(packet)? else {
                return Ok(None);
            };
            let (w, h) = buffer.size();
            Ok(Some(Arc::new(Frame::from_buffer(
                w,
                h,
                seq.nv12_colorspace(h),
                buffer,
            ))))
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
    use crate::decoder::Backend;
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

    /// The Y, Cb and Cr of the first pixel.
    fn first_pixel(f: &Frame) -> (u8, u8, u8) {
        let n = f.to_nv12().unwrap_or_else(|_| unreachable!());
        (n.y[0], n.uv[0], n.uv[1])
    }

    fn near(a: u8, b: u8) -> bool {
        a.abs_diff(b) <= 3
    }

    #[test]
    fn the_stream_has_four_units() {
        assert_eq!(units(&FLAT_8_BIT).len(), 4);
        assert_eq!(units(&FLAT_10_BIT).len(), 4);
    }

    #[test]
    fn rav1d_decodes_a_flat_8_bit_stream() {
        let mut d = software();
        let mut shown = 0;
        for unit in units(&FLAT_8_BIT) {
            let Some(f) = d
                .decode(unit)
                .unwrap_or_else(|e| panic!("decode failed: {e}"))
            else {
                continue;
            };
            shown += 1;
            assert_eq!((f.width(), f.height()), (64, 48));
            // BT.709 limited range.
            assert_eq!(f.colorspace(), 3);
            let (y, u, v) = first_pixel(&f);
            assert!(
                near(y, 100) && near(u, 150) && near(v, 90),
                "pixel {y} {u} {v}"
            );
            // The last pixel of the last row too.
            let n = f.to_nv12().unwrap_or_else(|_| unreachable!());
            assert!(near(n.y[64 * 48 - 1], 100));
        }
        assert_eq!(shown, 4);
    }

    #[test]
    fn rav1d_gives_a_10_bit_stream_as_8_bit_nv12() {
        let mut d = software();
        let mut shown = 0;
        for unit in units(&FLAT_10_BIT) {
            let Some(f) = d
                .decode(unit)
                .unwrap_or_else(|e| panic!("decode failed: {e}"))
            else {
                continue;
            };
            shown += 1;
            let (y, u, v) = first_pixel(&f);
            assert!(
                near(y, 100) && near(u, 150) && near(v, 90),
                "pixel {y} {u} {v}"
            );
        }
        assert_eq!(shown, 4);
    }

    #[test]
    fn a_changed_sequence_header_is_followed() {
        let mut d = software();
        let first = units(&FLAT_8_BIT);
        let second = units(&FLAT_10_BIT);
        assert!(d.decode(first[0]).is_ok_and(|f| f.is_some()));
        assert!(d.decode(second[0]).is_ok_and(|f| f.is_some()));
        assert!(d.decode(second[1]).is_ok_and(|f| f.is_some()));
    }

    #[test]
    fn units_before_the_first_sequence_header_show_nothing() {
        let mut d = software();
        let all = units(&FLAT_8_BIT);
        // The second unit is an inter frame without a sequence header.
        assert!(matches!(d.decode(all[1]), Ok(None)));
        assert!(matches!(d.decode(&[]), Ok(None)));
    }

    #[test]
    fn the_pipeline_of_rav1d_gives_every_picture_with_its_timestamp() {
        let mut d = software();
        let all = units(&FLAT_8_BIT);
        // No sequence header yet: the calling thread decodes.
        assert!(d.pipeline(all[1]).is_none());
        let Some(mut pipe) = d.pipeline(all[0]) else {
            panic!("no pipeline for a software stream");
        };
        let mut out = Vec::new();
        for (k, unit) in all.iter().enumerate() {
            pipe.push(unit, 700 + k as i64, &mut out)
                .unwrap_or_else(|e| panic!("push failed: {e}"));
        }
        pipe.finish(&mut out)
            .unwrap_or_else(|e| panic!("finish failed: {e}"));
        let ts: Vec<i64> = out.iter().map(|p| p.1).collect();
        assert_eq!(ts, vec![700, 701, 702, 703]);
        assert!(out.iter().all(|p| near(first_pixel(&p.0).0, 100)));
        // A flush drops the state: the stream starts again at a sequence header.
        pipe.flush();
        let mut again = Vec::new();
        pipe.push(all[1], 1, &mut again)
            .unwrap_or_else(|e| panic!("{e}"));
        pipe.finish(&mut again).unwrap_or_else(|e| panic!("{e}"));
        assert!(again.is_empty());
        pipe.push(all[0], 9, &mut again)
            .unwrap_or_else(|e| panic!("{e}"));
        pipe.finish(&mut again).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(again.len(), 1);
    }

    #[test]
    fn a_looping_stream_gives_every_picture_back() {
        // The title movie loops: key frame, three inter frames, again. Every picture must come out, with the waits that
        // the worker makes whenever it is idle.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut d = software();
            let all = units(&FLAT_8_BIT);
            let Some(mut pipe) = d.pipeline(all[0]) else {
                let _ = tx.send(Err("no pipeline".to_string()));
                return;
            };
            let mut out = Vec::new();
            let mut ts = 0;
            for round in 0..300 {
                for unit in &all {
                    if let Err(e) = pipe.push(unit, ts, &mut out) {
                        let _ = tx.send(Err(e));
                        return;
                    }
                    ts += 1;
                }
                if round % 3 == 2 {
                    // Idle: wait until nothing comes any more.
                    loop {
                        let n = out.len();
                        if let Err(e) = pipe.wait(&mut out) {
                            let _ = tx.send(Err(e));
                            return;
                        }
                        if out.len() == n {
                            break;
                        }
                    }
                }
            }
            let _ = pipe.finish(&mut out);
            let order_ok = out.windows(2).all(|w| w[0].1 < w[1].1);
            let _ = tx.send(Ok((out.len(), order_ok)));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(60)) {
            Ok(r) => assert_eq!(r, Ok((1200, true))),
            Err(_) => panic!("the decoder hung"),
        }
    }

    #[test]
    fn a_corrupt_unit_is_an_error_or_nothing_and_the_stream_goes_on() {
        let mut d = software();
        let all = units(&FLAT_8_BIT);
        assert!(d.decode(all[0]).is_ok_and(|f| f.is_some()));
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
            let _ = d.decode(bad);
        }
        // A key frame still decodes after them.
        assert!(d.decode(all[0]).is_ok_and(|f| f.is_some()));
    }
}
