//! A zstd decoder (RFC 8878), std only, with dictionaries, and [`raw_frame`], a writer of raw-block frames. Written
//! here as the workspace has no zstd crate offline (deps.toml O14), it belongs in `nxb-codec` once its owner takes it.
//! Checksums are skipped: a damaged frame almost always fails structurally first.

use crate::SplitError;

mod dict;
mod huf;
mod raw;
mod stream;
#[cfg(test)]
mod tests;

pub use dict::{Dicts, decompress_dict, dict_id};
use huf::Huf;
pub use raw::raw_frame;
pub use stream::*;

pub(crate) fn err(m: &str) -> SplitError {
    SplitError::Format(format!("zstd: {m}"))
}

pub(crate) type R<T> = Result<T, SplitError>;

pub(crate) fn hb(x: u32) -> u32 {
    31 - x.leading_zeros()
}

/// Bits read backward from the end of a stream (after the final marker bit).
pub(crate) struct Back<'a> {
    d: &'a [u8],
    pub(crate) pos: i64, // bits left
}

impl<'a> Back<'a> {
    pub(crate) fn new(d: &'a [u8]) -> R<Back<'a>> {
        let last = *d.last().ok_or_else(|| err("empty bitstream"))?;
        if last == 0 {
            return Err(err("bitstream without end marker"));
        }
        Ok(Back {
            d,
            pos: (d.len() as i64) * 8 - 8 + hb(last as u32) as i64,
        })
    }
    #[inline]
    pub(crate) fn read(&mut self, n: u32) -> u64 {
        if n == 0 {
            return 0;
        }
        self.pos -= n as i64;
        let mut v = 0u64;
        // bit i of the value = stream bit pos + i (bits below 0 read as zero)
        let start = self.pos;
        let first_byte = start.div_euclid(8);
        let shift = start.rem_euclid(8) as u32;
        // fast path: one 8-byte load (n <= 56 bits after the shift, always within the stream)
        if first_byte >= 0 && n + shift <= 64 {
            let i = first_byte as usize;
            if let Some(b) = self.d.get(i..i + 8) {
                let w = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
                let mask = if n == 64 { u64::MAX } else { (1u64 << n) - 1 };
                return (w >> shift) & mask;
            }
        }
        for k in 0..((n + shift).div_ceil(8) as i64) {
            let idx = first_byte + k;
            let b = if idx >= 0 && (idx as usize) < self.d.len() {
                self.d[idx as usize] as u64
            } else {
                0
            };
            v |= b << (8 * k as u32);
        }
        (v >> shift) & ((1u64 << n) - 1)
    }
}

/// Bits read forward (FSE table descriptions).
pub(crate) struct Fwd<'a> {
    pub(crate) d: &'a [u8],
    pub(crate) pos: usize,
}

impl Fwd<'_> {
    pub(crate) fn read(&mut self, n: u32) -> R<u32> {
        let mut v = 0u32;
        for i in 0..n {
            let byte = *self
                .d
                .get((self.pos + i as usize) / 8)
                .ok_or_else(|| err("table description past the end"))?;
            v |= (((byte >> ((self.pos + i as usize) % 8)) & 1) as u32) << i;
        }
        self.pos += n as usize;
        Ok(v)
    }
    pub(crate) fn bytes(&self) -> usize {
        self.pos.div_ceil(8)
    }
}

#[derive(Clone, Default)]
pub(crate) struct Fse {
    pub(crate) al: u32,
    pub(crate) sym: Vec<u8>,
    bits: Vec<u8>,
    base: Vec<u16>,
}

impl Fse {
    fn from_norm(norm: &[i16], al: u32) -> R<Fse> {
        let size = 1usize << al;
        let mut sym = vec![0u8; size];
        let mut next = vec![0u32; norm.len()];
        let mut high = size;
        for (s, &f) in norm.iter().enumerate() {
            if f == -1 {
                high -= 1;
                sym[high] = s as u8;
                next[s] = 1;
            } else {
                next[s] = f.max(0) as u32;
            }
        }
        let step = (size >> 1) + (size >> 3) + 3;
        let mut pos = 0usize;
        for (s, &f) in norm.iter().enumerate() {
            for _ in 0..f.max(0) {
                sym[pos] = s as u8;
                loop {
                    pos = (pos + step) & (size - 1);
                    if pos < high {
                        break;
                    }
                }
            }
        }
        if pos != 0 {
            return Err(err("FSE table spread"));
        }
        let mut bits = vec![0u8; size];
        let mut base = vec![0u16; size];
        for i in 0..size {
            let s = sym[i] as usize;
            let n = next[s];
            next[s] += 1;
            if n == 0 {
                return Err(err("FSE state"));
            }
            let nb = al - hb(n);
            bits[i] = nb as u8;
            base[i] = ((n << nb) as usize - size) as u16;
        }
        Ok(Fse {
            al,
            sym,
            bits,
            base,
        })
    }

    fn rle(s: u8) -> Fse {
        Fse {
            al: 0,
            sym: vec![s],
            bits: vec![0],
            base: vec![0],
        }
    }

    pub(crate) fn read(f: &mut Fwd<'_>, max_al: u32, max_sym: usize) -> R<Fse> {
        let al = f.read(4)? + 5;
        if al > max_al {
            return Err(err("FSE accuracy log too large"));
        }
        let mut remaining: i32 = 1 << al;
        let mut norm: Vec<i16> = Vec::new();
        while remaining > 0 && norm.len() <= max_sym {
            let bits = hb((remaining + 1) as u32) + 1;
            let mut val = f.read(bits)?;
            let lower = (1u32 << (bits - 1)) - 1;
            let threshold = (1u32 << bits) - 1 - (remaining as u32 + 1);
            if (val & lower) < threshold {
                f.pos -= 1;
                val &= lower;
            } else if val > lower {
                val -= threshold;
            }
            let p = val as i32 - 1;
            remaining -= p.abs();
            norm.push(p as i16);
            if p == 0 {
                loop {
                    let rep = f.read(2)?;
                    norm.extend(std::iter::repeat_n(0, rep as usize));
                    if rep != 3 {
                        break;
                    }
                }
            }
        }
        if remaining != 0 || norm.len() > max_sym + 1 {
            return Err(err("FSE table description"));
        }
        Fse::from_norm(&norm, al)
    }

    #[inline]
    pub(crate) fn init(&self, b: &mut Back<'_>) -> usize {
        b.read(self.al) as usize
    }
    #[inline]
    pub(crate) fn update(&self, state: &mut usize, b: &mut Back<'_>) {
        let s = *state;
        *state = self.base[s] as usize + b.read(self.bits[s] as u32) as usize;
    }
}

const LL_BASE: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64,
    128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
];
const LL_BITS: [u32; 36] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16,
];
const ML_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
    28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 131, 259, 515, 1027,
    2051, 4099, 8195, 16387, 32771, 65539,
];
const ML_BITS: [u32; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];
const LL_DEF: [i16; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
const ML_DEF: [i16; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
const OF_DEF: [i16; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

#[derive(Default)]
struct State {
    huf: Option<Huf>,
    ll: Option<Fse>,
    of: Option<Fse>,
    ml: Option<Fse>,
    rep: [usize; 3],
}

fn literals(d: &[u8], st: &mut State) -> R<(Vec<u8>, usize)> {
    let b0 = *d.first().ok_or_else(|| err("literals header"))? as usize;
    let ty = b0 & 3;
    let sf = (b0 >> 2) & 3;
    let byte = |i: usize| {
        d.get(i)
            .copied()
            .map(|x| x as usize)
            .ok_or_else(|| err("literals header"))
    };
    if ty < 2 {
        let (n, h) = match sf {
            0 | 2 => (b0 >> 3, 1),
            1 => ((b0 >> 4) + (byte(1)? << 4), 2),
            _ => ((b0 >> 4) + (byte(1)? << 4) + (byte(2)? << 12), 3),
        };
        if ty == 0 {
            let v = d
                .get(h..h + n)
                .ok_or_else(|| err("raw literals past the end"))?
                .to_vec();
            return Ok((v, h + n));
        }
        return Ok((vec![byte(h)? as u8; n], h + 1));
    }
    let (h, bits, streams) = match sf {
        0 => (3, 10, 1),
        1 => (3, 10, 4),
        2 => (4, 14, 4),
        _ => (5, 18, 4),
    };
    let mut v = 0u64;
    for i in 0..h {
        v |= (byte(i)? as u64) << (8 * i);
    }
    let regen = ((v >> 4) & ((1 << bits) - 1)) as usize;
    let comp = ((v >> (4 + bits)) & ((1 << bits) - 1)) as usize;
    let mut src = d
        .get(h..h + comp)
        .ok_or_else(|| err("literals past the end"))?;
    if ty == 2 {
        let (t, used) = Huf::read(src)?;
        st.huf = Some(t);
        src = &src[used..];
    }
    let huf = st
        .huf
        .as_ref()
        .ok_or_else(|| err("treeless literals without a previous tree"))?;
    let mut out = Vec::with_capacity(regen);
    if streams == 1 {
        huf.decode_stream(src, regen, &mut out)?;
    } else {
        if src.len() < 6 {
            return Err(err("jump table"));
        }
        let s = [
            u16::from_le_bytes([src[0], src[1]]) as usize,
            u16::from_le_bytes([src[2], src[3]]) as usize,
            u16::from_le_bytes([src[4], src[5]]) as usize,
        ];
        let seg = regen.div_ceil(4);
        let mut at = 6;
        for &len in &s {
            huf.decode_stream(
                src.get(at..at + len)
                    .ok_or_else(|| err("literal stream past the end"))?,
                seg,
                &mut out,
            )?;
            at += len;
        }
        let last = regen
            .checked_sub(3 * seg)
            .ok_or_else(|| err("literal stream sizes"))?;
        huf.decode_stream(
            src.get(at..).ok_or_else(|| err("literal stream"))?,
            last,
            &mut out,
        )?;
    }
    Ok((out, h + comp))
}

/// A sequence code's table parameters: predefined distribution and its accuracy log, maximum accuracy log, maximum symbol.
struct Spec(&'static [i16], u32, u32, usize);

fn table(mode: u8, d: &[u8], at: &mut usize, prev: &mut Option<Fse>, spec: Spec) -> R<()> {
    let Spec(def, def_al, max_al, max_sym) = spec;
    match mode {
        0 => *prev = Some(Fse::from_norm(def, def_al)?),
        1 => {
            let s = *d.get(*at).ok_or_else(|| err("RLE symbol"))?;
            *at += 1;
            *prev = Some(Fse::rle(s));
        }
        2 => {
            let mut f = Fwd {
                d: d.get(*at..).unwrap_or(&[]),
                pos: 0,
            };
            *prev = Some(Fse::read(&mut f, max_al, max_sym)?);
            *at += f.bytes();
        }
        _ => {
            if prev.is_none() {
                return Err(err("repeat table without a previous one"));
            }
        }
    }
    Ok(())
}

fn block(d: &[u8], st: &mut State, out: &mut Vec<u8>, frame_start: usize) -> R<()> {
    let (lit, used) = literals(d, st)?;
    let s = &d[used..];
    let b0 = *s.first().ok_or_else(|| err("sequences header"))? as usize;
    let (nseq, mut at) = if b0 < 128 {
        (b0, 1)
    } else if b0 < 255 {
        (
            ((b0 - 128) << 8) + *s.get(1).ok_or_else(|| err("sequences header"))? as usize,
            2,
        )
    } else {
        let (a, b) = (
            *s.get(1).ok_or_else(|| err("seq"))? as usize,
            *s.get(2).ok_or_else(|| err("seq"))? as usize,
        );
        (a + (b << 8) + 0x7F00, 3)
    };
    if nseq == 0 {
        out.extend_from_slice(&lit);
        return Ok(());
    }
    let modes = *s.get(at).ok_or_else(|| err("sequence modes"))?;
    at += 1;
    table(modes >> 6, s, &mut at, &mut st.ll, Spec(&LL_DEF, 6, 9, 35))?;
    table(
        (modes >> 4) & 3,
        s,
        &mut at,
        &mut st.of,
        Spec(&OF_DEF, 5, 8, 31),
    )?;
    table(
        (modes >> 2) & 3,
        s,
        &mut at,
        &mut st.ml,
        Spec(&ML_DEF, 6, 9, 52),
    )?;
    let (Some(ll), Some(of), Some(ml)) = (st.ll.as_ref(), st.of.as_ref(), st.ml.as_ref()) else {
        return Err(err("sequence tables"));
    };
    let mut b = Back::new(s.get(at..).ok_or_else(|| err("sequence bitstream"))?)?;
    let (mut sl, mut so, mut sm) = (ll.init(&mut b), of.init(&mut b), ml.init(&mut b));
    let mut lp = 0usize;
    for i in 0..nseq {
        let ofc = of.sym[so] as u32;
        let llc = ll.sym[sl] as usize;
        let mlc = ml.sym[sm] as usize;
        if ofc > 31 || llc >= 36 || mlc >= 53 {
            return Err(err("sequence code"));
        }
        let ofv = (1usize << ofc) + b.read(ofc) as usize;
        let mlen = ML_BASE[mlc] as usize + b.read(ML_BITS[mlc]) as usize;
        let llen = LL_BASE[llc] as usize + b.read(LL_BITS[llc]) as usize;
        let rep = &mut st.rep;
        let off = if ofv > 3 {
            let o = ofv - 3;
            *rep = [o, rep[0], rep[1]];
            o
        } else {
            let mut idx = ofv - 1;
            if llen == 0 {
                idx += 1;
            }
            if idx == 0 {
                rep[0]
            } else {
                let o = if idx < 3 {
                    rep[idx]
                } else {
                    rep[0].wrapping_sub(1)
                };
                if idx > 1 {
                    rep[2] = rep[1];
                }
                rep[1] = rep[0];
                rep[0] = o;
                o
            }
        };
        if i + 1 < nseq {
            ll.update(&mut sl, &mut b);
            ml.update(&mut sm, &mut b);
            of.update(&mut so, &mut b);
        }
        out.extend_from_slice(
            lit.get(lp..lp + llen)
                .ok_or_else(|| err("literal length past the literals"))?,
        );
        lp += llen;
        if off == 0 || off > out.len() - frame_start {
            return Err(err("match offset outside the frame"));
        }
        let start = out.len() - off;
        if off >= mlen {
            out.extend_from_within(start..start + mlen);
        } else {
            for k in 0..mlen {
                let c = out[start + k];
                out.push(c);
            }
        }
    }
    if b.pos != 0 {
        return Err(err("sequence bitstream length"));
    }
    out.extend_from_slice(&lit[lp..]);
    Ok(())
}

/// Output limit of [`decompress`]: a frame of RLE blocks expands about 30,000 times, so a small file could ask for
/// terabytes. Larger outputs are `Unsupported` (the reference step decodes them).
pub const DEFAULT_LIMIT: usize = 1 << 30;

/// Every frame of `d` decoded, skippable ones skipped. A dictionary frame or output over [`DEFAULT_LIMIT`] is `Unsupported`.
pub fn decompress(d: &[u8]) -> R<Vec<u8>> {
    frames(d, DEFAULT_LIMIT, &Dicts::new(), |n| {
        SplitError::Unsupported(format!("zstd output over {n} bytes"))
    })
}

/// Like [`decompress`] when the size of the output is known: more than `limit` bytes is a format error.
pub fn decompress_max(d: &[u8], limit: usize) -> R<Vec<u8>> {
    frames(d, limit, &Dicts::new(), |n| {
        err(&format!("output larger than {n} bytes"))
    })
}

fn frames(d: &[u8], limit: usize, dicts: &Dicts, over: impl Fn(usize) -> SplitError) -> R<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < d.len() {
        let magic = d
            .get(i..i + 4)
            .map(|m| u32::from_le_bytes([m[0], m[1], m[2], m[3]]))
            .ok_or_else(|| err("truncated frame"))?;
        if magic & 0xFFFF_FFF0 == 0x184D_2A50 {
            let n = d
                .get(i + 4..i + 8)
                .map(|m| u32::from_le_bytes([m[0], m[1], m[2], m[3]]))
                .ok_or_else(|| err("skippable"))?;
            i += 8 + n as usize;
            continue;
        }
        if magic != 0xFD2F_B528 {
            return Err(err("unknown frame magic"));
        }
        let fhd = *d.get(i + 4).ok_or_else(|| err("frame header"))?;
        let single = fhd >> 5 & 1 == 1;
        let did = [0usize, 1, 2, 4][(fhd & 3) as usize];
        let fcs = match fhd >> 6 {
            0 => usize::from(single),
            1 => 2,
            2 => 4,
            _ => 8,
        };
        let mut at = i + 5 + usize::from(!single);
        let id = d.get(at..at + did).map_or(0, |b| {
            b.iter().rev().fold(0u32, |a, &x| a << 8 | u32::from(x))
        });
        at += did + fcs;
        let frame_base = out.len();
        let mut st = State {
            rep: [1, 4, 8],
            ..State::default()
        };
        let mut dict_len = 0;
        if id != 0 {
            let Some(dict) = dicts.0.get(&id) else {
                return Err(if dicts.is_empty() {
                    SplitError::Unsupported("zstd frames with a dictionary".into())
                } else {
                    SplitError::Red(format!(
                        "a frame needs zstd dictionary {id}, which Pack/ZsDic.pack.zs does not have"
                    ))
                });
            };
            // the dictionary's content is the history before the frame: matches may reach into it
            out.extend_from_slice(&dict.content);
            dict_len = dict.content.len();
            st.huf = dict.huf.clone();
            st.ll = Some(dict.ll.clone());
            st.of = Some(dict.of.clone());
            st.ml = Some(dict.ml.clone());
            st.rep = dict.rep;
        }
        let start_len = out.len() - dict_len;
        loop {
            let h = d.get(at..at + 3).ok_or_else(|| err("block header"))?;
            let h = h[0] as usize | (h[1] as usize) << 8 | (h[2] as usize) << 16;
            let (last, ty, size) = (h & 1 == 1, (h >> 1) & 3, h >> 3);
            at += 3;
            match ty {
                0 => out.extend_from_slice(
                    d.get(at..at + size)
                        .ok_or_else(|| err("raw block past the end"))?,
                ),
                1 => {
                    let b = *d.get(at).ok_or_else(|| err("RLE block"))?;
                    out.extend(std::iter::repeat_n(b, size));
                }
                2 => {
                    // matches may reach into earlier blocks of the frame, not earlier frames
                    let src = d
                        .get(at..at + size)
                        .ok_or_else(|| err("compressed block past the end"))?;
                    block(src, &mut st, &mut out, start_len)?;
                }
                _ => return Err(err("reserved block type")),
            }
            // a block adds at most 2 MiB (RLE), so the check after the block bounds the memory
            if out.len() > limit {
                return Err(over(limit));
            }
            at += if ty == 1 { 1 } else { size };
            if last {
                break;
            }
        }
        if fhd & 4 != 0 {
            at += 4;
        }
        if dict_len > 0 {
            out.drain(frame_base..frame_base + dict_len);
        }
        i = at;
    }
    Ok(out)
}
