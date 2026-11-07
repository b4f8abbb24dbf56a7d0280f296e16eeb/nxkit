//! The DeviceSink command of the SDK's command list, as stereo 16-bit samples.
//!
//! The SDK renders in the guest. After it processed a frame, the host reads the DeviceSink command from the command list,
//! mixes the sink's input buffers down to stereo and resamples 32 kHz to 48 kHz. The command layouts of the SDK versions
//! differ, so the layout is worked out once from the first full command list. This follows `capture` of nxnative.

use crate::sys::mem;
use std::f64::consts::PI;
use std::sync::OnceLock;

/// The most samples of one frame at 48 kHz.
pub(crate) const FRAME_MAX: usize = 1024;
const RS_T: usize = 16;

fn byte(q: &[u8], o: usize) -> u8 {
    q.get(o).copied().unwrap_or(0)
}

fn le_i16(q: &[u8], o: usize) -> i32 {
    q.get(o..o + 2).map_or(0, |b| i32::from(i16::from_le_bytes([b[0], b[1]])))
}

fn le_u32(q: &[u8], o: usize) -> u32 {
    q.get(o..o + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn le_u64(q: &[u8], o: usize) -> u64 {
    q.get(o..o + 8).map_or(0, |b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

/// The command header of one SDK generation.
#[derive(Clone, Copy, Default)]
struct Hdr {
    /// SDK 4 to 6: size at +4, id at +6, enabled at +0x10.
    six: bool,
    /// SDK 21: no magic, enabled at +0, id at +1, size at +2.
    v21: bool,
}

impl Hdr {
    fn size(self, q: &[u8]) -> i32 {
        le_i16(q, if self.v21 { 2 } else if self.six { 4 } else { 6 })
    }

    fn id(self, q: &[u8]) -> i32 {
        if self.v21 {
            i32::from(byte(q, 1))
        } else if self.six {
            le_i16(q, 6)
        } else {
            i32::from(byte(q, 5))
        }
    }

    fn on(self, q: &[u8]) -> bool {
        if self.v21 {
            byte(q, 0) & 1 != 0
        } else if self.six {
            byte(q, 0x10) != 0
        } else {
            byte(q, 4) != 0
        }
    }

    fn header(self) -> usize {
        if self.v21 {
            0x10
        } else if self.six {
            0x11
        } else {
            8
        }
    }

    /// Whether `q`, which runs to the end of the list, starts with a whole command.
    fn ok(self, q: &[u8]) -> bool {
        let h = self.header();
        if q.len() < h {
            return false;
        }
        if !self.v21 && le_u32(q, 0) != 0xcafe_babe {
            return false;
        }
        let n = self.size(q);
        n >= h as i32 && n as usize <= q.len()
    }

    /// The offset of a DeviceSink field, which sits 8 bytes earlier in SDK 21.
    fn sink_off(self, o: usize) -> usize {
        if self.v21 { o - 8 } else { o }
    }

    fn walks(self, cl: &[u8], ncmd: u32) -> bool {
        let mut pos = 0x20usize;
        for _ in 0..ncmd {
            let Some(q) = cl.get(pos..) else { return false };
            if !self.ok(q) {
                return false;
            }
            pos += self.size(q) as usize;
        }
        true
    }
}

/// What the host learned about the command layout. One value serves all renderers.
pub(crate) struct Layout {
    sink_id: i32,
    hdr: Hdr,
    sink4: bool,
    sink5: bool,
    done: bool,
    skipped_logged: bool,
    malformed_logged: bool,
    hist_logged: bool,
    rate_logged: bool,
}

impl Layout {
    /// The layout before any list was seen.
    pub(crate) const fn new() -> Layout {
        Layout {
            sink_id: 0x13,
            hdr: Hdr { six: false, v21: false },
            sink4: false,
            sink5: false,
            done: false,
            skipped_logged: false,
            malformed_logged: false,
            hist_logged: false,
            rate_logged: false,
        }
    }

    /// Sets the DeviceSink command id that the SDK version has, before the layout is known.
    pub(crate) fn set_sink_id(&mut self, id: i32) {
        self.sink_id = id;
    }
}

/// The state of one renderer's capture: the upsampler history and counters.
pub(crate) struct Cap {
    buf: Vec<i32>,
    pos: usize,
    st: Vec<i32>,
    ch: Vec<i32>,
    frames: u64,
    nonsilent: u64,
    sink_logged: bool,
}

impl Cap {
    /// A capture state with a silent history.
    pub(crate) fn new() -> Cap {
        Cap {
            buf: vec![0; 2 * (RS_T - 1 + FRAME_MAX)],
            pos: 0,
            st: vec![0; 2 * FRAME_MAX],
            ch: vec![0; 6 * FRAME_MAX],
            frames: 0,
            nonsilent: 0,
            sink_logged: false,
        }
    }
}

struct Head {
    mix: u64,
    samples: usize,
    rate: u32,
}

fn taps() -> &'static [[f32; RS_T]; 3] {
    static T: OnceLock<[[f32; RS_T]; 3]> = OnceLock::new();
    T.get_or_init(|| {
        let mut h = [[0f32; RS_T]; 3];
        for (p, row) in h.iter_mut().enumerate() {
            let mut w = [0f64; RS_T];
            let mut sum = 0f64;
            for (k, wk) in w.iter_mut().enumerate() {
                let x = p as f64 / 3.0 + (RS_T / 2 - 1) as f64 - k as f64;
                let fc = 0.9;
                let s = if x.abs() < 1e-9 { fc } else { (PI * fc * x).sin() / (PI * x) };
                let t = (x + (RS_T / 2) as f64) / RS_T as f64;
                let win = 0.42 - 0.5 * (2.0 * PI * t).cos() + 0.08 * (4.0 * PI * t).cos();
                *wk = s * win;
                sum += *wk;
            }
            for (r, wk) in row.iter_mut().zip(w.iter()) {
                *r = (*wk / sum) as f32;
            }
        }
        h
    })
}

/// Resamples the `n` stereo samples in `cap.st` from 32 kHz to 48 kHz in place. Returns the output count.
fn up_3_2(cap: &mut Cap, n: usize) -> usize {
    let h = taps();
    let off = 2 * (RS_T - 1);
    cap.buf[off..off + 2 * n].copy_from_slice(&cap.st[..2 * n]);
    let mut m = 0usize;
    let mut pos = cap.pos;
    while pos < 3 * n {
        let base = 2 * (pos / 3);
        let c = &h[pos % 3];
        let mut l = 0f32;
        let mut r = 0f32;
        for (k, ck) in c.iter().enumerate() {
            l += ck * cap.buf[base + 2 * k] as f32;
            r += ck * cap.buf[base + 2 * k + 1] as f32;
        }
        cap.st[2 * m] = l.round_ties_even() as i32;
        cap.st[2 * m + 1] = r.round_ties_even() as i32;
        pos += 2;
        m += 1;
    }
    cap.pos = pos - 3 * n;
    cap.buf.copy_within(2 * n..2 * n + off, 0);
    m
}

fn read_i32s(addr: u64, out: &mut [i32]) {
    let len = (out.len() * 4) as u64;
    mem::with_host(addr, len, |b| {
        for (o, c) in out.iter_mut().zip(b.chunks_exact(4)) {
            *o = i32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        }
    });
}

fn detect(lay: &mut Layout, cl: &[u8], ncmd: u32) -> bool {
    let w16 = Hdr::default().walks(cl, ncmd);
    let w6 = Hdr { six: true, v21: false }.walks(cl, ncmd);
    let w21 = Hdr { six: false, v21: true }.walks(cl, ncmd);
    if u8::from(w16) + u8::from(w6) + u8::from(w21) != 1 {
        if !lay.skipped_logged {
            lay.skipped_logged = true;
            let which = if w16 || w6 || w21 { "more than one" } else { "no" };
            eprintln!(
                "nxkit-audio: audren: command list of {ncmd} commands walks in {which} header layout (generated before the first update?), silent frame, the layout is taken from a later list"
            );
        }
        return false;
    }
    lay.done = true;
    if w6 {
        lay.hdr.six = true;
        lay.sink_id = 0x10;
        eprintln!("nxkit-audio: audren: command headers in the SDK 6 layout, DeviceSink is ID 0x10");
    }
    if w21 {
        lay.hdr.v21 = true;
        eprintln!("nxkit-audio: audren: command headers in the SDK 21 layout (no magic), DeviceSink 0x140 bytes");
    }
    true
}

fn histogram(lay: &mut Layout, cl: &[u8], ncmd: u32) {
    lay.hist_logged = true;
    let hdr = lay.hdr;
    let sink_sz = 0x148 + if hdr.v21 { -8 } else { 0 };
    let mut hist = [0u32; 64];
    let (mut cand, mut ncand, mut cand4, mut ncand4) = (-1, 0, -1, 0);
    let mut pos = 0x20usize;
    for _ in 0..ncmd {
        let Some(q) = cl.get(pos..) else { break };
        let sz = hdr.size(q);
        hist[(hdr.id(q) & 63) as usize] += 1;
        if sz == sink_sz && le_u32(q, hdr.sink_off(0x130)).wrapping_sub(1) < 6 {
            cand = hdr.id(q);
            ncand += 1;
        }
        if hdr.six && (sz == 0x130 || sz == 0x138) && le_u32(q, 0x120).wrapping_sub(1) < 6 {
            cand4 = hdr.id(q);
            ncand4 += 1;
            lay.sink5 = sz == 0x138;
        }
        pos += sz as usize;
    }
    let line: Vec<String> =
        hist.iter().enumerate().filter(|(_, n)| **n > 0).map(|(k, n)| format!("{k}x{n}")).collect();
    eprintln!("nxkit-audio: audren: first-frame commands {ncmd}, by command ID: {}", line.join(" "));
    let have = hist[(lay.sink_id & 63) as usize] != 0;
    if !have && ncand == 1 {
        eprintln!("nxkit-audio: audren: no command with ID {:#x}, taking the only 0x148 byte command, ID {cand:#x}, as DeviceSink", lay.sink_id);
        lay.sink_id = cand;
    } else if !have && ncand == 0 && ncand4 == 1 {
        eprintln!("nxkit-audio: audren: no command with ID {:#x}, taking the only 0x130 or 0x138 byte command, ID {cand4:#x}, as DeviceSink", lay.sink_id);
        lay.sink_id = cand4;
        lay.sink4 = true;
    } else if !have {
        eprintln!("nxkit-audio: [RED] audren: no DeviceSink (ID {:#x}) in the command list, {ncand} commands of {sink_sz:#x} bytes: the game will have no sound", lay.sink_id);
    }
}

fn sink_out(lay: &mut Layout, cap: &mut Cap, idx: usize, c: &[u8], h: &Head, dst: &mut [i16]) -> usize {
    let hdr = lay.hdr;
    let nin = le_u32(c, hdr.sink_off(if lay.sink4 { 0x120 } else { 0x130 })) as usize;
    let mut ins = [0i32; 6];
    for (j, slot) in ins.iter_mut().enumerate() {
        *slot = if lay.sink4 && !lay.sink5 {
            i32::from(byte(c, hdr.sink_off(0x124) + j) as i8)
        } else {
            le_i16(c, hdr.sink_off(if lay.sink4 { 0x124 } else { 0x134 }) + 2 * j)
        };
    }
    if !cap.sink_logged {
        cap.sink_logged = true;
        eprintln!(
            "nxkit-audio: audren: DeviceSink inputs {nin} ({} {} ...) rate {} samples per frame {}{}",
            ins[0],
            ins[1],
            h.rate,
            h.samples,
            if h.rate == 32000 { " (32 kHz: resampled to 48 kHz on the host)" } else { "" }
        );
    }
    let limit = if h.rate == 48000 { FRAME_MAX } else { FRAME_MAX * 2 / 3 - 1 };
    if nin == 0 || h.samples > limit {
        return 0;
    }
    if h.rate != 48000 && h.rate != 32000 {
        if !lay.rate_logged {
            lay.rate_logged = true;
            eprintln!("nxkit-audio: [RED] audren: renderer sample rate {} (the SDK takes 32000 or 48000): no sound", h.rate);
        }
        return 0;
    }
    let k = nin.min(6);
    let n = h.samples;
    for (j, input) in ins.iter().enumerate().take(k) {
        if *input < 0 {
            return 0;
        }
        let addr = h.mix.wrapping_add(*input as u64 * n as u64 * 4);
        read_i32s(addr, &mut cap.ch[j * FRAME_MAX..j * FRAME_MAX + n]);
    }
    // Channel order: front left, front right, center, LFE, back left, back right. Q15 coefficients.
    const CF: i64 = 32768;
    const CC: i64 = 23167;
    const CL: i64 = 8224;
    const CB: i64 = 23167;
    {
        let ch = &cap.ch;
        let st = &mut cap.st;
        let at = |j: usize, s: usize| i64::from(ch[j * FRAME_MAX + s]);
        for s in 0..n {
            let (l, r) = if k >= 6 {
                let c = at(2, s) * CC + at(3, s) * CL;
                ((at(0, s) * CF + c + at(4, s) * CB) >> 15, (at(1, s) * CF + c + at(5, s) * CB) >> 15)
            } else {
                let l = at(0, s);
                (l, if k > 1 { at(1, s) } else { l })
            };
            st[2 * s] = l as i32;
            st[2 * s + 1] = r as i32;
        }
    }
    let nout = if h.rate == 48000 { n } else { up_3_2(cap, n) };
    let mut loud = false;
    for (d, v) in dst.iter_mut().zip(cap.st.iter().take(2 * nout)) {
        let v = (*v).clamp(-32768, 32767);
        *d = v as i16;
        loud |= v != 0;
    }
    cap.frames += 1;
    if loud {
        cap.nonsilent += 1;
    }
    if cap.frames == 200 || cap.frames % 12000 == 0 {
        eprintln!("nxkit-audio: audren: renderer {idx} rendered {} frames, {} of them not silent", cap.frames, cap.nonsilent);
    }
    nout
}

/// Reads the DeviceSink of the command list `cl` into `dst` as stereo 16-bit samples. Returns the number of stereo
/// samples, or 0 when the list has no sound for the device. `dst` holds `FRAME_MAX * 2` values.
pub(crate) fn capture(lay: &mut Layout, cap: &mut Cap, idx: usize, cl: &[u8], dst: &mut [i16]) -> usize {
    if cl.len() < 0x20 {
        return 0;
    }
    let ncmd = le_u32(cl, 4);
    let h = Head { mix: le_u64(cl, 8), samples: le_u32(cl, 0x14) as usize, rate: le_u32(cl, 0x18) };
    // A list made before the game's first update need not walk in any layout. It decides nothing.
    if !lay.done && ncmd != 0 && !detect(lay, cl, ncmd) {
        return 0;
    }
    if !lay.done {
        return 0;
    }
    let hdr = lay.hdr;
    let mut pos = 0x20usize;
    for i in 0..ncmd {
        let ok = cl.get(pos..).filter(|q| hdr.ok(q));
        let Some(q) = ok else {
            if !lay.malformed_logged {
                lay.malformed_logged = true;
                eprintln!("nxkit-audio: [RED] audren: command {i} of the command list is malformed (offset {pos:#x}, list {:#x} bytes)", cl.len());
            }
            return 0;
        };
        pos += hdr.size(q) as usize;
    }
    if !lay.hist_logged && ncmd > 4 {
        histogram(lay, cl, ncmd);
    }
    let hdr = lay.hdr;
    let mut pos = 0x20usize;
    for _ in 0..ncmd {
        let Some(c) = cl.get(pos..) else { break };
        if hdr.id(c) == lay.sink_id && hdr.on(c) {
            return sink_out(lay, cap, idx, c, &h, dst);
        }
        pos += hdr.size(c) as usize;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsample_keeps_dc() {
        let mut cap = Cap::new();
        let mut total = 0;
        for _ in 0..4 {
            for v in cap.st.iter_mut().take(320) {
                *v = 1000;
            }
            total += up_3_2(&mut cap, 160);
        }
        assert!(total >= 4 * 240 - 2 && total <= 4 * 240 + 2);
    }

    #[test]
    fn short_list_is_silent() {
        let mut lay = Layout::new();
        let mut cap = Cap::new();
        let mut dst = vec![0i16; FRAME_MAX * 2];
        assert_eq!(capture(&mut lay, &mut cap, 0, &[0u8; 8], &mut dst), 0);
    }
}
