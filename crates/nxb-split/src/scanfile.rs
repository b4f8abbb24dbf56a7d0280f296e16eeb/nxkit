//! One file of the RomFS as `scan_file_in` of the reference reads it, for the files no archive job takes: the format
//! is chosen by the first bytes, in the Python's order. `label` is the prefix of every place (empty for a template that
//! is relabelled at merge time), `path` the file's real path (extensions decide a few forms).

use std::borrow::Cow;

use crate::SplitError;
use crate::records::{FileResult, uniq};
use crate::shaders::{BahsVar, Found, Profile, memfind, scan_blob, var_pairs};
use crate::src::Src;
use crate::unity::ser::Trees;
use crate::zstd::Dicts;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// What the scan needs besides the file: the zstd dictionaries, the Unity common string table of the first main,
/// and (in the second pass) the type trees of every bundle.
pub struct Env<'a> {
    /// zstd dictionaries.
    pub dicts: &'a Dicts,
    /// Unity's common strings.
    pub common: Option<&'a HashMap<u32, String>>,
    /// Trees to borrow (second pass), `None` in the first.
    pub trees: Option<&'a Trees>,
}

type R<T> = Result<T, SplitError>;

static DECODED_FILES: AtomicU64 = AtomicU64::new(0);
static DECODED_BYTES: AtomicU64 = AtomicU64::new(0);

/// Files and bytes the scans decoded from a zstd or size-prefixed frame so far in this process (a counter for the
/// stage output; take two readings and subtract).
pub fn decoded_totals() -> (u64, u64) {
    (
        DECODED_FILES.load(Ordering::Relaxed),
        DECODED_BYTES.load(Ordering::Relaxed),
    )
}

fn count_decoded(bytes: usize) {
    DECODED_FILES.fetch_add(1, Ordering::Relaxed);
    DECODED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

const ZST: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const LZ4F: [u8; 4] = [0x04, 0x22, 0x4d, 0x18];
const GLSLC: [u8; 4] = [0x91, 0x68, 0x86, 0x19];

/// Whether any of `pats` occurs in `d` (one pass, candidates by first byte).
fn any_of(d: &[u8], pats: &[&[u8]]) -> bool {
    let mut first = [false; 256];
    for p in pats {
        if let Some(&b) = p.first() {
            first[b as usize] = true;
        }
    }
    d.iter()
        .enumerate()
        .any(|(i, &b)| first[b as usize] && pats.iter().any(|p| d[i..].starts_with(p)))
}

/// A result from the shaders found.
pub fn finish(p: &Profile, found: Vec<Found>, var: &BahsVar) -> R<Option<FileResult>> {
    let pairs = var_pairs(p, &found, var);
    Ok(Some(FileResult {
        keys: uniq(p, found, false)?,
        pairs,
        ..FileResult::default()
    }))
}

fn zst_err(label: &str, e: SplitError) -> SplitError {
    match e {
        SplitError::Red(m) => SplitError::Red(format!("{label}: {m}")),
        e => e,
    }
}

/// `scan_file_in` for a file without archive jobs.
pub fn scan_file(
    p: &Profile,
    src: &dyn Src,
    label: &str,
    path: &str,
    env: &Env<'_>,
    notes: &mut Vec<String>,
) -> R<Option<FileResult>> {
    crate::mgfx::set_xnb(path.ends_with(".xnb"));
    let r = scan_file_in(p, src, label, path, env, notes);
    crate::mgfx::set_xnb(false);
    r
}

fn scan_file_in(
    p: &Profile,
    src: &dyn Src,
    label: &str,
    path: &str,
    env: &Env<'_>,
    notes: &mut Vec<String>,
) -> R<Option<FileResult>> {
    let head = src.read_up(0, 16)?;
    let mut found: Vec<Found> = Vec::new();
    let mut var = BahsVar::new();
    // the readers that take the file in ranges (they read a few entries of a multi-GB file)
    if head.starts_with(b"IDRK0000") || head.starts_with(b"PDRK0000") {
        found.extend(crate::packs::idrk_shaders(src, label)?);
        return finish(p, found, &var);
    }
    if head.starts_with(b"bnd2") {
        found.extend(crate::packs::bnd2_shaders(&src.whole()?, label)?);
        return finish(p, found, &var);
    }
    if head.starts_with(b"PAK")
        && let Some(members) = crate::packs::heaps_files(src)?
    {
        for (nm, off, n) in members {
            if nm.ends_with("/shaders.cache.nx") {
                let d = src.read_up(off, n)?;
                found.extend(crate::packs::heaps_shaders(&d, &format!("{label}{nm}"))?);
            }
        }
        return finish(p, found, &var);
    }
    if head.starts_with(b"SPKG") || (head.starts_with(&[0, 0, 1, 0]) && path.ends_with(".gz")) {
        crate::packs::kt_shaders(&src.whole()?, label, &mut found, &mut var)?;
        return finish(p, found, &var);
    }
    if head.starts_with(b"SRYK") {
        for e in crate::packs::vbf_entries(src)? {
            let n = &e.0;
            if n.contains(".fx")
                || n.contains("/shaders/")
                || [".vs.bin", ".ps.bin", ".fs.bin"]
                    .iter()
                    .any(|x| n.ends_with(x))
            {
                let d = crate::packs::vbf_read(src, &e)?;
                found.extend(crate::sumo::phyre_shaders(&d, &format!("{label}/{n}")));
            }
        }
        return finish(p, found, &var);
    }
    let data = src.whole()?;
    scan_whole(p, &data, &head, label, path, env, notes)
}

/// The rest of `scan_file_in` over the file's bytes.
fn scan_whole(
    p: &Profile,
    data: &[u8],
    head: &[u8],
    label: &str,
    path: &str,
    env: &Env<'_>,
    notes: &mut Vec<String>,
) -> R<Option<FileResult>> {
    let dicts = env.dicts;
    let mut found: Vec<Found> = Vec::new();
    let mut var = BahsVar::new();
    let h4 = head.get(..4).unwrap_or(&[]);
    if h4 == b"BMSG" || crate::ue3::is_package(head) {
        // an Unreal Engine 3 shader cache or package: records by the control section's magic
        let body = if data.starts_with(&[0xc1, 0x83, 0x2a, 0x9e]) {
            crate::ue3::body(data, label)?
        } else {
            data.to_vec()
        };
        found.extend(crate::ue3::shaders(&body, label)?);
        return finish(p, found, &var);
    }
    if h4 == b"RFRM" {
        found.extend(crate::retro::rfrm_shaders(data, label)?);
        return finish(p, found, &var);
    }
    if h4 == b"BERG" {
        found.extend(crate::snowdrop::snowdrop_shaders(data, label, true)?);
        return finish(p, found, &var);
    }
    if h4 == b"ARC\0" {
        found.extend(crate::capcom::mtarc_shaders(data, label)?);
        return finish(p, found, &var);
    }
    if h4 == b"SCNE" {
        for (nm, m) in crate::packs::bea_members(data, label)? {
            scan_blob(&m, &format!("{label}/{nm}"), &mut found, &mut var, 0)?;
        }
        return finish(p, found, &var);
    }
    if head.starts_with(b"GFLXPACK") {
        for (nm, m) in crate::packs::gfpak_members(data, label)? {
            scan_blob(&m, &format!("{label}{nm}"), &mut found, &mut var, 0)?;
        }
        return finish(p, found, &var);
    }
    let mut d: Cow<'_, [u8]> = if h4 == ZST {
        let v = crate::zstd::decompress_dict(data, dicts).map_err(|e| zst_err(label, e))?;
        count_decoded(v.len());
        v.into()
    } else {
        data.into()
    };
    // `head` is the file's first bytes, or the decoded bytes' after a gzip file
    let mut head2 = head.to_vec();
    if d.get(..3) == Some(&[0x1f, 0x8b, 0x08]) {
        d = crate::misc::ungzip(&d, label)?.into();
        head2 = d.get(..16).unwrap_or(&d).to_vec();
    }
    if head2.get(4) == Some(&0x78)
        && path.ends_with(".gz")
        && let Some(z) = crate::packs::gust_gz(&d)?
    {
        d = z.into();
    }
    if d.get(..4)
        .is_some_and(|m| m == b"HCPS" || m == b"xbc1" || m == b"DRSM")
        || d.get(..8) == Some(b"cmp\0zlib")
    {
        scan_blob(&d, label, &mut found, &mut var, 0)?;
        return finish(p, found, &var);
    }
    if d.get(..4) == Some(b"RYHP") || d.get(8..12) == Some(&[0x78, 0x56, 0x34, 0x12][..]) {
        found.extend(crate::sumo::phyre_shaders(&d, label));
        return finish(p, found, &var);
    }
    let sized =
        head2.get(..4) != Some(&ZST[..]) && head2.get(4..8).is_some_and(|m| m == ZST || m == LZ4F);
    if sized {
        match crate::sized::unsized_frame(&d, dicts)? {
            crate::sized::Sized::Opened(v) => {
                count_decoded(v.len());
                d = v.into();
            }
            crate::sized::Sized::AsIs(why) => {
                if let Some(w) = why {
                    notes.push(format!("[RED] {{label}}: a size-prefixed frame that did not open ({w}): the file is read as it is"));
                }
            }
        }
    }
    if d.get(..4) == Some(b"Yaz0") {
        d = crate::yaz0::decode(&d, label)?.into();
    }
    if d.get(..3) == Some(b"XNB")
        && d.get(4) == Some(&5)
        && d.get(5).is_some_and(|f| f & 0xc0 != 0)
        && d.len() > 14
    {
        if d[5] & 0x80 != 0 {
            let hd = crate::xnb::xnb_decompress(&d, true)?;
            if memfind(&hd, b"EffectReader").is_none() {
                return Ok(None);
            }
            let want = crate::rd::u32le(&d, 10)?;
            d = if hd.len() >= want {
                hd.into()
            } else {
                crate::xnb::xnb_decompress(&d, false)?.into()
            };
        } else {
            d = crate::xnb::xnb_decompress(&d, false)?.into();
        }
    }
    if d.get(..8) == Some(b"UnityFS\0")
        || (d.get(8..11) == Some(&[0, 0, 0][..]) && crate::unity::bundle::is_serialized(&d))
    {
        let second = env.trees.is_some();
        let mut trees = env.trees.cloned().unwrap_or_default();
        return match crate::unity::prog::unity_shaders(
            &d, label, env.common, &mut trees, second, p, notes,
        ) {
            Err(e) if crate::unity::ser::is_no_tree(&e) => Ok(Some(FileResult {
                defer: true,
                ..FileResult::default()
            })),
            Err(e) => Err(e),
            Ok(u) => {
                let mut r = finish(p, u.found, &var)?.unwrap_or_default();
                r.pairs
                    .extend(u.pairs.into_iter().map(|(a, b)| (a, b, true)));
                if !second {
                    r.trees = trees;
                }
                Ok(Some(r))
            }
        };
    }
    let d = &*d;
    let head = d.get(..4).unwrap_or(&[]);
    const MARKERS: [&[u8]; 7] = [
        b"BNSH\0\0\0\0",
        b"BAHS",
        &GLSLC,
        b"MGFX",
        b"HNSHV002",
        b"Ninty_NX\0\0\0\0\0\0\0\0",
        b"NVNsh\0",
    ];
    let wanted = any_of(d, &MARKERS)
        || head == b"MSHD"
        || head == b"NDSR"
        || head == b"NDVE"
        || head == b"NARC"
        || (head == b"SARC" && memfind(d, b"Yaz0").is_some())
        || d.get(..8) == Some(b"MuxFDMR\x02");
    if !wanted {
        return Ok(None);
    }
    scan_blob(d, label, &mut found, &mut var, 0)?;
    if found.is_empty() && head != b"MSHD" && memfind(d, &GLSLC).is_some() {
        found.extend(crate::glslc::glslc_shaders(d, label, false, &[])?);
    }
    finish(p, found, &var)
}
