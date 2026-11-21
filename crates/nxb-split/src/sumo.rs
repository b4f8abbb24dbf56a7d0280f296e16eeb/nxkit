//! Sumo's MuxFDMR containers, Ninty_NX descriptors and NVNsh records, and Phyre's effects (`sumo_members`,
//! `sumo_program`, `sumo_shaders`, `phyre_shaders` of `shader_split.py`).

use crate::SplitError;
use crate::glslc::stage;
use crate::prim::{CTL_MAGIC, Found, GFX_MAGIC, STAGES, check, find_all, sph_stage, u32le};

const NX: [u8; 16] = *b"Ninty_NX\0\0\0\0\0\0\0\0";

fn red(m: String) -> SplitError {
    SplitError::Red(m)
}

fn u64at(d: &[u8], o: usize) -> Option<u64> {
    d.get(o..o.checked_add(8)?)
        .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

fn cut(d: &[u8], a: usize, n: usize) -> Vec<u8> {
    let a = a.min(d.len());
    d[a..a.saturating_add(n).min(d.len())].to_vec()
}

/// Whether `d` is a MuxFDMR version 2 container.
pub fn is_mux(d: &[u8]) -> bool {
    d.starts_with(b"MuxFDMR\x02")
}

/// `sumo_members`: (name suffix, bytes) of each zlib stream.
pub fn sumo_members(d: &[u8], label: &str) -> Result<Vec<(String, Vec<u8>)>, SplitError> {
    if d.len() < 44 {
        return Err(red(format!("{label}: truncated MuxFDMR header")));
    }
    let head = u32le(d, 8).unwrap_or(0) as usize;
    let n = d[18] as usize;
    if d[17] != 2 || !(n == 1 || n == 2) || head != 44 + 4 * n || head > d.len() {
        return Err(red(format!("{label}: unsupported MuxFDMR layout")));
    }
    let sizes: Vec<usize> = (0..n)
        .map(|i| u32le(d, 40 + 4 * i).unwrap_or(0) as usize)
        .collect();
    if head + sizes.iter().sum::<usize>() != d.len() {
        return Err(red(format!("{label}: MuxFDMR streams do not fit the file")));
    }
    let mut head = head;
    let mut out = Vec::new();
    for (i, &size) in sizes.iter().enumerate() {
        let expected = u32le(d, 24 + 8 * i).unwrap_or(0) as usize;
        let ok = crate::zlibx::zlib_stream_used(&d[head..head + size], expected + 1)
            .filter(|(raw, used)| *used == size && raw.len() == expected);
        let Some((raw, _)) = ok else {
            return Err(red(format!(
                "{label}: MuxFDMR stream {i} length does not match its header"
            )));
        };
        out.push((format!("#MUX{i}"), raw));
        head += size;
    }
    Ok(out)
}

struct Range {
    base: usize,
    size: usize,
    co: usize,
    ko: usize,
    cn: usize,
    kn: usize,
}

fn program(d: &[u8], r: Range, label: String, want: Option<&str>) -> Result<Found, SplitError> {
    let Range {
        base,
        size,
        co,
        ko,
        cn,
        kn,
    } = r;
    if size < 0x34
        || base + size > d.len()
        || cn < 0x760
        || kn < 0x40
        || co.min(ko) < 0x34
        || (co + cn).max(ko + kn) > size
        || !(co + cn <= ko || ko + kn <= co)
    {
        return Err(red(format!(
            "{label}: shader ranges do not fit the container"
        )));
    }
    let (ctl, code) = (cut(d, base + co, cn), cut(d, base + ko, kn));
    let st = stage(&ctl, &code);
    let Some(st) = st.filter(|s| want.is_none_or(|w| w == *s)) else {
        return Err(red(format!("{label}: invalid shader stage")));
    };
    if let Some(w) = check(st, &ctl, &code) {
        return Err(red(format!("{label}: {w}")));
    }
    Ok(Found {
        place: label,
        stage: st,
        ctl,
        code,
    })
}

/// `sumo_shaders`.
pub fn sumo_shaders(d: &[u8], label: &str) -> Result<Vec<Found>, SplitError> {
    let mut out = Vec::new();
    for m in find_all(d, &NX) {
        if m < 0x30 {
            continue;
        }
        let r = m - 0x30;
        let Some(back) = u64at(d, r + 0x28) else {
            continue;
        };
        let Some(base) = r.checked_sub(back as usize) else {
            continue;
        };
        if r < base + 0x38 {
            continue;
        }
        let Some(size) = u64at(d, base + 8).map(|x| x as usize) else {
            continue;
        };
        if r + 0x40 > base + size || base + size > d.len() {
            continue;
        }
        if u32le(d, base + 4) != Some(1) {
            return Err(red(format!(
                "{label}: unsupported Ninty_NX container version"
            )));
        }
        let (co, ko) = (
            u64at(d, r).unwrap_or(0) as usize,
            u64at(d, r + 8).unwrap_or(0) as usize,
        );
        let (cn, kn) = (
            u32le(d, r + 16).unwrap_or(0) as usize,
            u32le(d, r + 20).unwrap_or(0) as usize,
        );
        out.push(program(
            d,
            Range {
                base,
                size,
                co,
                ko,
                cn,
                kn,
            },
            format!("{label}#SUMO@{r:#x}"),
            None,
        )?);
    }
    for h in find_all(d, b"NVNsh\0") {
        let size = u32le(d, h + 8).unwrap_or(0) as usize;
        if h + 0x34 > d.len() || !(0x34 <= size && size <= d.len() - h) {
            continue;
        }
        if d.get(h + 6..h + 8) != Some(&[1, 0]) {
            return Err(red(format!("{label}: unsupported NVNsh version")));
        }
        let st = u32le(d, h + 28).unwrap_or(0) as usize;
        if st >= STAGES.len() {
            return Err(red(format!("{label}: invalid NVNsh stage {st}")));
        }
        let rd = |o: usize| u32le(d, h + o).unwrap_or(0) as usize;
        let (co, ko, kn, cn) = (rd(32), rd(36), rd(40), rd(44));
        out.push(program(
            d,
            Range {
                base: h,
                size,
                co,
                ko,
                cn,
                kn,
            },
            format!("{label}#NVNSH@{h:#x}"),
            Some(STAGES[st]),
        )?);
    }
    Ok(out)
}

/// `phyre_shaders`: NVN code immediately before its control section.
pub fn phyre_shaders(d: &[u8], label: &str) -> Vec<Found> {
    let mut out = Vec::new();
    for c in find_all(d, &CTL_MAGIC) {
        if c + 0x708 > d.len() {
            continue;
        }
        let cn = u32le(d, c + 0x20).unwrap_or(0) as usize;
        let kn = u32le(d, c + 0x704).unwrap_or(0) as usize;
        let Some(k) = c.checked_sub(kn) else { continue };
        if kn < 0x80 || cn < 0x760 || c + cn > d.len() || d.get(k..k + 4) != Some(&GFX_MAGIC[..]) {
            continue;
        }
        let (ctl, code) = (d[c..c + cn].to_vec(), d[k..c].to_vec());
        if let Some(st) = sph_stage(&code)
            && check(st, &ctl, &code).is_none()
        {
            out.push(Found {
                place: format!("{label}#PHYRE@{k:#x}"),
                stage: st,
                ctl,
                code,
            });
        }
    }
    out
}
