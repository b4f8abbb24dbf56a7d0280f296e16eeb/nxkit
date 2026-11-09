//! The walk over a file's containers (SARC, BAHS, BNSH, Yaz0) that collects its shaders, `scan_blob` of the Python.

use super::{BahsVar, Found, NEST, bahs_shaders, bnsh_shaders, u32le};
use crate::SplitError;

/// `sarc_members`: (name, bytes) of each member.
pub fn sarc_members<'a>(d: &'a [u8], label: &str) -> Result<Vec<(String, &'a [u8])>, SplitError> {
    let hdr = |m: &str| SplitError::Red(format!("{label} {m}"));
    let hl = d
        .get(4..6)
        .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
        .ok_or_else(|| hdr("SARC header"))?;
    let doff = u32le(d, 12).ok_or_else(|| hdr("SARC header"))? as usize;
    let n = d
        .get(hl + 6..hl + 8)
        .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
        .ok_or_else(|| hdr("SFAT header"))?;
    let names = hl + 12 + 16 * n + 8;
    if !(names <= doff && doff <= d.len()) {
        return Err(SplitError::Red(format!(
            "{label}: SARC data at {doff:#x}, its {n} entries and the names end at {names:#x}, the archive at {:#x}",
            d.len()
        )));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e = hl + 12 + 16 * i;
        let (attr, s, en) = (
            u32le(d, e + 4).unwrap_or(0),
            u32le(d, e + 8).unwrap_or(0) as usize,
            u32le(d, e + 12).unwrap_or(0) as usize,
        );
        if !(s <= en && en <= d.len() - doff) {
            return Err(SplitError::Red(format!(
                "{label}: SARC member {i} at {s:#x}..{en:#x} of the data lies outside its {:#x} bytes",
                d.len() - doff
            )));
        }
        let nm = if attr >> 24 != 0 {
            let o = names + (attr & 0xffffff) as usize * 4;
            let rest = d.get(o..).unwrap_or(&[]);
            let z = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            String::from_utf8_lossy(&rest[..z]).into_owned()
        } else {
            format!("#{i}")
        };
        out.push((nm, &d[doff + s..doff + en]));
    }
    Ok(out)
}

pub(crate) fn find_all<'a>(d: &'a [u8], pat: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    // non-overlapping left-to-right matches, as re.finditer
    let mut i = 0usize;
    std::iter::from_fn(move || {
        if i + pat.len() > d.len() {
            return None;
        }
        let p = memfind(&d[i..], pat)? + i;
        i = p + pat.len();
        Some(p)
    })
}

/// Position of `pat` in `d`.
pub fn memfind(d: &[u8], pat: &[u8]) -> Option<usize> {
    let first = *pat.first()?;
    let mut i = 0;
    while i + pat.len() <= d.len() {
        let p = d[i..=d.len() - pat.len()]
            .iter()
            .position(|&b| b == first)?
            + i;
        if &d[p..p + pat.len()] == pat {
            return Some(p);
        }
        i = p + 1;
    }
    None
}

/// `scan_blob` for the containers this port reads (SARC, BAHS, BNSH).`left` is the expansion budget.
pub fn scan_blob(
    d: &[u8],
    label: &str,
    out: &mut Vec<Found>,
    var: &mut BahsVar,
    depth: usize,
) -> Result<(), SplitError> {
    let mut left = None;
    scan_blob_in(d, label, out, var, depth, &mut left)
}

/// `scan_blob` with its `left` budget: what the members of the file's archives may add up to, and what their Yaz0
/// may expand to (16 times the file each).
fn scan_blob_in(
    d: &[u8],
    label: &str,
    out: &mut Vec<Found>,
    var: &mut BahsVar,
    depth: usize,
    left: &mut Option<[i64; 2]>,
) -> Result<(), SplitError> {
    if depth > NEST {
        return Err(SplitError::Red(format!(
            "{label}: archives nested more than {NEST} deep"
        )));
    }
    let head = d.get(..4).unwrap_or(&[]);
    if crate::sumo::is_mux(d) {
        let members = crate::sumo::sumo_members(d, label)?;
        let total: usize = members.iter().map(|m| m.1.len()).sum();
        // as with Yaz0, decoded bytes are what the budget counts
        if left.is_none() {
            *left = Some([(NEST * total) as i64; 2]);
        }
        for (nm, m) in members {
            scan_blob_in(&m, &format!("{label}{nm}"), out, var, depth + 1, left)?;
        }
        return Ok(());
    }
    if head == b"HCPS" || head == b"xbc1" || head == b"DRSM" || d.get(..8) == Some(b"cmp\0zlib") {
        return crate::monolith::monolith_shaders(d, label, out);
    }
    for at in find_all(d, b"SLCT\x03\x10\0\0").collect::<Vec<_>>() {
        out.extend(crate::monolith::slct_shaders(d, at, label)?);
    }
    let stored = d.len() as i64;
    let decoded;
    let d = if head == b"Yaz0" {
        if let Some(l) = left.as_mut() {
            l[1] -= d
                .get(4..8)
                .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64);
            if l[1] < 0 {
                return Err(SplitError::Red(format!(
                    "{label}: the Yaz0 archive members expand to more than {NEST} times the file"
                )));
            }
        }
        decoded = crate::yaz0::decode(d, label)?;
        &decoded[..]
    } else {
        d
    };
    let head = d.get(..4).unwrap_or(&[]);
    match left.as_mut() {
        None => *left = Some([NEST as i64 * d.len() as i64; 2]),
        Some(l) => {
            l[0] -= stored;
            if l[0] < 0 {
                return Err(SplitError::Red(format!(
                    "{label}: the archive members add up to more than {NEST} times the file (members that overlap)"
                )));
            }
        }
    }
    if head == b"SARC" {
        for (nm, m) in sarc_members(d, label)? {
            scan_blob_in(m, &format!("{label}/{nm}"), out, var, depth + 1, left)?;
        }
        return Ok(());
    }
    if head == b"NARC" {
        // Camelot's containers: each member is packed by its first byte, so its size says nothing about the budget
        for (i, m) in crate::narc::narc_members(d, label)?.into_iter().enumerate() {
            let m = crate::narc::undo(m)?;
            scan_blob_in(&m, &format!("{label}/{i}"), out, var, depth + 1, &mut None)?;
        }
        return Ok(());
    }
    if head == b"BAHS" {
        out.extend(bahs_shaders(d, label, var)?);
        return Ok(());
    }
    if head == b"NDSR" {
        out.extend(crate::engines::ndsr_shaders(d, label, var)?);
        return Ok(());
    }
    if head == b"NDVE" {
        out.extend(crate::engines::ndve_shaders(d, label, var)?);
        return Ok(());
    }
    let prim = crate::engines::sead_prim_shaders(d, label);
    if !prim.is_empty() {
        out.extend(prim);
        return Ok(());
    }
    if crate::misc::is_tfxo(d) {
        out.extend(crate::misc::tfxo_shaders(d, label)?);
        return Ok(());
    }
    if head == b"MSHD" {
        out.extend(crate::engines::mshd_shaders(d, label)?);
        return Ok(());
    }
    if head == b"DAT\0"
        && d.len() >= 0x1c
        && let Some(members) = crate::engines::pgdat_members(d)?
    {
        for (nm, m) in members {
            scan_blob_in(m, &format!("{label}/{nm}"), out, var, depth + 1, left)?;
        }
        return Ok(());
    }
    if let Some(st) = crate::engines::pg_object_stage(d) {
        out.extend(crate::engines::pg_shader(d, label, st)?);
        return Ok(());
    }
    let sized = crate::engines::pg_sized_shader(d, label)?;
    if !sized.is_empty() {
        out.extend(sized);
        return Ok(());
    }
    for at in find_all(d, b"BNSH\0\0\0\0").collect::<Vec<_>>() {
        if at + 0x20 > d.len() {
            continue;
        }
        let size = u32le(d, at + 0x1c).unwrap_or(0) as usize;
        if at + size <= d.len() && d.get(at + 0x60..at + 0x64) == Some(b"grsc") {
            out.extend(bnsh_shaders(d, at, label)?);
        }
    }
    if memfind(d, b"MGFX").is_some() {
        out.extend(crate::mgfx::mgfx_shaders(d, label)?);
    }
    out.extend(crate::misc::needle_shaders(d, label)?);
    if memfind(d, b"Ninty_NX\0\0\0\0\0\0\0\0").is_some() || memfind(d, b"NVNsh\0").is_some() {
        out.extend(crate::sumo::sumo_shaders(d, label)?);
    }
    Ok(())
}
