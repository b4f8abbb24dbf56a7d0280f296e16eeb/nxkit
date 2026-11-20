//! MonoGame effects (`mgfx_effect`, `mgfx_shaders`): versions 5 and 8 of the MGFX container, read to the end.

use crate::SplitError;
use crate::glslc::glslc_shaders;
use crate::prim::{CTL_MAGIC, Found, check, find_all, sph_stage, u32le};

const GLSLC: [u8; 4] = [0x91, 0x68, 0x86, 0x19];
const GFX: [u8; 4] = [0x78, 0x56, 0x34, 0x12];

thread_local! {
    static XNB: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks the scan of this thread as one of a `.xnb` file (a template is scanned without its path in the label).
pub(crate) fn set_xnb(on: bool) {
    XNB.with(|c| c.set(on));
}

fn is_xnb(label: &str) -> bool {
    label.ends_with(".xnb") || XNB.with(std::cell::Cell::get)
}

/// The Python's IndexError or struct.error: a read past the end of the data.
struct Short;
type R<T> = Result<T, Short>;

/// `_Rd`: a little-endian reader with .NET's strings (`raw` never fails, it clamps like a slice).
struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn u8(&mut self) -> R<u8> {
        self.p = self.p.saturating_add(1);
        self.d.get(self.p.wrapping_sub(1)).copied().ok_or(Short)
    }
    fn u32(&mut self) -> R<u32> {
        self.p = self.p.saturating_add(4);
        u32le(self.d, self.p.wrapping_sub(4)).ok_or(Short)
    }
    fn raw(&mut self, n: usize) -> &'a [u8] {
        let a = self.p;
        self.p = self.p.saturating_add(n);
        let b = self.p;
        self.d
            .get(a.min(self.d.len())..b.min(self.d.len()))
            .unwrap_or(&[])
    }
    fn str(&mut self) -> R<()> {
        let (mut n, mut sh) = (0usize, 0u32);
        loop {
            let b = self.u8()?;
            n |= usize::from(b & 0x7f).checked_shl(sh).unwrap_or(0);
            sh += 7;
            if b < 0x80 {
                self.raw(n);
                return Ok(());
            }
        }
    }
}

fn idx(_: Short) -> SplitError {
    SplitError::Red("IndexError: the effect ends inside a field".into())
}

/// The pass over the parameters: `{u8 class, u8 type, name, semantic, annotations, rows, columns, elements, members, value}`.
fn params(r: &mut Rd<'_>, red: &dyn Fn(&str) -> SplitError) -> Result<u8, SplitError> {
    let n = r.u8().map_err(idx)?;
    for _ in 0..n {
        let typ = *r.raw(2).get(1).ok_or(()).map_err(|_| idx(Short))?;
        r.str().map_err(idx)?;
        r.str().map_err(idx)?;
        if r.u8().map_err(idx)? != 0 {
            return Err(red("annotations (MonoGame writes none)"));
        }
        let (rows, cols) = (r.u8().map_err(idx)?, r.u8().map_err(idx)?);
        let a = u32::from(params(r, red)?);
        let b = u32::from(params(r, red)?);
        if a + b != 0 {
            continue;
        }
        match typ {
            1..=3 => {
                r.raw(4 * usize::from(rows) * usize::from(cols));
            }
            4 => r.str().map_err(idx)?,
            _ => {}
        }
    }
    Ok(n)
}

/// `mgfx_effect` at `d[p..]`: `Ok(None)` when the header is not an effect of NVN shaders.
pub fn mgfx_effect(d: &[u8], p: usize, label: &str) -> Result<Option<Vec<Found>>, SplitError> {
    let v5 = d.get(p + 4) == Some(&5);
    if d.get(p + 4) != Some(&8) && !v5 {
        return Ok(None);
    }
    let mut r = Rd {
        d,
        p: p + if v5 { 6 } else { 10 },
    };
    let head = (|| -> R<bool> {
        for _ in 0..r.u8()? {
            if !v5 {
                r.str()?;
            }
            r.raw(2);
            let n = usize::from(r.u8()?);
            r.raw(3 * n);
        }
        let at = r.p;
        Ok(if v5 {
            d.get(at + 6..at + 10) != Some(&GLSLC[..])
        } else {
            d.get(at + 10..at + 14) != Some(&CTL_MAGIC[..])
        })
    })();
    match head {
        Err(Short) | Ok(true) => return Ok(None),
        Ok(false) => {}
    }
    let xnb = is_xnb(label);
    let red = |m: &str| SplitError::Red(format!("{label} MGFX @{p:#x}: {m}"));
    let mut out: Vec<Found> = Vec::new();
    let count = r.u8().map_err(idx)?;
    for i in 0..count {
        let vs = r.u8().map_err(idx)?;
        let n = r.u32().map_err(idx)? as usize;
        if r.p + n > d.len() {
            if xnb {
                return Err(SplitError::Red(format!(
                    "{label} MGFX @{p:#x}: shader {i} claims {n:#x} bytes past the end of the file"
                )));
            }
            return Ok(None);
        }
        let bc = r.raw(n);
        let sred = |m: &str| SplitError::Red(format!("{label} MGFX @{p:#x} shader {i}: {m}"));
        let (st, ctl, code): (Option<&'static str>, Vec<u8>, Vec<u8>);
        if v5 {
            let mut g = glslc_shaders(bc, &format!("{label} MGFX @{p:#x} shader {i}"), false, &[])?;
            if vs > 1
                || g.len() != 1
                || bc.get(..4) != Some(&GLSLC[..])
                || u32le(bc, 0x44) != Some(n as u32)
            {
                return Err(sred(&format!(
                    "{n:#x} bytes, not one GLSLC output of one stage"
                )));
            }
            let f = g.remove(0);
            (st, ctl, code) = (Some(f.stage), f.ctl, f.code);
        } else {
            let nctl = u32le(bc, 0).ok_or_else(|| idx(Short))? as usize;
            let ncode = if 8 + nctl <= n {
                u32le(bc, 4 + nctl).ok_or_else(|| idx(Short))? as i64
            } else {
                -1
            };
            if vs > 1 || (8 + nctl) as i64 + ncode != n as i64 {
                return Err(sred(&format!(
                    "{n:#x} bytes, not a control section and a code area"
                )));
            }
            ctl = bc[4..4 + nctl].to_vec();
            code = bc[8 + nctl..].to_vec();
            st = if code.get(..4) == Some(&GFX[..]) {
                sph_stage(&code)
            } else {
                None
            };
        }
        let want = if vs == 1 { "vs" } else { "ps" };
        let why = if st == Some(want) {
            check(want, &ctl, &code).map(str::to_string)
        } else {
            Some(format!(
                "the SPH stage is {}, the effect says {}",
                st.map_or("None".to_string(), |s| format!("'{s}'")),
                if vs == 1 { "vertex" } else { "pixel" }
            ))
        };
        if let Some(w) = why {
            return Err(sred(&w));
        }
        out.push(Found {
            place: format!("{label}#MGFX@{p:#x}#{i}"),
            stage: want,
            ctl,
            code,
        });
        for _ in 0..r.u8().map_err(idx)? {
            r.raw(3);
            if r.u8().map_err(idx)? != 0 {
                if v5 {
                    return Err(sred("a sampler state (none in the version 5 effects seen)"));
                }
                r.raw(20);
            }
            if !v5 {
                r.str().map_err(idx)?;
            }
            r.u8().map_err(idx)?;
        }
        let k = usize::from(r.u8().map_err(idx)?);
        r.raw(k);
        if !v5 {
            for _ in 0..r.u8().map_err(idx)? {
                r.str().map_err(idx)?;
                r.raw(4);
            }
        }
    }
    params(&mut r, &red)?;
    let ann = |r: &mut Rd<'_>| -> Result<(), SplitError> {
        if r.u8().map_err(idx)? != 0 {
            return Err(red("annotations (MonoGame writes none)"));
        }
        Ok(())
    };
    for _ in 0..r.u8().map_err(idx)? {
        r.str().map_err(idx)?;
        ann(&mut r)?;
        for _ in 0..r.u8().map_err(idx)? {
            r.str().map_err(idx)?;
            ann(&mut r)?;
            let (v, f) = (r.u8().map_err(idx)?, r.u8().map_err(idx)?);
            if (v != 255 && usize::from(v) >= out.len())
                || (f != 255 && usize::from(f) >= out.len())
            {
                return Err(red(&format!(
                    "a pass names shader {v} / {f} of {}",
                    out.len()
                )));
            }
            for n in [18, 25, 12] {
                if r.u8().map_err(idx)? != 0 {
                    r.raw(n);
                }
            }
        }
    }
    if r.p > d.len() || (xnb && d.get(r.p..).is_some_and(|t| t.iter().any(|&b| b != 0))) {
        return Err(red(&format!(
            "the effect ends at {:#x}, the file at {:#x}",
            r.p,
            d.len()
        )));
    }
    Ok(Some(out))
}

/// `mgfx_shaders`: every effect in `d`.
pub fn mgfx_shaders(d: &[u8], label: &str) -> Result<Vec<Found>, SplitError> {
    let mut out = Vec::new();
    for at in find_all(d, b"MGFX") {
        out.extend(mgfx_effect(d, at, label)?.unwrap_or_default());
    }
    if is_xnb(label)
        && crate::prim::memfind(&d[..d.len().min(0x200)], b"EffectReader").is_some()
        && out.is_empty()
    {
        return Err(SplitError::Red(format!(
            "{label}: an XNB effect without an MGFX version 5 or 8 effect of NVN shaders"
        )));
    }
    Ok(out)
}
