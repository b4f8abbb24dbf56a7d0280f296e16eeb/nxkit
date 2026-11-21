//! Retro Studios' RFRM forms and packs: SNVN chunks (zlib records of NVN programs), PACK archives with their ADIR
//! chunk, and the LZSS-compressed startup archive in main (`snvn_shaders`, `rfrm_shaders`, `retro_main_forms`).

use crate::SplitError;
use crate::glslc::stage;
use crate::prim::{CTL_MAGIC, Found, check, find_all, memfind};
use crate::rd::{red, sl, u32be, u32le, u64be, u64le, zlib_all};

type R<T> = Result<T, SplitError>;

/// `rfrm_order`: `true` for little-endian (the reading that is a length is the small one).
fn little(d: &[u8], o: usize) -> R<bool> {
    Ok(u64le(d, o + 4)? < u64be(d, o + 4)?)
}

fn rd32(d: &[u8], o: usize, le: bool) -> R<usize> {
    if le { u32le(d, o) } else { u32be(d, o) }
}

fn rd64(d: &[u8], o: usize, le: bool) -> R<usize> {
    Ok(if le { u64le(d, o)? } else { u64be(d, o)? } as usize)
}

/// `rfrm_chunk`: (data start, data end, little-endian) of the chunk at `o`.
fn chunk(d: &[u8], o: usize) -> R<(usize, usize, bool)> {
    let e = little(d, o)?;
    Ok((o + 0x18, o + 0x18 + rd64(d, o + 4, e)?, e))
}

/// Position of `pat` in `d[from..to]`.
fn find_in(d: &[u8], pat: &[u8], from: usize, to: usize) -> Option<usize> {
    memfind(sl(d, from, to), pat).map(|p| p + from.min(d.len()))
}

type Rec = (usize, Vec<(Vec<u8>, Vec<u8>)>);

/// `snvn_records`: (offset, [(control, code)]) of each program record of an SNVN chunk's data `d[a..b]`.
fn snvn_records(d: &[u8], a: usize, b: usize, label: &str, le: bool) -> R<Vec<Rec>> {
    let mut out = Vec::new();
    let mut i = a + 12;
    while let Some(p) = find_in(d, &[0x78], i + 1, b) {
        i = p;
        let (ncode, nctl, nz) = (
            rd32(d, i - 13, le)?,
            rd32(d, i - 9, le)?,
            rd32(d, i - 4, le)?,
        );
        if !(nctl != 0
            && nctl % 0x40 == 0
            && nctl < 1 << 20
            && ncode < 1 << 24
            && 0 < nz
            && nz <= b - i)
        {
            continue;
        }
        let Some((y, used)) = crate::zlibx::zlib_stream_used(sl(d, i, i + nz), 1 << 31) else {
            continue;
        };
        if used != nz || y.len() != ncode + nctl {
            continue;
        }
        let (mut stages, mut k, mut c) = (Vec::new(), 0usize, ncode);
        while c < y.len() {
            if sl(&y, c, c + 4) != CTL_MAGIC {
                return red(format!(
                    "{label} SNVN record @{i:#x}: no control section at {c:#x}"
                ));
            }
            let (mut n, nk) = (u32le(&y, c + 0x20)?, u32le(&y, c + 0x704)?);
            let (x, xn) = (u32le(&y, c + 0x7e8)?, u32le(&y, c + 0x7ec)?);
            if xn != 0 && x >= n {
                n = x + xn;
            }
            stages.push((sl(&y, c, c + n).to_vec(), sl(&y, k, k + nk).to_vec()));
            k += nk;
            c += n;
        }
        if k != ncode {
            return red(format!(
                "{label} SNVN record @{i:#x}: the code areas end at {k:#x}, the control sections start at {ncode:#x}"
            ));
        }
        out.push((i, stages));
        i += nz - 1;
    }
    Ok(out)
}

/// `snvn_shaders`: the programs of every SNVN chunk of the form `d`.
pub fn snvn_shaders(d: &[u8], label: &str) -> R<Vec<Found>> {
    let mut out = Vec::new();
    let mut p = memfind(d, b"SNVN");
    while let Some(at) = p {
        let (a, b, e) = chunk(d, at)?;
        for (i, stages) in snvn_records(d, a, b, label, e)? {
            for (ctl, code) in stages {
                let st = stage(&ctl, &code);
                let why = match st {
                    Some(s) => check(s, &ctl, &code).map(str::to_string),
                    None => Some("no stage in the code area".into()),
                };
                if let Some(w) = why {
                    return red(format!("{label} SNVN record @{i:#x}: {w}"));
                }
                out.push(Found {
                    place: format!("{label}#SNVN@{i:#x}#var"),
                    stage: st.unwrap_or("vs"),
                    ctl,
                    code,
                });
            }
        }
        p = find_in(d, b"SNVN", b, d.len());
    }
    Ok(out)
}

/// `retro_main_forms`: RFRM forms compressed into main with CBufferedCompression (type 1, LZSS on 8-bit units).
pub fn retro_main_forms(d: &[u8], label: &str) -> R<Vec<Found>> {
    let mut out = Vec::new();
    for p in find_all(d, b"RFRM") {
        if p < 5 || d[p - 5] != 1 || d[p - 4..p - 1] != [0, 0, 0] {
            continue;
        }
        let Some(head) = retro_lzss8(&d[p - 1..], 0x20) else {
            continue;
        };
        if head.is_empty() || &head[..4] != b"RFRM" || &head[0x14..0x18] != b"MTRL" {
            continue;
        }
        let n = 0x20 + rd64(&head, 4, little(&head, 0)?)?;
        if let Some(x) = retro_lzss8(&d[p - 1..], n).filter(|x| !x.is_empty()) {
            out.extend(snvn_shaders(&x, &format!("{label}@{:#x}", p - 5))?);
        }
    }
    Ok(out)
}

/// `rfrm_shaders`: a Retro Studios RFRM file, a PACK archive or a single form.
pub fn rfrm_shaders(d: &[u8], label: &str) -> R<Vec<Found>> {
    if sl(d, 0x14, 0x18) != b"PACK" {
        return snvn_shaders(d, label);
    }
    let Some(a) = find_in(d, b"ADIR", 0x20, d.len()) else {
        return red(format!("{label}: RFRM PACK without an ADIR chunk"));
    };
    let (_, end, o) = chunk(d, a)?;
    let n = rd32(d, a + 0x18, o)?;
    let size = end.wrapping_sub(a + 0x1c).checked_div(n).unwrap_or(36);
    if !(size == 36 || size == 52) || a + 0x1c + n * size != end {
        return red(format!(
            "{label}: ADIR of {n} entries in {:#x} bytes, neither 36 nor 52 bytes each",
            end.wrapping_sub(a + 0x1c)
        ));
    }
    let mut out = Vec::new();
    for i in 0..n {
        let e = a + 0x1c + size * i;
        if sl(d, e, e + 4) != b"MTRL" {
            continue;
        }
        let at = e + if size == 52 { 28 } else { 20 };
        let (off, ln) = (rd64(d, at, o)?, rd64(d, at + 8, o)?);
        let stored = if size == 52 { rd64(d, e + 44, o)? } else { ln };
        let id: String = sl(d, e + 4, e + 20)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if stored != ln {
            return red(format!(
                "{label}: MTRL resource {id} is stored compressed ({stored:#x} of {ln:#x} bytes), which this reader does not decode"
            ));
        }
        let r = sl(d, off, off + ln);
        if sl(r, 0, 4) != b"RFRM" {
            return red(format!("{label}: MTRL resource {id} is not an RFRM form"));
        }
        let form = if sl(r, 0x20, 0x21) == [0x78] {
            zlib_all(sl(r, 0x20, r.len()), label)?
        } else {
            r.to_vec()
        };
        out.extend(snvn_shaders(&form, &format!("{label}/{id}"))?);
    }
    Ok(out)
}

/// `retro_lzss8`: the first `n` bytes of Retro's LZSS stream, or `None` when it runs out or points before its start.
pub(crate) fn retro_lzss8(s: &[u8], n: usize) -> Option<Vec<u8>> {
    let (mut out, mut i, mut flags, mut left) = (Vec::new(), 0usize, 0u8, 0u32);
    while out.len() < n {
        if left == 0 {
            flags = *s.get(i)?;
            i += 1;
            left = 8;
        }
        if flags & 0x80 != 0 {
            let (a, b) = (*s.get(i)? as usize, *s.get(i + 1)? as usize);
            let (ln, dist) = ((a >> 4) + 3, (a & 0xf) << 8 | b);
            i += 2;
            if dist == 0 || dist > out.len() {
                return None;
            }
            for _ in 0..ln {
                out.push(out[out.len() - dist]);
            }
        } else {
            out.push(*s.get(i)?);
            i += 1;
        }
        flags <<= 1;
        left -= 1;
    }
    out.truncate(n);
    Some(out)
}
