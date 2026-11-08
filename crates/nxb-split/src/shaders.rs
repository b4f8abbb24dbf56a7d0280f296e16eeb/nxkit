//! Shaders found in game data: the control section and code area of each, its stage and where it was found, and the
//! readers of `shader_split.py` this port has so far (BNSH, agl BAHS with its program variations, SARC, the
//! `scan_blob` walk). Each function names the Python function it ports.

use std::collections::BTreeMap;

use nxb_model::sha256;

use crate::SplitError;

/// Control section magic.
pub const CTL_MAGIC: [u8; 4] = [0x34, 0x12, 0x76, 0x98];
pub(crate) const GFX_MAGIC: [u8; 4] = [0x78, 0x56, 0x34, 0x12];
pub(crate) const CS_MAGIC: [u8; 4] = [0x79, 0x56, 0x34, 0x12];
/// BNSH stage order.
pub const STAGES: [&str; 6] = ["vs", "hs", "ds", "gs", "ps", "cs"];
const NEST: usize = 16;

pub use crate::bahs::{BahsVar, bahs_shaders};

pub(crate) fn u32le(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o.checked_add(4)?)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64le(d: &[u8], o: usize) -> Option<u64> {
    let s = d.get(o..o.checked_add(8)?)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_le_bytes(a))
}

/// Whether `head` (a file's first bytes) starts a codec the split has to decode before it can look inside.
fn wrapped(head: &[u8]) -> bool {
    const ZST: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
    const LZ4F: [u8; 4] = [0x04, 0x22, 0x4d, 0x18];
    head.starts_with(&ZST)
        || head.get(4..8).is_some_and(|m| m == ZST || m == LZ4F)
        || head.starts_with(&[0x1f, 0x8b, 0x08])
        || head.starts_with(b"Yaz0")
}

/// The per-game settings the split reads from the profile (`shader_groups`, `shader_key_skip`).
#[derive(Clone, Debug, Default)]
pub struct Profile {
    /// `(path part, group)`, first match wins.
    pub groups: Vec<(String, String)>,
    /// Control bytes hashed as zeros: (offset, length).
    pub key_skip: (usize, usize),
    /// `shader_scope groups`: the title says its shaders lie in the files under `groups`, so a compressed file
    /// outside them is not decoded (see [`Profile::reads`]). Off: every file is read.
    pub groups_only: bool,
}

impl Profile {
    /// Parses a `game.tsv` (only the keys the split uses).
    pub fn from_game_tsv(text: &str) -> Profile {
        let mut p = Profile::default();
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('\t') else {
                continue;
            };
            match k {
                "shader_groups" => {
                    p.groups = v
                        .split(' ')
                        .filter_map(|g| g.split_once('='))
                        .map(|(a, b)| (a.to_string(), b.to_string()))
                        .collect()
                }
                "shader_scope" => p.groups_only = v.trim() == "groups",
                "shader_key_skip" => {
                    let num = |s: &str| {
                        let s = s.trim();
                        s.strip_prefix("0x")
                            .map_or_else(|| s.parse().ok(), |h| usize::from_str_radix(h, 16).ok())
                    };
                    if let Some((a, b)) = v.split_once('+') {
                        p.key_skip = (num(a).unwrap_or(0), num(b).unwrap_or(0));
                    }
                }
                _ => {}
            }
        }
        p
    }

    /// Whether the split reads the file at `label` (`/` + its path) whose first bytes are `head`. Always, unless
    /// the profile has `shader_scope groups`: then a file outside every group is read only when its bytes are not
    /// wrapped in a codec (zstd, a size-prefixed zstd or LZ4 frame, gzip, Yaz0), because only the decode costs much.
    pub fn reads(&self, label: &str, head: &[u8]) -> bool {
        !self.groups_only
            || self.groups.iter().any(|(k, _)| label.contains(k.as_str()))
            || !wrapped(head)
    }

    /// `group`: the source group of a label.
    pub fn group(&self, label: &str) -> String {
        if label.starts_with("main") {
            return "main".into();
        }
        self.groups
            .iter()
            .find(|(k, _)| label.contains(k.as_str()))
            .map_or_else(|| "other".into(), |(_, g)| g.clone())
    }
}

/// One shader found: `(where, stage, control, code)` of the Python.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Where (label and position).
    pub place: String,
    /// Stage (`vs`, `ps`, `cs`, ...).
    pub stage: &'static str,
    /// Control section.
    pub ctl: Vec<u8>,
    /// Code area.
    pub code: Vec<u8>,
}

pub(crate) fn nvn2_ctl(ctl: &[u8]) -> bool {
    ctl.len() >= 0x48 && ctl[..4] == CTL_MAGIC && ctl[4..8] == [0xc9, 0, 0, 0]
}

pub(crate) fn ctl_len(ctl: &[u8]) -> usize {
    if nvn2_ctl(ctl) {
        return 0x48;
    }
    let n = u32le(ctl, 0x20).unwrap_or(0) as usize;
    if (0x760..0x880).contains(&n) {
        n
    } else {
        0x880
    }
}

/// `key_ctl`.
pub fn key_ctl(p: &Profile, ctl: &[u8]) -> Vec<u8> {
    let c = &ctl[..ctl_len(ctl).min(ctl.len())];
    if nvn2_ctl(ctl) {
        let mut v = c[..0x30].to_vec();
        v.extend_from_slice(&[0; 4]);
        v.extend_from_slice(&c[0x34..]);
        return v;
    }
    let (o, n) = p.key_skip;
    if n > 0 && o + n <= c.len() {
        let mut v = c[..o].to_vec();
        v.extend(std::iter::repeat_n(0, n));
        v.extend_from_slice(&c[o + n..]);
        v
    } else {
        c.to_vec()
    }
}

/// `skey`: the first 16 hex digits of SHA-256(key control ‖ code).
pub fn skey(p: &Profile, ctl: &[u8], code: &[u8]) -> String {
    let mut v = key_ctl(p, ctl);
    v.extend_from_slice(code);
    sha256(&v).to_hex()[..16].to_string()
}

/// `same_shader`.
pub fn same_shader(p: &Profile, a: (&[u8], &[u8]), b: (&[u8], &[u8])) -> bool {
    let ((ca, ka), (cb, kb)) = (a, b);
    let mut n = if ca.len() >= 0x24 {
        u32le(ca, 0x20).unwrap_or(0) as usize
    } else {
        ca.len()
    };
    n = if 0x760 <= n && n <= ca.len().min(cb.len()) {
        n
    } else {
        ca.len().max(cb.len())
    };
    if nvn2_ctl(ca) {
        return ka == kb && key_ctl(p, ca) == key_ctl(p, cb);
    }
    let (o, ln) = p.key_skip;
    if ln > 0 && o + ln <= n {
        let cut = |c: &[u8]| -> Vec<u8> {
            let mut v = c[..o.min(c.len())].to_vec();
            v.extend_from_slice(c.get(o + ln..n.min(c.len())).unwrap_or(&[]));
            v
        };
        return ka == kb && cut(ca) == cut(cb);
    }
    ka == kb && ca[..n.min(ca.len())] == cb[..n.min(cb.len())]
}

/// `check`: the reason a control section and code area do not fit, or `None`.
pub fn check(st: &str, ctl: &[u8], code: &[u8]) -> Option<&'static str> {
    if ctl.len() < 0x24 || ctl[..4] != CTL_MAGIC || ctl.len() < ctl_len(ctl) {
        return Some("bad control section");
    }
    if nvn2_ctl(ctl) {
        return if u32le(ctl, 0x44) == Some(code.len() as u32) {
            None
        } else {
            Some("control +0x44 does not match the code area length")
        };
    }
    let head = code.get(..4);
    let bad_cs = st == "cs"
        && (head == Some(&CS_MAGIC[..]) || head == Some(&GFX_MAGIC[..]))
        && head != Some(&CS_MAGIC[..]);
    if bad_cs || (st != "cs" && head != Some(&GFX_MAGIC[..])) {
        return Some("code area header magic does not match the stage");
    }
    if u32le(ctl, 0x704) != Some(code.len() as u32) {
        return Some("control +0x704 does not match the code area length");
    }
    None
}

pub(crate) fn sph_stage(code: &[u8]) -> Option<&'static str> {
    let w = u32le(code, 0x30)?;
    if (w >> 10) & 0xf == 0 && w & 0x1f == 1 {
        return Some("vs");
    }
    match (w >> 10) & 0xf {
        1 => Some("vs"),
        2 => Some("hs"),
        3 => Some("ds"),
        4 => Some("gs"),
        5 => Some("ps"),
        _ => None,
    }
}

pub(crate) fn span<'a>(
    d: &'a [u8],
    at: usize,
    n: usize,
    place: &str,
) -> Result<&'a [u8], SplitError> {
    d.get(at..at.saturating_add(n)).ok_or_else(|| {
        SplitError::Red(format!(
            "{place}: {n:#x} bytes at {at:#x} lie outside the {:#x} bytes there are",
            d.len()
        ))
    })
}

fn field_u64(d: &[u8], at: usize, place: &str) -> Result<u64, SplitError> {
    u64le(d, at).ok_or_else(|| {
        SplitError::Red(format!(
            "{place}: 0x8 bytes at {at:#x} lie outside the {:#x} bytes there are",
            d.len()
        ))
    })
}

/// `bnsh_shaders`.
pub fn bnsh_shaders(d: &[u8], base: usize, label: &str) -> Result<Vec<Found>, SplitError> {
    let place = format!("{label} BNSH @{base:#x}");
    if d.get(base + 0x60..base + 0x64) != Some(b"grsc") {
        return Err(SplitError::Red(format!("{place} has no grsc block")));
    }
    let nvar = u32le(d, base + 0x7c).ok_or_else(|| SplitError::Red(format!("{place} grsc")))?;
    let pvar = field_u64(d, base + 0x80, &format!("{place} grsc"))? as usize;
    let mut out = Vec::new();
    for v in 0..nvar as usize {
        let prog = field_u64(
            d,
            base + pvar + 0x40 * v + 0x10,
            &format!("{place} variation {v}"),
        )? as usize;
        if prog == 0 {
            continue;
        }
        for (si, st) in STAGES.iter().enumerate() {
            let sc = field_u64(
                d,
                base + prog + 8 + 8 * si,
                &format!("{place} variation {v} program"),
            )? as usize;
            if sc == 0 {
                continue;
            }
            let w = format!("{place} variation {v} {st}");
            let pctl = field_u64(d, base + sc + 8, &w)? as usize;
            let pcode = field_u64(d, base + sc + 16, &w)? as usize;
            let ncode =
                u32le(d, base + sc + 24).ok_or_else(|| SplitError::Red(w.clone()))? as usize;
            let nctl = u32le(d, base + sc + 28).ok_or_else(|| SplitError::Red(w.clone()))? as usize;
            let ctl = span(d, base + pctl, nctl, &format!("{w} control section"))?;
            let code = span(d, base + pcode, ncode, &format!("{w} code area"))?;
            if let Some(why) = check(st, ctl, code) {
                return Err(SplitError::Red(format!(
                    "{label} BNSH @{base:#x} variation {v} {st}: {why}"
                )));
            }
            out.push(Found {
                place: format!("{label}#BNSH@{base:#x}#var{v}"),
                stage: st,
                ctl: ctl.to_vec(),
                code: code.to_vec(),
            });
        }
        out.extend(task_mesh(d, base, prog, label, v)?);
    }
    Ok(out)
}

/// NVN2's task and mesh stages, at +0x40 and +0x48 of a BNSH program. An older BNSH's program ends before them,
/// so only a pointer to a control section of that stage counts.
fn task_mesh(
    d: &[u8],
    base: usize,
    prog: usize,
    label: &str,
    v: usize,
) -> Result<Vec<Found>, SplitError> {
    let mut out = Vec::new();
    for (off, st, stage) in [(0x40, "ts", 6u8), (0x48, "ms", 7u8)] {
        let Some(sc) = u64le(d, base + prog + off).map(|x| x as usize) else {
            continue;
        };
        if sc == 0 || base + sc + 0x20 > d.len() {
            continue;
        }
        let at = |o: usize| u64le(d, base + sc + o).unwrap_or(0) as usize;
        let (pctl, pcode) = (at(8), at(16));
        let ncode = u32le(d, base + sc + 24).unwrap_or(0) as usize;
        let nctl = u32le(d, base + sc + 28).unwrap_or(0) as usize;
        let (Some(ctl), Some(code)) = (
            d.get(base + pctl..base + pctl + nctl),
            d.get(base + pcode..base + pcode + ncode),
        ) else {
            continue;
        };
        if !(0x48..0x10000).contains(&nctl) || !nvn2_ctl(ctl) || ctl.get(0x56) != Some(&stage) {
            continue;
        }
        if let Some(why) = check(st, ctl, code) {
            return Err(SplitError::Red(format!(
                "{label} BNSH @{base:#x} variation {v} {st}: {why}"
            )));
        }
        out.push(Found {
            place: format!("{label}#BNSH@{base:#x}#var{v}"),
            stage: st,
            ctl: ctl.to_vec(),
            code: code.to_vec(),
        });
    }
    Ok(out)
}

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

/// `var_pairs`: (vertex or mesh key, pixel key, from a BAHS program) within one variation. A mesh shader is the stage
/// before the pixel shader as a vertex shader is, so the translator can pad the outputs that the pixel shader reads.
pub fn var_pairs(
    p: &Profile,
    found: &[Found],
    var: &BahsVar,
) -> std::collections::BTreeSet<(String, String, bool)> {
    let mut g: BTreeMap<String, BTreeMap<&str, String>> = BTreeMap::new();
    for f in found {
        let grp = if f.place.contains("#var") {
            Some(f.place.clone())
        } else {
            var.get(&f.place).cloned()
        };
        if let Some(grp) = grp {
            g.entry(grp)
                .or_default()
                .insert(f.stage, skey(p, &f.ctl, &f.code));
        }
    }
    g.iter()
        .flat_map(|(grp, v)| {
            let ps = v.get("ps");
            ["vs", "ms"].into_iter().filter_map(move |st| {
                Some((v.get(st)?.clone(), ps?.clone(), grp.contains("#BAHS#prog")))
            })
        })
        .collect()
}
