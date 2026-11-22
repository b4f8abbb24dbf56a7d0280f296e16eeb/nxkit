//! Frostbite's game data (`frostbite.py`, `frostbite_jobs` and `frostbite_shaders`, for EA SPORTS FC 27): the shaders
//! are the CompiledBytecode resources of the bundles the superbundle tables (`.toc`) list, stored in the cas files,
//! Oodle Leviathan blocks decoded with the game's own decoder ([`crate::oodle`]).
//!
//! `Data/layout.toc` names the install chunks (a DbObject), each superbundle `.toc` under `Data/NX` and `Patch/NX`
//! lists its bundles: entry 0 the manifest in a cas file, then its items (ebx, res, chunks).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::{Ctx, File, Job, JobOut, Plan, R, ranges};
use crate::bahs::BahsVar;
use crate::rd::red;
use crate::shaders::{Found, check};
use crate::src::Src;

/// The magic of every `.toc`.
pub const TOC_MAGIC: [u8; 4] = [0x00, 0xd1, 0xce, 0x01];
/// Where a `.toc`'s body starts.
const TOC_BODY: usize = 0x22c;
/// A bundle manifest's magic.
const BUNDLE_MAGIC: u32 = 0x9D79_8ED6;

/// `name_hash`: Frostbite's hash of a type name, djb2 with xor over the lower-case name.
pub fn name_hash(name: &str) -> u32 {
    name.to_lowercase()
        .bytes()
        .fold(5381u32, |h, c| h.wrapping_mul(33) ^ u32::from(c))
}

fn bad<T>(m: impl Into<String>) -> R<T> {
    red(m)
}

fn be32(d: &[u8], o: usize) -> R<u32> {
    d.get(o..o + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_be_bytes)
        .map_or_else(|| bad(format!("a read at {o:#x} runs past the end")), Ok)
}

fn le32(d: &[u8], o: usize) -> R<u32> {
    d.get(o..o + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .map_or_else(|| bad(format!("a read at {o:#x} runs past the end")), Ok)
}

fn le16(d: &[u8], o: usize) -> R<usize> {
    d.get(o..o + 2)
        .and_then(|b| b.try_into().ok())
        .map(|b| usize::from(u16::from_le_bytes(b)))
        .map_or_else(|| bad(format!("a read at {o:#x} runs past the end")), Ok)
}

fn be16(d: &[u8], o: usize) -> R<u32> {
    d.get(o..o + 2)
        .and_then(|b| b.try_into().ok())
        .map(|b| u32::from(u16::from_be_bytes(b)))
        .map_or_else(|| bad(format!("a read at {o:#x} runs past the end")), Ok)
}

/// One DbObject value.
#[derive(Clone, Debug, PartialEq)]
pub enum Db {
    /// A list.
    List(Vec<Db>),
    /// An object, its fields in order.
    Obj(Vec<(String, Db)>),
    /// An integer (bool, int32, int64).
    Int(i64),
    /// A float or double.
    Float(f64),
    /// A string.
    Str(String),
    /// A GUID, SHA-1 or blob.
    Bytes(Vec<u8>),
}

impl Db {
    fn get(&self, k: &str) -> Option<&Db> {
        match self {
            Db::Obj(v) => v.iter().find(|(n, _)| n == k).map(|(_, x)| x),
            _ => None,
        }
    }
}

fn leb(d: &[u8], mut p: usize) -> R<(usize, usize)> {
    let (mut v, mut s) = (0usize, 0u32);
    loop {
        let Some(&b) = d.get(p) else {
            return bad("DbObject: a length runs past the end");
        };
        p += 1;
        if s < 63 {
            v |= usize::from(b & 0x7f) << s;
        }
        s += 7;
        if b & 0x80 == 0 {
            return Ok((v, p));
        }
    }
}

/// `dbobject`: one value at `p` → ((name, value), next offset).
pub fn dbobject(d: &[u8], p: usize) -> R<((Option<String>, Db), usize)> {
    let Some(&t) = d.get(p) else {
        return bad("DbObject: runs past the end");
    };
    let mut p = p + 1;
    let mut name = None;
    if t & 0x80 == 0 {
        let Some(e) = d[p.min(d.len())..].iter().position(|&c| c == 0) else {
            return bad("DbObject: a name runs past the end");
        };
        name = Some(String::from_utf8_lossy(&d[p..p + e]).into_owned());
        p += e + 1;
    }
    let k = t & 0x1f;
    let take = |p: usize, n: usize| -> R<&[u8]> {
        d.get(p..p + n)
            .map_or_else(|| bad("DbObject: a value runs past the end"), Ok)
    };
    let v = match k {
        1 | 2 => {
            let (n, q) = leb(d, p)?;
            let end = q + n;
            let mut items = Vec::new();
            let mut p = q;
            while p < end && d.get(p).is_some_and(|&c| c != 0) {
                let (it, np) = dbobject(d, p)?;
                items.push(it);
                p = np;
            }
            let v = if k == 2 {
                Db::Obj(
                    items
                        .into_iter()
                        .map(|(n, v)| (n.unwrap_or_default(), v))
                        .collect(),
                )
            } else {
                Db::List(items.into_iter().map(|(_, v)| v).collect())
            };
            return Ok(((name, v), end));
        }
        7 => {
            let (n, q) = leb(d, p)?;
            let s = take(q, n.saturating_sub(1))?;
            return Ok((
                (name, Db::Str(String::from_utf8_lossy(s).into_owned())),
                q + n,
            ));
        }
        19 => {
            let (n, q) = leb(d, p)?;
            return Ok(((name, Db::Bytes(take(q, n)?.to_vec())), q + n));
        }
        6 => (Db::Int(i64::from(take(p, 1)?[0] != 0)), 1),
        8 => (Db::Int(i64::from(le32(d, p)? as i32)), 4),
        9 => {
            let b: [u8; 8] = take(p, 8)?.try_into().unwrap_or_default();
            (Db::Int(i64::from_le_bytes(b)), 8)
        }
        0xb => (Db::Float(f64::from(f32::from_bits(le32(d, p)?))), 4),
        0xc => {
            let b: [u8; 8] = take(p, 8)?.try_into().unwrap_or_default();
            (Db::Float(f64::from_le_bytes(b)), 8)
        }
        0xf => (Db::Bytes(take(p, 16)?.to_vec()), 16),
        0x10 => (Db::Bytes(take(p, 20)?.to_vec()), 20),
        _ => {
            return bad(format!(
                "DbObject: type {t:#x} at {:#x} is not known",
                p - 1
            ));
        }
    };
    Ok(((name, v.0), p + v.1))
}

/// `install_chunks`: persistentIndex → the install chunk's folder under `NX/`.
pub fn install_chunks(layout: &[u8]) -> R<HashMap<u32, String>> {
    let ((_, lay), _) = dbobject(layout, TOC_BODY)?;
    let Some(chunks) = lay
        .get("installManifest")
        .and_then(|m| m.get("installChunks"))
    else {
        return bad("Data/layout.toc: no installManifest");
    };
    let Db::List(v) = chunks else {
        return bad("Data/layout.toc: installChunks is not a list");
    };
    let mut out = HashMap::new();
    for c in v {
        match (c.get("persistentIndex"), c.get("name")) {
            (Some(Db::Int(i)), Some(Db::Str(n))) => {
                out.insert(*i as u32, n.clone());
            }
            _ => return bad("Data/layout.toc: an install chunk without persistentIndex or name"),
        }
    }
    Ok(out)
}

/// A cas id: 8 bytes.
type CasId = [u8; 8];

/// One entry of a bundle: (cas id, offset, stored size).
type Ent = (CasId, u32, u32);

/// `toc_bundles`: the entries of each bundle of a superbundle `.toc` (entry 0 the manifest).
pub fn toc_bundles(d: &[u8], what: &str) -> R<Vec<Vec<Ent>>> {
    if d.get(..4) != Some(&TOC_MAGIC[..]) {
        return bad(format!("{what}: not a Frostbite toc"));
    }
    if d.len() < TOC_BODY + 0x20 {
        return Ok(Vec::new());
    }
    let b = TOC_BODY;
    let (bo, nb) = (be32(d, b + 4)? as usize, be32(d, b + 8)? as usize);
    let mut out = Vec::new();
    for i in 0..nb {
        let at = b + bo + 16 * i;
        let sz = be32(d, at + 4)? as usize;
        let off = d
            .get(at + 8..at + 16)
            .and_then(|x| x.try_into().ok())
            .map(u64::from_be_bytes)
            .map_or_else(|| bad(format!("{what}: bundle {i} runs past the end")), Ok)?
            as usize;
        let s = b + off;
        let end = s + (sz & 0xff_ffff);
        if end > d.len() {
            return bad(format!("{what}: bundle {i}'s data runs past the end"));
        }
        let (fo, n, p) = (
            be32(d, s + 8)? as usize + s,
            be32(d, s + 12)? as usize,
            be32(d, s + 16)? as usize + s,
        );
        if fo + n > end {
            return bad(format!("{what}: bundle {i}'s flags run past its data"));
        }
        let (mut p, mut cas, mut ents) = (p, None::<CasId>, Vec::new());
        for k in 0..n {
            if d[fo + k] & 0x80 != 0 || cas.is_none() {
                cas = d.get(p..p + 8).and_then(|x| x.try_into().ok());
                if cas.is_none() {
                    return bad(format!("{what}: bundle {i} runs past the end"));
                }
                p += 8;
            }
            let c = cas.unwrap_or_default();
            ents.push((c, be32(d, p)?, be32(d, p + 4)?));
            p += 8;
        }
        if p != fo {
            return bad(format!(
                "{what}: bundle {i}'s entries end at {:#x}, its flags start at {:#x}",
                p - s,
                fo - s
            ));
        }
        out.push(ents);
    }
    Ok(out)
}

/// `cas_path`: the `rel` (under the root) of the cas file a cas id names.
fn cas_path(cid: &CasId, chunks: &HashMap<u32, String>) -> R<String> {
    let pi = u32::from_be_bytes([cid[2], cid[3], cid[4], cid[5]]);
    let n = u16::from_be_bytes([cid[6], cid[7]]);
    let Some(c) = chunks.get(&pi) else {
        return bad(format!(
            "cas id {}: no install chunk has persistentIndex {pi:#x}",
            hex(cid)
        ));
    };
    Ok(format!(
        "{}/NX/{c}/cas_{n:02}.cas",
        if cid[1] != 0 { "Patch" } else { "Data" }
    ))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A bundle manifest: (its length, ebx count, res (name, size, type)).
type Manifest = (usize, usize, Vec<(String, u32, u32)>, usize);

/// `manifest`: the bundle manifest at the start of `d` (read from its cas file).
fn manifest(src: &dyn Src, off: u64) -> R<Manifest> {
    let hl = be32(&src.read_up(off, 4)?, 0)? as usize;
    let d = src.read_up(off, hl + 4)?;
    if d.len() < hl + 4 {
        return bad(format!(
            "bundle manifest at {off:#x} runs past its cas file"
        ));
    }
    let (magic, tot, ne, nr, nc, so) = (
        le32(&d, 4)?,
        le32(&d, 8)? as usize,
        le32(&d, 12)? as usize,
        le32(&d, 16)? as usize,
        le32(&d, 20)? as usize,
        le32(&d, 24)? as usize,
    );
    if magic != BUNDLE_MAGIC {
        return bad(format!("no bundle manifest at {off:#x} (magic {magic:#x})"));
    }
    if ne + nr + nc != tot {
        return bad(format!(
            "bundle manifest at {off:#x}: {ne} + {nr} + {nc} items, not the {tot} it counts"
        ));
    }
    let h = 4;
    let mut p = h + 0x20 + 20 * tot + 8 * ne;
    let res: Vec<(u32, u32)> = (0..nr)
        .map(|i| Ok((le32(&d, p + 8 * i)?, le32(&d, p + 8 * i + 4)?)))
        .collect::<R<_>>()?;
    p += 8 * nr;
    let types: Vec<u32> = (0..nr).map(|i| le32(&d, p + 4 * i)).collect::<R<_>>()?;
    let name = |o: u32| -> R<String> {
        let a = h + so + o as usize;
        let e = d
            .get(a..h + hl)
            .and_then(|s| s.iter().position(|&c| c == 0));
        match e {
            Some(e) => Ok(String::from_utf8_lossy(&d[a..a + e]).into_owned()),
            None => bad(format!(
                "bundle manifest at {off:#x}: a name at {:#x} runs past the manifest",
                a
            )),
        }
    };
    let res = res
        .iter()
        .zip(types)
        .map(|(&(a, s), t)| Ok((name(a)?, s, t)))
        .collect::<R<_>>()?;
    Ok((hl + 4, ne, res, nc))
}

/// `read`: an item's data, its blocks at `off` (`stored` bytes in all) decoded.
pub fn read_item(d: &[u8], label: &str, size: usize) -> R<Vec<u8>> {
    let (mut o, end, mut out) = (0usize, d.len(), Vec::with_capacity(size));
    while o < end {
        if o + 8 > end {
            return bad(format!(
                "{label}: a block header at {o:#x} runs past the item"
            ));
        }
        let n = (be32(d, o)? & 0xff_ffff) as usize;
        let (codec, cs) = (d[o + 4], d[o + 5]);
        let c = ((u32::from(cs) & 0xf) << 16 | be16(d, o + 6)?) as usize;
        if cs & 0xf0 != 0x70 || o + 8 + c > end {
            return bad(format!(
                "{label}: a damaged block at {o:#x} ({})",
                hex(&d[o..o + 8])
            ));
        }
        let blk = &d[o + 8..o + 8 + c];
        match codec {
            0 => out.extend_from_slice(blk),
            0x19 => out.extend(crate::oodle::decode(
                blk,
                n,
                &format!("{label}: block at {o:#x}"),
            )?),
            _ => {
                return bad(format!(
                    "{label}: block at {o:#x}: codec {codec:#x} is not known"
                ));
            }
        }
        o += 8 + c;
    }
    if out.len() != size {
        return bad(format!(
            "{label}: {:#x} bytes decoded, not {size:#x}",
            out.len()
        ));
    }
    Ok(out)
}

/// `nvn_shader`: (code area, control section) of a CompiledBytecode resource, `None` without a VNVN record.
pub fn nvn_shader(b: &[u8]) -> R<Option<(&[u8], &[u8])>> {
    if b.len() < 0x24 || &b[0x20..0x24] != b"VNVN" {
        return Ok(None);
    }
    let r = 0x20;
    let v = le16(b, r + 8)?;
    let mut p = r + if v >= 3 { 0x11 } else { 0x10 };
    for _ in 0..4 {
        p += 2 + 4 * le16(b, p)?;
    }
    p += 2 + 8 * le16(b, p)?;
    let (co, cl, ko, kl) = (
        le32(b, p)? as usize,
        le32(b, p + 4)? as usize,
        le32(b, p + 8)? as usize,
        le32(b, p + 12)? as usize,
    );
    if r + co + cl > b.len() || r + ko + kl > b.len() {
        return bad(format!(
            "VNVN record: code {co:#x}+{cl:#x} or control {ko:#x}+{kl:#x} outside its {:#x} bytes",
            b.len() - r
        ));
    }
    Ok(Some((&b[r + co..r + co + cl], &b[r + ko..r + ko + kl])))
}

/// A resource of a job: (offset, stored size, size, name).
type Res = (u64, u64, u64, String);

/// `frostbite_shaders`: a range of one cas file's CompiledBytecode resources.
fn shaders(s: &dyn Src, rel: &str, ents: &[Res], notes: &mut Vec<String>) -> R<JobOut> {
    let mut out = Vec::new();
    for (off, stored, size, name) in ents {
        let label = format!("/{rel}#{name}");
        let d = s.read_up(*off, *stored as usize)?;
        let b = read_item(&d, &label, *size as usize)?;
        let Some((code, ctl)) =
            nvn_shader(&b).map_err(|e| crate::SplitError::Red(format!("{label}: {e}")))?
        else {
            notes.push(format!(
                "[RED] {label}: a CompiledBytecode resource without a VNVN record; left out"
            ));
            continue;
        };
        let st = crate::glslc::stage(ctl, code);
        let why = match st {
            Some(st) => check(st, ctl, code),
            None => Some("no stage in the code area header"),
        };
        if let Some(w) = why {
            notes.push(format!("[RED] {label}: {w}; left out"));
            continue;
        }
        out.push(Found {
            place: label,
            stage: st.unwrap_or("cs"),
            ctl: ctl.to_vec(),
            code: code.to_vec(),
        });
    }
    Ok(JobOut {
        found: out,
        var: BahsVar::new(),
    })
}

/// Resources: (cas rel under the root, offset, stored) → (size, name).
type Resources = BTreeMap<(String, u64, u64), (u64, String)>;

/// `resources` of one root: every res of type `rtype` → (cas rel under the root, offset, stored) → (size, name).
fn resources(
    files: &[File],
    by_rel: &HashMap<&str, usize>,
    root: &str,
    rtype: u32,
) -> R<Resources> {
    let file = |rel: &str| -> R<&File> {
        by_rel
            .get(format!("{root}{rel}").as_str())
            .map(|&i| &files[i])
            .map_or_else(|| bad(format!("/{root}{rel}: not in the game's files")), Ok)
    };
    let chunks = install_chunks(&file("Data/layout.toc")?.src.whole()?)?;
    let mut tocs: Vec<&str> = files
        .iter()
        .map(|f| f.rel.as_str())
        .filter_map(|r| r.strip_prefix(root))
        .filter(|r| {
            (r.starts_with("Data/NX/") || r.starts_with("Patch/NX/")) && r.ends_with(".toc")
        })
        .collect();
    tocs.sort_by_key(|r| (!r.starts_with("Data/"), r.to_string()));
    let mut out = BTreeMap::new();
    for t in tocs {
        let what = format!("/{root}{t}");
        for ents in toc_bundles(&file(t)?.src.whole()?, &what)? {
            let Some(&(cid, off, size)) = ents.first() else {
                continue;
            };
            let cas = file(&cas_path(&cid, &chunks)?)?;
            let (mlen, ne, res, nc) = manifest(cas.src.as_ref(), u64::from(off))?;
            if mlen != size as usize {
                return bad(format!(
                    "{what}: a bundle manifest of {mlen:#x} bytes, the toc says {size:#x}"
                ));
            }
            if ne + res.len() + nc != ents.len() - 1 {
                return bad(format!(
                    "{what}: a bundle of {} items with {} entries",
                    ne + res.len() + nc,
                    ents.len() - 1
                ));
            }
            for ((nm, sz, ty), (c, o, s)) in res.into_iter().zip(&ents[1 + ne..]) {
                if ty == rtype {
                    out.insert(
                        (cas_path(c, &chunks)?, u64::from(*o), u64::from(*s)),
                        (u64::from(sz), nm),
                    );
                }
            }
        }
    }
    Ok(out)
}

/// `frostbite_jobs`: the CompiledBytecode resources in about 64 equal stored parts. Every `.toc` and `.cas` file of
/// a Frostbite root is taken.
pub fn plan(files: &[File], p: &mut Plan) -> R<()> {
    let by_rel: HashMap<&str, usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.rel.as_str(), i))
        .collect();
    let mut roots: Vec<&str> = files
        .iter()
        .filter_map(|f| f.rel.strip_suffix("Data/layout.toc"))
        .filter(|r| r.is_empty() || r.ends_with('/'))
        .collect();
    roots.sort_unstable();
    roots.dedup();
    let rtype = name_hash("CompiledBytecode");
    for root in roots {
        let lay = &files[by_rel[format!("{root}Data/layout.toc").as_str()]];
        if lay.head(4)? != TOC_MAGIC {
            continue;
        }
        let res = resources(files, &by_rel, root, rtype)?;
        for (i, f) in files.iter().enumerate() {
            if let Some(r) = f.rel.strip_prefix(root)
                && (r.ends_with(".toc") || r.ends_with(".cas"))
            {
                p.taken.insert(i);
            }
        }
        let step = res.keys().map(|k| k.2).sum::<u64>() as f64 / 64.0;
        let mut by_cas: BTreeMap<&str, Vec<Res>> = BTreeMap::new();
        for ((cas, off, stored), (size, name)) in &res {
            by_cas
                .entry(cas.as_str())
                .or_default()
                .push((*off, *stored, *size, name.clone()));
        }
        for (cas, ents) in by_cas {
            let rel = format!("{root}{cas}");
            let Some(&ci) = by_rel.get(rel.as_str()) else {
                return bad(format!("/{rel}: not in the game's files"));
            };
            for chunk in ranges(&ents, |e| e.1, step) {
                let src: Arc<dyn Src> = files[ci].src.clone();
                let r2 = rel.clone();
                p.jobs.push(Job {
                    rel: rel.clone(),
                    run: Box::new(move |_: &Ctx<'_>, notes| {
                        shaders(src.as_ref(), &r2, &chunk, notes)
                    }),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
