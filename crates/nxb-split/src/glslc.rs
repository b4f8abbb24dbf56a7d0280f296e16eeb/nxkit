//! GLSLC compiler outputs (`glslc_shaders`, `glslc_program`, `glslc_section`, `glslc_layout`, `glslc_stage`,
//! `nvn2_stage` of `shader_split.py`): whole outputs found by their magic in any file no other reader takes.

use crate::SplitError;
use crate::prim::{
    CS_MAGIC, CTL_MAGIC, Found, GFX_MAGIC, check, find_all, memfind, nvn2_ctl, sph_stage, u32le,
};

const MAGIC: [u8; 4] = [0x91, 0x68, 0x86, 0x19];

/// `nvn2_stage`: compute, task and mesh by the control section's stage byte, else the SPH word at the code area's start.
pub(crate) fn nvn2_stage(ctl: &[u8], code: &[u8]) -> Option<&'static str> {
    if let Some(&b) = ctl.get(0x56) {
        match b {
            5 => return Some("cs"),
            6 => return Some("ts"),
            7 => return Some("ms"),
            _ => {}
        }
    }
    if code.len() >= 4 {
        let mut v = vec![0u8; 0x30];
        v.extend_from_slice(&code[..4]);
        sph_stage(&v)
    } else {
        None
    }
}

/// `glslc_stage`.
pub(crate) fn stage(ctl: &[u8], code: &[u8]) -> Option<&'static str> {
    if nvn2_ctl(ctl) {
        return nvn2_stage(ctl, code);
    }
    let head = code.get(..4);
    let is_cs = head == Some(&CS_MAGIC[..]);
    let known = is_cs || head == Some(&GFX_MAGIC[..]);
    let rd = |o: usize| u32le(ctl, o).unwrap_or(0);
    let headerless_cs = !known && ctl.len() >= 0x754 && (0..3).all(|i| rd(0x748 + 4 * i) != 0);
    if is_cs || headerless_cs {
        Some("cs")
    } else if head == Some(&GFX_MAGIC[..]) && code.len() >= 0x34 {
        sph_stage(code)
    } else {
        None
    }
}

/// `glslc_layout`: (the output's length, section count, first section header, distance between headers).
pub(crate) fn layout(d: &[u8], h: usize) -> (u64, u64, usize, usize) {
    let w = |o: usize| u64::from(u32le(d, h + o).unwrap_or(0));
    if h + 0x68 <= d.len() && w(0x30) == 0xc9 && w(0x44) == 0 {
        (w(0x5c), w(0x64), 0xa8, 0x98)
    } else if h + 0x50 <= d.len() {
        (w(0x44), w(0x4c), 0x90, 0x90)
    } else {
        (0, 0, 0x90, 0x90)
    }
}

type Section = (Vec<u8>, Vec<u8>);

fn cut(d: &[u8], a: usize, n: usize) -> Vec<u8> {
    let a = a.min(d.len());
    d[a..a.saturating_add(n).min(d.len())].to_vec()
}

/// `glslc_section`: (control, code) of each GPU code section by the section headers, or `None`.
fn section(d: &[u8], h: usize, size: u64) -> Option<Vec<Section>> {
    let (_, n, first, stride) = layout(d, h);
    if !(0 < n && n <= 16) || h + first + stride * n as usize > d.len() {
        return None;
    }
    let mut secs = Vec::new();
    for i in 0..n as usize {
        let s = h + first + stride * i;
        let rd = |o: usize| u64::from(u32le(d, s + o).unwrap_or(0));
        let (ssz, soff, typ) = (rd(0), rd(4), rd(8));
        if typ != 0 {
            continue;
        }
        let (co, ko, kn, cn) = (rd(0x30), rd(0x34), rd(0x38), rd(0x3c));
        if soff + ssz > size || co + cn > ssz || ko + kn > ssz {
            return None;
        }
        let base = h + soff as usize;
        secs.push((
            cut(d, base + co as usize, cn as usize),
            cut(d, base + ko as usize, kn as usize),
        ));
    }
    Some(secs)
}

/// `glslc_program`: every GPU code section of an output of several stages, or none when one fails.
fn program(d: &[u8], h: usize, size: u64, label: &str) -> Vec<Found> {
    let secs = section(d, h, size).unwrap_or_default();
    let sts: Vec<Option<&'static str>> = secs.iter().map(|(c, k)| stage(c, k)).collect();
    let mut seen = std::collections::HashSet::new();
    let dup = sts.iter().any(|s| !seen.insert(*s));
    if secs.len() < 2
        || dup
        || sts
            .iter()
            .zip(&secs)
            .any(|(s, (c, k))| s.is_none_or(|s| check(s, c, k).is_some()))
    {
        return Vec::new();
    }
    sts.into_iter()
        .zip(secs)
        .map(|(s, (ctl, code))| Found {
            place: format!("{label}#GLSLC@{h:#x}#var"),
            stage: s.unwrap_or("vs"),
            ctl,
            code,
        })
        .collect()
}

/// `glslc_shaders`: GLSLC outputs in `d`. `need`: an error when there is none. `skip`: byte ranges to leave out.
pub fn glslc_shaders(
    d: &[u8],
    label: &str,
    need: bool,
    skip: &[(usize, usize)],
) -> Result<Vec<Found>, SplitError> {
    let mut out = Vec::new();
    for h in find_all(d, &MAGIC) {
        if skip.iter().any(|&(a, b)| a <= h && h < b) || h + 0x48 > d.len() {
            continue;
        }
        let (size, _, first, _) = layout(d, h);
        let place = format!("{label}#GLSLC@{h:#x}");
        if first == 0xa8 {
            if !(0 < size && size <= (d.len() - h) as u64) {
                continue;
            }
            let secs = section(d, h, size).unwrap_or_default();
            if secs.len() > 1 {
                out.extend(program(d, h, size, label));
            } else if let Some((c, k)) = secs.first()
                && let Some(st) = stage(c, k)
                && check(st, c, k).is_none()
            {
                out.push(Found {
                    place,
                    stage: st,
                    ctl: c.clone(),
                    code: k.clone(),
                });
            }
            continue;
        }
        let end = h + size as usize;
        let c = if 0x760 < size && size <= (d.len() - h) as u64 {
            memfind(&d[h..end], &CTL_MAGIC).map(|x| x + h)
        } else {
            None
        };
        let Some(c) = c else { continue };
        if memfind(&d[(c + 4).min(end)..end], &CTL_MAGIC).is_some() {
            out.extend(program(d, h, size, label));
            continue;
        }
        let (nctl, ncode) = if c + 0x708 <= end {
            (
                u32le(d, c + 0x20).unwrap_or(0) as usize,
                u32le(d, c + 0x704).unwrap_or(0) as usize,
            )
        } else {
            (0, 0)
        };
        let (ctl, code) = (cut(d, c, nctl), cut(d, c + nctl, ncode));
        let st = if nctl >= 0x760 && nctl & 3 == 0 && c + nctl + ncode <= end {
            stage(&ctl, &code)
        } else {
            None
        };
        if let Some(st) = st
            && check(st, &ctl, &code).is_none()
        {
            out.push(Found {
                place,
                stage: st,
                ctl,
                code,
            });
            continue;
        }
        // SDK 4's output: the control section's +0x20 is 12 bytes short, the section headers say where both are
        let Some(sec) = section(d, h, size).filter(|s| s.len() == 1) else {
            continue;
        };
        let (ctl, code) = sec.into_iter().next().unwrap_or_default();
        let Some(st) = stage(&ctl, &code) else {
            continue;
        };
        if check(st, &ctl, &code).is_some() {
            continue;
        }
        out.push(Found {
            place,
            stage: st,
            ctl,
            code,
        });
    }
    if out.is_empty() && need {
        return Err(SplitError::Red(format!("{label}: no GLSLC output")));
    }
    Ok(out)
}
