//! Small engine formats of `scan_blob`: `mshd_shaders`, `sead_prim_shaders`, `ndsr_shaders`, `pg_shader`,
//! `pg_sized_shader` and `pgdat_members` of `shader_split.py`.

use crate::SplitError;
use crate::bahs::BahsVar;
use crate::glslc::nvn2_stage;
use crate::prim::{
    CS_MAGIC, CTL_MAGIC, Found, GFX_MAGIC, check, memfind, nvn2_ctl, span, sph_stage, u32le,
};

fn red(m: String) -> SplitError {
    SplitError::Red(m)
}

fn cut(d: &[u8], a: usize, n: usize) -> Vec<u8> {
    let a = a.min(d.len());
    d[a..a.saturating_add(n).min(d.len())].to_vec()
}

fn nvn2_at(d: &[u8], c: usize) -> bool {
    c.checked_add(0x48).is_some_and(|e| e <= d.len()) && nvn2_ctl(&d[c..c + 0x48])
}

fn rd(d: &[u8], o: usize) -> Result<usize, SplitError> {
    u32le(d, o)
        .map(|x| x as usize)
        .ok_or_else(|| SplitError::Format("a field past the end of the file".into()))
}

/// `mshd_shaders`: Metroid Dread's MSHD files, GLSLC outputs as vertex / pixel pairs.
pub fn mshd_shaders(d: &[u8], label: &str) -> Result<Vec<Found>, SplitError> {
    let mut out: Vec<Found> = Vec::new();
    let mut c = memfind(d, &CTL_MAGIC);
    while let Some(cp) = c {
        let nctl = u32le(d, cp + 0x20).ok_or_else(|| {
            red(format!(
                "{label} MSHD control section @{cp:#x}: 0x4 bytes at {:#x} lie outside the {:#x} bytes there are",
                cp + 0x20,
                d.len()
            ))
        })? as usize;
        let from = (cp + nctl).min(d.len());
        let g = memfind(&d[from..], &GFX_MAGIC).map(|x| x + from);
        let Some(g) = g.filter(|&g| nctl >= 0x880 && g - cp - nctl <= 0x400) else {
            return Err(red(format!(
                "{label} MSHD control section @{cp:#x}: length {nctl:#x}, no code area header after it"
            )));
        };
        let ncode = rd(d, cp + 0x704)?;
        let ctl = cut(d, cp, nctl);
        let code = span(d, g, ncode, &format!("{label} MSHD @{cp:#x} code area"))?.to_vec();
        if ncode < 0x34 {
            return Err(red(format!(
                "{label} MSHD @{cp:#x}: a code area of {ncode:#x} bytes, shorter than its header"
            )));
        }
        let st = sph_stage(&code);
        let want = ["vs", "ps"][out.len() % 2];
        if st != Some(want) {
            return Err(red(format!(
                "{label} MSHD @{cp:#x}: stage {} is {}, not alternating vertex / pixel",
                out.len(),
                st.unwrap_or("None")
            )));
        }
        if let Some(why) = check(want, &ctl, &code) {
            return Err(red(format!("{label} MSHD @{cp:#x} {want}: {why}")));
        }
        out.push(Found {
            place: format!("{label}#MSHD#var{}", out.len() / 2),
            stage: want,
            ctl,
            code,
        });
        let from = (g + ncode).min(d.len());
        c = memfind(&d[from..], &CTL_MAGIC).map(|x| x + from);
    }
    if !out.len().is_multiple_of(2) {
        return Err(red(format!(
            "{label} MSHD: {} stages, not in pairs",
            out.len()
        )));
    }
    Ok(out)
}

/// `sead_prim_shaders`: sead's primitive drawer for NVN2.
pub fn sead_prim_shaders(d: &[u8], label: &str) -> Vec<Found> {
    if d.len() < 0x10 {
        return Vec::new();
    }
    let o = |i: usize| u32le(d, 4 * i).unwrap_or(0) as usize;
    let mut out = Vec::new();
    for (st, c, k) in [("vs", o(0), o(2)), ("ps", o(1), o(3))] {
        if !(0x10 <= c && nvn2_at(d, c)) {
            return Vec::new();
        }
        let own = u32le(d, c + 0x30).unwrap_or(0) as usize;
        let ctl = cut(d, c, own.max(0x48));
        let n = u32le(&ctl, 0x44).unwrap_or(0) as usize;
        let code = cut(d, k, n);
        if code.len() != n
            || nvn2_stage(&ctl, &code) != Some(st)
            || check(st, &ctl, &code).is_some()
        {
            return Vec::new();
        }
        out.push(Found {
            place: format!("{label}#SEADPRIM@{c:#x}"),
            stage: st,
            ctl,
            code,
        });
    }
    out
}

/// `ndsr_shaders`: agl's NDSR pack. Fills `var` with the vertex / pixel programs.
pub fn ndsr_shaders(d: &[u8], label: &str, var: &mut BahsVar) -> Result<Vec<Found>, SplitError> {
    if d.get(..4) != Some(b"NDSR") || d.len() < 0x20 {
        return Ok(Vec::new());
    }
    // Only complete 16-byte records count. A short pack ends in a partial one.
    let records_from = |o: usize| -> usize {
        let n = (d.len().saturating_sub(o) / 16).min(16);
        (0..n)
            .filter(|i| nvn2_at(d, u32le(d, o + 16 * i).unwrap_or(0) as usize))
            .count()
    };
    let mut o = if records_from(0x14) > records_from(0x10) {
        0x14
    } else {
        0x10
    };
    let mut out = Vec::new();
    let mut prog: Option<String> = None;
    while o + 16 <= d.len() {
        let (c, cl, k) = (rd(d, o)?, rd(d, o + 4)?, rd(d, o + 8)?);
        if c == 0 && cl == 0 && k == 0 && rd(d, o + 12)? == 0 {
            break;
        }
        // Entries are 8 bytes, a shader's control section and code area two of them, so a data entry can shift
        // the next shader by 8 (the second network of an NDVE file: u32 offset, u32 length << 4 | type).
        if !nvn2_at(d, c) {
            o += 8;
            continue;
        }
        o += 16;
        let ctl = cut(d, c, (cl >> 4).max(0x48));
        let code = cut(d, k, u32le(&ctl, 0x44).unwrap_or(0) as usize);
        let Some(st) = nvn2_stage(&ctl, &code) else {
            continue;
        };
        if check(st, &ctl, &code).is_some() {
            continue;
        }
        let place = format!("{label}#NDSR@{c:#x}");
        if st == "vs" {
            prog = Some(place.clone());
        } else if st == "ps"
            && let Some(p) = prog.take()
        {
            let v = format!(
                "{label}#NDSR#{}",
                p.rsplit_once('@').map_or(p.as_str(), |x| x.1)
            );
            var.insert(p, v.clone());
            var.insert(place.clone(), v);
        }
        out.push(Found {
            place,
            stage: st,
            ctl,
            code,
        });
    }
    Ok(out)
}

/// `ndve_shaders`: nn::ndve's movie enhancement file, the compute networks that upscale a game's movies. `NDVE`,
/// +0xc the length of its NDSR pack, +0x14 the count of 8-byte model records, then the NDSR pack (offsets from its own
/// start), then the networks. Each network control section names its own length (+0x30) and its code area's
/// (+0x44), and the code area follows it.
pub fn ndve_shaders(d: &[u8], label: &str, var: &mut BahsVar) -> Result<Vec<Found>, SplitError> {
    if d.len() < 0x18 {
        return Ok(Vec::new());
    }
    let pack = 0x18 + 8 * rd(d, 0x14)?;
    let nets = pack + rd(d, 0xc)?;
    let mut out = ndsr_shaders(d.get(pack..nets.min(d.len())).unwrap_or(&[]), label, var)?;
    let mut c = nets;
    while let Some(i) = d.get(c..).and_then(|s| memfind(s, &CTL_MAGIC)) {
        c += i;
        if c & 3 == 0 && nvn2_at(d, c) {
            let (own, n) = (rd(d, c + 0x30)?, rd(d, c + 0x44)?);
            let ctl = cut(d, c, own);
            let code = cut(d, c + own, n);
            if let Some(st) = nvn2_stage(&ctl, &code)
                && code.len() == n
                && check(st, &ctl, &code).is_none()
            {
                out.push(Found {
                    place: format!("{label}#NDVE@{c:#x}"),
                    stage: st,
                    ctl,
                    code,
                });
            }
        }
        c += 4;
    }
    Ok(out)
}

/// Whether `d` is a PlatinumGames shader object (`pg_shader`): its magic names the stage.
pub fn pg_object_stage(d: &[u8]) -> Option<&'static str> {
    let st = match d.get(..4)? {
        b"VSO\0" => "vs",
        b"HSO\0" => "hs",
        b"DSO\0" => "ds",
        b"GSO\0" => "gs",
        b"PSO\0" => "ps",
        b"CSO\0" => "cs",
        _ => return None,
    };
    let o = u32le(d, 0xc)? as usize;
    (d.len() >= 0x18 && d.get(o..o + 4) == Some(&CTL_MAGIC[..])).then_some(st)
}

/// `pg_shader`.
pub fn pg_shader(d: &[u8], label: &str, st: &'static str) -> Result<Vec<Found>, SplitError> {
    let (nctl, octl, ncode, ocode) = (rd(d, 8)?, rd(d, 0xc)?, rd(d, 0x10)?, rd(d, 0x14)?);
    if octl + nctl > d.len() || ocode + ncode > d.len() {
        return Err(red(format!(
            "{label}: shader object sections out of bounds"
        )));
    }
    let (ctl, code) = (cut(d, octl, nctl), cut(d, ocode, ncode));
    let mut why = check(st, &ctl, &code).map(str::to_string);
    if why.is_none() && st != "cs" && sph_stage(&code) != Some(st) {
        why = Some(format!(
            "the SPH stage {} does not match the object's magic",
            sph_stage(&code).unwrap_or("None")
        ));
    }
    if let Some(w) = why {
        return Err(red(format!("{label} ({st}): {w}")));
    }
    Ok(vec![Found {
        place: format!("{label}#PGSO"),
        stage: st,
        ctl,
        code,
    }])
}

/// `pg_sized_shader`: PlatinumGames' binding-table form.
pub fn pg_sized_shader(d: &[u8], label: &str) -> Result<Vec<Found>, SplitError> {
    if d.len() < 12 {
        return Ok(Vec::new());
    }
    let prefix = rd(d, 0)?;
    if prefix < 4 || prefix & 3 != 0 || prefix + 8 > d.len() {
        return Ok(Vec::new());
    }
    let ncode = rd(d, prefix)?;
    let (codeoff, ctlsize) = (prefix + 4, prefix + 4 + ncode);
    let head = d.get(codeoff..codeoff + 4);
    if ncode < 0x80
        || ctlsize + 4 > d.len()
        || (head != Some(&GFX_MAGIC[..]) && head != Some(&CS_MAGIC[..]))
    {
        return Ok(Vec::new());
    }
    let nctl = rd(d, ctlsize)?;
    let ctloff = ctlsize + 4;
    if nctl < 0x760 || ctloff + nctl != d.len() || d.get(ctloff..ctloff + 4) != Some(&CTL_MAGIC[..])
    {
        return Ok(Vec::new());
    }
    let (ctl, code) = (d[ctloff..].to_vec(), d[codeoff..ctlsize].to_vec());
    let st = if code.starts_with(&CS_MAGIC) {
        Some("cs")
    } else {
        sph_stage(&code)
    };
    let Some(st) = st else {
        return Err(red(format!(
            "{label}: binding-table shader has an unknown SPH stage"
        )));
    };
    if let Some(w) = check(st, &ctl, &code) {
        return Err(red(format!("{label} ({st}): {w}")));
    }
    Ok(vec![Found {
        place: format!("{label}#PGSO"),
        stage: st,
        ctl,
        code,
    }])
}

/// The (name, data) members of an archive.
pub type Members<'a> = Vec<(String, &'a [u8])>;

/// `pgdat_members`: a PlatinumGames DAT archive's members, or `None` when its tables do not fit.
pub fn pgdat_members(d: &[u8]) -> Result<Option<Members<'_>>, SplitError> {
    let (n, ot, nt, st) = (rd(d, 4)?, rd(d, 8)?, rd(d, 0x10)?, rd(d, 0x14)?);
    if n == 0 {
        return Ok(Some(Vec::new()));
    }
    if (ot + 4 * n).max(st + 4 * n).max(nt + 4) > d.len() {
        return Ok(None);
    }
    let w = rd(d, nt)?;
    if nt + 4 + w * n > d.len() {
        return Ok(None);
    }
    let mut out = Vec::new();
    for i in 0..n {
        let (o, s) = (rd(d, ot + 4 * i)?, rd(d, st + 4 * i)?);
        if o + s > d.len() {
            return Ok(None);
        }
        let raw = &d[nt + 4 + w * i..nt + 4 + w * (i + 1)];
        let name = raw.split(|&b| b == 0).next().unwrap_or(&[]);
        out.push((String::from_utf8_lossy(name).into_owned(), &d[o..o + s]));
    }
    Ok(Some(out))
}
