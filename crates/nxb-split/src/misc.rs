//! Needle, TFXO, gzip files and the extra scans of an NSO image (`needle_shaders`, `tfxo_shaders`, `tfxo_streams`,
//! `ungzip` and the steps of `main()` for `main` in `shader_split.py`).

use crate::SplitError;
use crate::glslc::{glslc_shaders, stage};
use crate::prim::{
    CS_MAGIC, CTL_MAGIC, Found, GFX_MAGIC, check, find_all, memfind, span, sph_stage, u32le,
};

const TFXO: &[u8] = b".tfxo file\0";

fn red(m: String) -> SplitError {
    SplitError::Red(m)
}

fn cut(d: &[u8], a: usize, n: usize) -> Vec<u8> {
    let a = a.min(d.len());
    d[a..a.saturating_add(n).min(d.len())].to_vec()
}

/// `needle_shaders`: Needle shader containers (`HNSHV002`).
pub fn needle_shaders(d: &[u8], label: &str) -> Result<Vec<Found>, SplitError> {
    let mut out = Vec::new();
    for h in find_all(d, b"HNSHV002") {
        if h + 12 > d.len() {
            continue;
        }
        let size = u32::from_be_bytes([d[h + 8], d[h + 9], d[h + 10], d[h + 11]]) as usize;
        if size < 0x90 + 0x760 || size > d.len() - h {
            continue;
        }
        let end = h + size;
        let mut c = memfind(&d[h + 12..end], &CTL_MAGIC).map(|x| x + h + 12);
        while let Some(cp) = c {
            let s = cp as i64 - 0x90;
            if s < (h + 12) as i64 || cp + 0x760 > end {
                return Err(red(format!(
                    "{label} NEEDLE @{h:#x}: incomplete NVN section at {cp:#x}"
                )));
            }
            let s = s as usize;
            let rd = |o: usize| u32le(d, s + o).unwrap_or(0) as usize;
            let (co, ko, kn, cn) = (rd(0x30), rd(0x34), rd(0x38), rd(0x3c));
            if co != 0x90 || cn < 0x760 || cn != u32le(d, cp + 0x20).unwrap_or(0) as usize {
                return Err(red(format!(
                    "{label} NEEDLE @{h:#x}: invalid control offset or control length at {s:#x}"
                )));
            }
            if ko < co + cn || kn == 0 || (co + cn).max(ko + kn) > end - s {
                return Err(red(format!(
                    "{label} NEEDLE @{h:#x}: NVN section at {s:#x} is outside its HNSHV002 container"
                )));
            }
            let (ctl, code) = (cut(d, cp, cn), cut(d, s + ko, kn));
            let st = stage(&ctl, &code);
            let why = match st {
                Some(st) => check(st, &ctl, &code).map(str::to_string),
                None => Some("unknown shader stage".into()),
            };
            if let Some(w) = why {
                return Err(red(format!(
                    "{label} NEEDLE @{h:#x}: NVN section at {s:#x}: {w}"
                )));
            }
            out.push(Found {
                place: format!("{label}#NEEDLE@{s:#x}"),
                stage: st.unwrap_or("vs"),
                ctl,
                code,
            });
            let from = (s + ko + kn).min(end);
            c = memfind(&d[from..end], &CTL_MAGIC).map(|x| x + from);
        }
    }
    Ok(out)
}

/// Whether `d` starts a YEBIS effect file.
pub fn is_tfxo(d: &[u8]) -> bool {
    d.starts_with(TFXO)
}

/// `tfxo_shaders`: control sections each followed by its code area.
pub fn tfxo_shaders(d: &[u8], label: &str) -> Result<Vec<Found>, SplitError> {
    let mut out = Vec::new();
    for a in find_all(d, &CTL_MAGIC) {
        if a + 0x24 > d.len() {
            continue;
        }
        let n = u32le(d, a + 0x20).unwrap_or(0) as usize;
        if !(0x760..=0x880).contains(&n) {
            continue;
        }
        let ctl = span(d, a, n, &format!("{label}: the control section at {a:#x}"))?.to_vec();
        let ncode = u32le(&ctl, 0x704).unwrap_or(0) as usize;
        let code = cut(d, a + n, ncode);
        let head = code.get(..4);
        let st = if head == Some(&CS_MAGIC[..]) {
            Some("cs")
        } else if head == Some(&GFX_MAGIC[..]) {
            sph_stage(&code)
        } else {
            None
        };
        let Some(st) = st else {
            return Err(red(format!(
                "{label}: the control section at {a:#x} is not followed by a code area"
            )));
        };
        if let Some(w) = check(st, &ctl, &code) {
            return Err(red(format!("{label}: shader at {a:#x}: {w}")));
        }
        out.push(Found {
            place: format!("{label}#TFXO@{a:#x}"),
            stage: st,
            ctl,
            code,
        });
    }
    Ok(out)
}

/// `tfxo_streams`: effect files linked into a module as zlib streams.
pub fn tfxo_streams(d: &[u8], label: &str) -> Result<Vec<Found>, SplitError> {
    let mut out = Vec::new();
    for p in 0..d.len().saturating_sub(1) {
        if d[p] != 0x78 || !matches!(d[p + 1], 0x01 | 0x5e | 0x9c | 0xda) {
            continue;
        }
        let head = crate::inflate::inflate_head(&d[p..(p + 4096).min(d.len())], TFXO.len(), true);
        if head.as_deref() != Some(TFXO) {
            continue;
        }
        let x = crate::zlibx::zlib_stream(&d[p..], 1 << 31)
            .ok_or_else(|| SplitError::Format(format!("{label}@{p:#x}: damaged zlib stream")))?;
        out.extend(tfxo_shaders(&x, &format!("{label}@{p:#x}"))?);
    }
    Ok(out)
}

/// `ungzip`: a gzip file with every member checked, at most `max(1 GiB, 16 times the file)` bytes.
pub fn ungzip(d: &[u8], label: &str) -> Result<Vec<u8>, SplitError> {
    let limit = (1usize << 30).max(16 * d.len());
    match crate::zlibx::gunzip_all(d, limit + 1) {
        Ok(v) if v.len() > limit => Err(red(format!(
            "{label}: gzip stream expands beyond its {limit:#x} byte budget"
        ))),
        Ok(v) => Ok(v),
        Err(e) => Err(red(format!("{label}: invalid gzip stream ({e})"))),
    }
}

/// The steps of `main()` for an NSO image `img` beyond `scan_blob`: GLSLC outputs, YEBIS streams, Retro's startup
/// archive and `raw_shaders`. `found` is what `scan_blob` took.
pub fn main_extras(img: &[u8], label: &str, found: &[Found]) -> Result<Vec<Found>, SplitError> {
    let mut out = glslc_shaders(img, label, false, &[])?;
    out.extend(tfxo_streams(img, label)?);
    out.extend(crate::retro::retro_main_forms(img, label)?);
    let known: std::collections::HashSet<&[u8]> = found
        .iter()
        .chain(out.iter())
        .map(|f| f.ctl.as_slice())
        .collect();
    let raw = crate::raw::raw_shaders(img, label, &known)?;
    out.extend(raw);
    Ok(out)
}
