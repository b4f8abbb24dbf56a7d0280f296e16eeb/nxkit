//! The bit readers of a zstd stream (backward and forward) and the FSE tables read through them.

use super::{R, err, hb};

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
    pub(super) fn from_norm(norm: &[i16], al: u32) -> R<Fse> {
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

    pub(super) fn rle(s: u8) -> Fse {
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
