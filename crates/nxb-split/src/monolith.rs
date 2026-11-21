//! Monolith's containers: HCPS shader programs, standalone SLCT blocks, XBC1 and `cmp\0zlib` streams (`hcps_shaders`,
//! `slct_shaders`, `xbc_data`, `monolith_cmp_data`, `monolith_shaders`).

use crate::SplitError;
use crate::glslc::{glslc_shaders, stage};
use crate::prim::{Found, check, find_all, u32le};

fn red<T>(m: String) -> Result<T, SplitError> {
    Err(SplitError::Red(m))
}

/// Reads a u32 (a short buffer is the Python's `struct.error`, an unreadable file).
fn rd(d: &[u8], o: usize) -> Result<usize, SplitError> {
    u32le(d, o).map(|v| v as usize).ok_or_else(|| {
        SplitError::Red("struct.error: unpack_from requires a buffer of more bytes".into())
    })
}

/// `xbc_data`: (decoded bytes, end) of an XBC1 stream at `off`, `None` for bytes that merely look like a header.
fn xbc_data(d: &[u8], off: usize) -> Result<Option<(Vec<u8>, usize)>, SplitError> {
    if off + 48 > d.len() || &d[off..off + 4] != b"xbc1" {
        return Ok(None);
    }
    let (kind, raw, size) = (rd(d, off + 4)?, rd(d, off + 8)?, rd(d, off + 12)?);
    if !matches!(kind, 0 | 1 | 3) || size == 0 || off + 48 + size > d.len() {
        return Ok(None);
    }
    let data = &d[off + 48..off + 48 + size];
    let data = match kind {
        1 => crate::zlibx::zlib_stream(data, 1 << 30)
            .ok_or_else(|| SplitError::Red("zlib.error: damaged XBC1 payload".into()))?,
        3 => crate::zstd::decompress(data)?,
        _ => data.to_vec(),
    };
    if data.len() != raw {
        return red(format!(
            "XBC1 @{off:#x}: expected {raw} bytes, decoded {}",
            data.len()
        ));
    }
    Ok(Some((data, off + 48 + size)))
}

/// `monolith_cmp_data`: Firehawk effect archives, `cmp\0zlib`, stored and raw sizes, zlib payload.
fn cmp_data(d: &[u8], off: usize) -> Result<Option<(Vec<u8>, usize)>, SplitError> {
    if off + 16 > d.len() || &d[off..off + 8] != b"cmp\0zlib" {
        return Ok(None);
    }
    let (size, raw) = (rd(d, off + 8)?, rd(d, off + 12)?);
    if size == 0 || size > d.len() - off - 16 {
        return Ok(None);
    }
    let z = &d[off + 16..off + 16 + size];
    let bad = || SplitError::Red(format!("cmp zlib @{off:#x}: damaged payload"));
    let (data, used) = crate::zlibx::zlib_stream_used(z, 1 << 30).ok_or_else(bad)?;
    if used != size || data.len() != raw {
        return red(format!(
            "cmp zlib @{off:#x}: payload does not match stored/raw sizes"
        ));
    }
    Ok(Some((data, off + 16 + size)))
}

/// `hcps_shaders`: HCPS shader programs (SLCT ranges, NVSD stage records) paired within each variant.
pub fn hcps_shaders(d: &[u8], base: usize, label: &str) -> Result<Vec<Found>, SplitError> {
    let span = |off: usize, n: usize| -> Result<&[u8], SplitError> {
        if off < base || off > d.len() || n > d.len() - off {
            return red(format!("{label} HCPS @{base:#x}: section out of bounds"));
        }
        Ok(&d[off..off + n])
    };
    span(base, 72)?;
    let (table, count) = (rd(d, base + 8)?, rd(d, base + 12)?);
    let h: Vec<usize> = (0..6)
        .map(|i| rd(d, base + 24 + 4 * i))
        .collect::<Result<_, _>>()?;
    let (slct_off, slct_size, code_off, code_size, ctl_off, ctl_size) =
        (h[0], h[1], h[2], h[3], h[4], h[5]);
    span(base + table, count * 8)?;
    let slcts = span(base + slct_off, slct_size)?;
    let codes = span(base + code_off, code_size)?;
    let ctls = span(base + ctl_off, ctl_size)?;
    if code_size == 0 && ctl_size == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for i in 0..count {
        let s = rd(d, base + table + 8 * i)?;
        if s + 72 > slcts.len() || &slcts[s..s + 4] != b"SLCT" {
            return red(format!("{label} HCPS: invalid SLCT {i}"));
        }
        let (num, ptr) = (rd(slcts, s + 16)?, rd(slcts, s + 20)?);
        let (mut c, cn, mut k, kn) = (
            rd(slcts, s + 40)?,
            rd(slcts, s + 44)?,
            rd(slcts, s + 48)?,
            rd(slcts, s + 52)?,
        );
        let (ce, ke) = (c + cn, k + kn);
        if s + ptr + num * 8 > slcts.len() || ce > ctls.len() || ke > codes.len() {
            return red(format!(
                "{label} HCPS SLCT {i}: variant range out of bounds"
            ));
        }
        for j in 0..num {
            let (po, pn) = (rd(slcts, s + ptr + j * 8)?, rd(slcts, s + ptr + j * 8 + 4)?);
            let p = s + po;
            if pn < 32 || p + pn > slcts.len() {
                return red(format!("{label} HCPS: invalid NVSD"));
            }
            let (vo, vn) = (rd(slcts, p + 24)?, rd(slcts, p + 28)?);
            if vn == 0 || vo + vn * 20 > pn {
                return red(format!("{label} HCPS: invalid NVSD stage records"));
            }
            let r = p + vo + (vn - 1) * 20;
            let (vs, ps, vc, pc) = (
                rd(slcts, r + 4)?,
                rd(slcts, r + 8)?,
                rd(slcts, r + 12)?,
                rd(slcts, r + 16)?,
            );
            let (pend, kend) = (c + vc + pc, k + vs + ps);
            if pend > ce || kend > ke {
                return red(format!("{label} HCPS: program exceeds SLCT range"));
            }
            while c < pend {
                if c + 0x708 > pend {
                    return red(format!("{label} HCPS: truncated control"));
                }
                let (nc, nk) = (rd(ctls, c + 0x20)?, rd(ctls, c + 0x704)?);
                if nc < 0x760 || c + nc > pend || nk == 0 || k + nk > kend {
                    return red(format!("{label} HCPS: shader exceeds program range"));
                }
                let (ctl, code) = (&ctls[c..c + nc], &codes[k..k + nk]);
                c += (nc + 7) & !7;
                k += nk;
                let st = stage(ctl, code);
                let why = match st {
                    Some(s) => check(s, ctl, code).map(str::to_string),
                    None => Some("unknown stage".to_string()),
                };
                if let Some(why) = why {
                    return red(format!("{label} HCPS SLCT {i} variant {j}: {why}"));
                }
                out.push(Found {
                    place: format!("{label}#HCPS@{base:#x}#var{i}.{j}"),
                    stage: st.unwrap_or("vs"),
                    ctl: ctl.to_vec(),
                    code: code.to_vec(),
                });
            }
            if k != kend {
                return red(format!("{label} HCPS: unused program code bytes"));
            }
        }
        if c != ce || k != ke {
            return red(format!("{label} HCPS SLCT {i}: unused control/code bytes"));
        }
    }
    Ok(out)
}

/// `slct_shaders`: a standalone SLCT (offsets relative to itself) presented to the HCPS reader.
pub fn slct_shaders(d: &[u8], base: usize, label: &str) -> Result<Vec<Found>, SplitError> {
    if base + 72 > d.len() {
        return Ok(Vec::new());
    }
    let (c, cn, k, kn) = (
        rd(d, base + 40)?,
        rd(d, base + 44)?,
        rd(d, base + 48)?,
        rd(d, base + 52)?,
    );
    let size = (c + cn).max(k + kn);
    if c.min(k) < 72 || size > d.len() - base {
        return Ok(Vec::new());
    }
    let mut h = vec![0u8; 80];
    h[..4].copy_from_slice(b"HCPS");
    h[8..12].copy_from_slice(&72u32.to_le_bytes());
    h[12..16].copy_from_slice(&1u32.to_le_bytes());
    for i in 0..6 {
        let v = if i % 2 == 0 { 80 } else { size as u32 };
        h[24 + 4 * i..28 + 4 * i].copy_from_slice(&v.to_le_bytes());
    }
    h.extend_from_slice(&d[base..base + size]);
    hcps_shaders(&h, 0, &format!("{label}#SLCT@{base:#x}"))
}

/// `monolith_shaders`: decoded archive entries and their XBC1 streams, HCPS and GLSLC outputs.
pub fn monolith_shaders(d: &[u8], label: &str, out: &mut Vec<Found>) -> Result<(), SplitError> {
    let mut pos = 0usize;
    let mut compressed: Vec<(usize, usize)> = Vec::new();
    let mut markers: Vec<(usize, bool, usize)> = find_all(d, b"xbc1")
        .map(|o| (o, true, 4))
        .chain(find_all(d, b"cmp\0zlib").map(|o| (o, false, 8)))
        .collect();
    markers.sort();
    for (off, xbc, size) in markers {
        if off < pos {
            continue;
        }
        let r = if xbc {
            xbc_data(d, off)?
        } else {
            cmp_data(d, off)?
        };
        match r {
            None => pos = off + size,
            Some((data, end)) => {
                pos = end;
                compressed.push((off, pos));
                let kind = if xbc { "XBC1" } else { "cmp-zlib" };
                monolith_shaders(&data, &format!("{label}#{kind}@{off:#x}"), out)?;
            }
        }
    }
    let mut ci = 0;
    for base in find_all(d, b"HCPS") {
        while ci < compressed.len() && compressed[ci].1 <= base {
            ci += 1;
        }
        if ci < compressed.len() && compressed[ci].0 <= base {
            continue;
        }
        if base + 8 <= d.len() && matches!(rd(d, base + 4)?, 1000 | 1010) {
            out.extend(hcps_shaders(d, base, label)?);
        }
    }
    out.extend(glslc_shaders(d, label, false, &compressed)?);
    Ok(())
}
