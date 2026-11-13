//! NUP containers (one `.nup` file, format version 1): the base game, updates and DLC of a Switch 2 game, each an
//! `exefs/` and `romfs/` tree whose files are stored raw or as one zstd frame each, with a SHA-256 per file. The files
//! are not encrypted: no keys are needed.
//!
//! Output, as the engine's NUP packer writes it (byte-identical files):
//! - `<out>/ExeFS/<module>`: the version's code modules (names starting with `.` left out).
//! - `<out>/RomFS.img`: the version's RomFS as a `DMROMFS1` image, written from the container (never unpacked).
//! - `<out>/control.nacp`, `<out>/icon.jpg`: the package's control data when it carries it.
//! - `<aoc>/<index>.img`: each DLC's RomFS (index = DLC title ID - base title ID - 0x1000).
//!
//! [`unpack_nup`] writes the RomFS as a folder of files instead (no image), with the shader split in the same pass:
//! what a build without the engine puts into the app.
//!
//! The container is untrusted input: every offset, size and count is checked against the file before use, names
//! never become host paths unchecked, and every file is checked against its size and SHA-256 when it is read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nxb_model::sha256;

use crate::error::{ExtractError, Result};
use crate::storage::{FileStorage, Storage};

mod extract;
mod unpack;
pub use extract::{NupRequest, extract_nup, is_nup};
pub use unpack::{SplitStats, UnpackRequest, Unpacked, unpack_dlc, unpack_nup};

pub(crate) const NO_DIR: u32 = 0xFFFF_FFFF;
pub(crate) const BASE: u8 = 0;
const UPDATE: u8 = 1;
const ADDON: u8 = 2;
const KINDS: [&str; 5] = ["base", "update", "addon", "addon_update", "system"];
const STANDALONE: u8 = 1;
pub(crate) const T_NACP: u16 = 2;
pub(crate) const T_ICON: u16 = 3;
pub(crate) const T_LICONS: u16 = 10;
/// Upper bounds for the table of contents (a guard against crafted sizes).
const MAX_TOC: u64 = 512 << 20;
/// Upper bound for one file's size (read into memory when it is checked).
const MAX_FILE: u64 = 8 << 30;

pub(crate) fn bad(path: &Path, m: impl AsRef<str>) -> ExtractError {
    ExtractError::format(format!("{}: {}", path.display(), m.as_ref()))
}

fn u16le(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2)
        .map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}
pub(crate) fn u32le(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4)
        .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn u64le(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 8).map_or(0, |s| {
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        u64::from_le_bytes(a)
    })
}

/// One package record.
#[derive(Clone, Debug)]
pub struct Package {
    /// Record index.
    pub index: usize,
    /// Title ID.
    pub title_id: u64,
    /// Root, ExeFS and RomFS folders (`NO_DIR` when absent).
    pub root: u32,
    /// ExeFS folder.
    pub exefs: u32,
    /// RomFS folder.
    pub romfs: u32,
    /// Version.
    pub version: u32,
    /// Kind (0 base, 1 update, 2 add-on, ...).
    pub kind: u8,
    /// Flags (bit 0: standalone update).
    pub flags: u8,
    meta_off: u64,
    meta_size: u32,
    /// The name the engine prints (`<file> (update 131072)`).
    pub name: String,
}

/// One file record.
#[derive(Clone, Debug)]
pub(crate) struct FileRec {
    name: Vec<u8>,
    pub(crate) size: u64,
    sha: [u8; 32],
    off: u64,
    stored: u64,
    zstd: bool,
}

/// A validated container.
pub struct Nup {
    pub(crate) path: PathBuf,
    st: Arc<FileStorage>,
    toc_sha: [u8; 32],
    /// The package records.
    pub packages: Vec<Package>,
    dirs: Vec<(Vec<u8>, u32, u32)>,
    pub(crate) files: Vec<FileRec>,
    slots: Vec<u32>,
}

impl Nup {
    /// Opens and validates a container (header, table of contents and its SHA-256, every record's bounds).
    pub fn open(path: &Path) -> Result<Nup> {
        let st = Arc::new(FileStorage::open(path)?);
        let fsize_real = st.size();
        if fsize_real < 0x80 {
            return Err(bad(path, "not a NUP (too short)"));
        }
        let h = st.read_vec(0, 0x80)?;
        if &h[..3] != b"NUP" {
            return Err(bad(path, "not a NUP"));
        }
        if h[3] != 1 {
            return Err(bad(
                path,
                format!("NUP format version {}; this reads version 1", h[3]),
            ));
        }
        let (hsize, toc_off, toc_size) = (u16le(&h, 4) as u64, u64le(&h, 8), u64le(&h, 16));
        let mut toc_sha = [0u8; 32];
        toc_sha.copy_from_slice(&h[24..56]);
        let (blob, fsize, npkg, flags) = (
            u64le(&h, 56),
            u64le(&h, 64),
            u32le(&h, 72) as usize,
            u32le(&h, 76),
        );
        if hsize < 0x80 || toc_off != hsize || flags != 0 {
            return Err(bad(path, "unsupported NUP header"));
        }
        if fsize != fsize_real {
            return Err(bad(
                path,
                format!(
                    "{fsize_real} bytes, its header says {fsize} (copied or downloaded partly?)"
                ),
            ));
        }
        if toc_size > MAX_TOC
            || blob < toc_off + toc_size
            || !blob.is_multiple_of(16)
            || blob > fsize
        {
            return Err(bad(path, "TOC and data regions overlap"));
        }
        let toc = st.read_vec(toc_off, toc_size as usize)?;
        if sha256(&toc).0 != toc_sha {
            return Err(bad(
                path,
                "table of contents does not match its SHA-256 (damaged file)",
            ));
        }
        let (tpkg, ndirs, nfiles, pool_off, pool_size) = (
            u32le(&toc, 4) as usize,
            u64le(&toc, 8),
            u64le(&toc, 16),
            u64le(&toc, 24),
            u64le(&toc, 32),
        );
        let need = 0x30u64
            .checked_add((tpkg as u64).saturating_mul(0x40))
            .and_then(|x| x.checked_add(ndirs.saturating_mul(0x18)))
            .and_then(|x| x.checked_add(nfiles.saturating_mul(0x50)))
            .unwrap_or(u64::MAX);
        let tl = toc.len() as u64;
        if toc.get(..4) != Some(b"TOC\x01")
            || tpkg != npkg
            || npkg == 0
            || tl < need
            || pool_off < need
            || pool_off.checked_add(pool_size).is_none_or(|e| e > tl)
            || !(pool_off - need).is_multiple_of(4)
        {
            return Err(bad(path, "malformed table of contents"));
        }
        let pool = &toc[pool_off as usize..(pool_off + pool_size) as usize];
        let name = |o: u32, n: u32| -> Result<Vec<u8>> {
            pool.get(o as usize..(o as usize).saturating_add(n as usize))
                .map(<[u8]>::to_vec)
                .ok_or_else(|| bad(path, "name out of bounds"))
        };
        let fname = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut p = 0x30usize;
        let mut packages = Vec::with_capacity(npkg);
        for i in 0..npkg {
            let r = &toc[p..p + 0x40];
            let k = Package {
                index: i,
                title_id: u64le(r, 0),
                root: u32le(r, 8),
                exefs: u32le(r, 12),
                romfs: u32le(r, 16),
                version: u32le(r, 20),
                kind: r[24],
                flags: r[25],
                meta_off: u64le(r, 32),
                meta_size: u32le(r, 40),
                name: String::new(),
            };
            let (r1, r2) = (u16le(r, 26), u32le(r, 28));
            if k.kind as usize >= KINDS.len() {
                return Err(bad(path, format!("package {i} of unknown kind {}", k.kind)));
            }
            if r1 != 0 || r2 != 0 || k.flags & !STANDALONE != 0 {
                return Err(bad(
                    path,
                    format!("package {i} has reserved fields or flags set"),
                ));
            }
            let extra = match k.kind {
                UPDATE => format!(" {}", k.version),
                ADDON => format!(" {:016X}", k.title_id),
                _ => String::new(),
            };
            packages.push(Package {
                name: format!("{fname} ({}{extra})", KINDS[k.kind as usize]),
                ..k
            });
            p += 0x40;
        }
        let mut dirs = Vec::with_capacity(ndirs.min(1 << 24) as usize);
        for _ in 0..ndirs {
            let r = &toc[p..p + 0x18];
            dirs.push((name(u32le(r, 0), u32le(r, 4))?, u32le(r, 8), u32le(r, 12)));
            p += 0x18;
        }
        let mut files = Vec::with_capacity(nfiles.min(1 << 24) as usize);
        for _ in 0..nfiles {
            let r = &toc[p..p + 0x50];
            let n = name(u32le(r, 0), u32le(r, 4))?;
            let (size, off, stored) = (u64le(r, 8), u64le(r, 16), u64le(r, 24));
            let mut sha = [0u8; 32];
            sha.copy_from_slice(&r[32..64]);
            let (codec, cflags) = (r[64], r[65]);
            let shown = String::from_utf8_lossy(&n).into_owned();
            if codec > 1 || cflags != 0 {
                return Err(bad(
                    path,
                    format!("file {shown:?} uses codec {codec}; this reads raw and zstd"),
                ));
            }
            if size > MAX_FILE {
                return Err(bad(
                    path,
                    format!("file {shown:?} is larger than this reader allows"),
                ));
            }
            if size != 0
                && (off < blob
                    || off.checked_add(stored).is_none_or(|e| e > fsize)
                    || !off.is_multiple_of(16)
                    || stored == 0
                    || (codec == 0 && stored != size))
            {
                return Err(bad(path, format!("file {shown:?} out of bounds")));
            }
            if size == 0 && (off != 0 || stored != 0 || codec != 0) {
                return Err(bad(path, format!("empty file {shown:?} with data")));
            }
            files.push(FileRec {
                name: n,
                size,
                sha,
                off,
                stored,
                zstd: codec == 1,
            });
            p += 0x50;
        }
        let slots: Vec<u32> = (p..pool_off as usize)
            .step_by(4)
            .map(|o| u32le(&toc, o))
            .collect();
        for (n, first, count) in &dirs {
            if (*first as u64 + *count as u64) > slots.len() as u64 {
                return Err(bad(
                    path,
                    format!("folder {:?} out of bounds", String::from_utf8_lossy(n)),
                ));
            }
        }
        if slots.iter().any(|&s| s as u64 >= ndirs + nfiles) {
            return Err(bad(path, "child out of bounds"));
        }
        let nd = ndirs;
        for k in &packages {
            let dir_bad = |d: u32| d != NO_DIR && d as u64 >= nd;
            if k.root as u64 >= nd
                || dir_bad(k.exefs)
                || dir_bad(k.romfs)
                || (k.meta_size != 0
                    && k.meta_off
                        .checked_add(k.meta_size as u64)
                        .is_none_or(|e| e > fsize))
            {
                return Err(bad(path, format!("package {} out of bounds", k.index)));
            }
        }
        Ok(Nup {
            path: path.to_path_buf(),
            st,
            toc_sha,
            packages,
            dirs,
            files,
            slots,
        })
    }

    /// The children of folder `d`: (name, is folder, index), sorted by name, dump-tool artifacts left out.
    pub(crate) fn children(&self, d: u32) -> Vec<(Vec<u8>, bool, usize)> {
        let Some((_, first, count)) = self.dirs.get(d as usize) else {
            return Vec::new();
        };
        let nd = self.dirs.len();
        let mut out: Vec<(Vec<u8>, bool, usize)> = self.slots
            [*first as usize..(*first + *count) as usize]
            .iter()
            .filter_map(|&s| {
                let s = s as usize;
                if s < nd {
                    Some((self.dirs[s].0.clone(), true, s))
                } else {
                    let f = &self.files[s - nd];
                    (f.name != b".listing").then(|| (f.name.clone(), false, s - nd))
                }
            })
            .collect();
        out.sort();
        out
    }

    pub(crate) fn child(&self, d: u32, name: &[u8]) -> Option<usize> {
        self.children(d)
            .into_iter()
            .find(|(n, isdir, _)| n == name && !isdir)
            .map(|x| x.2)
    }

    /// The uncompressed size of everything under folder `d`.
    fn tree_bytes(&self, d: u32, depth: usize) -> u64 {
        if depth > 64 {
            return 0;
        }
        self.children(d)
            .iter()
            .map(|(_, isdir, i)| {
                if *isdir {
                    self.tree_bytes(*i as u32, depth + 1)
                } else {
                    self.files[*i].size
                }
            })
            .sum()
    }

    /// File `i`, decompressed and checked against its size and SHA-256.
    pub(crate) fn read(&self, i: usize) -> Result<Vec<u8>> {
        self.read_checked(i, true)
    }

    /// File `i`, decompressed and checked against its size, and with `sha` against its SHA-256.
    pub(crate) fn read_checked(&self, i: usize, sha: bool) -> Result<Vec<u8>> {
        let f = &self.files[i];
        let shown = String::from_utf8_lossy(&f.name).into_owned();
        let data = if f.size == 0 {
            Vec::new()
        } else {
            let raw = self.st.read_vec(f.off, f.stored as usize)?;
            if f.zstd {
                nxb_split::zstd::decompress_max(&raw, usize::try_from(f.size).unwrap_or(usize::MAX))
                    .map_err(|e| bad(&self.path, format!("{shown}: {e}")))?
            } else {
                raw
            }
        };
        if data.len() as u64 != f.size || (sha && sha256(&data).0 != f.sha) {
            return Err(bad(
                &self.path,
                format!("{shown} does not match its size or SHA-256 (damaged file)"),
            ));
        }
        Ok(data)
    }

    pub(crate) fn meta(&self, k: &Package) -> Result<BTreeMap<u16, Vec<u8>>> {
        let mut out = BTreeMap::new();
        if k.meta_size == 0 {
            return Ok(out);
        }
        let b = self.st.read_vec(k.meta_off, k.meta_size as usize)?;
        let mut p = 0usize;
        while p + 8 <= b.len() {
            let (t, n) = (u16le(&b, p), u32le(&b, p + 4) as usize);
            p += 8;
            let v = b.get(p..p.saturating_add(n)).ok_or_else(|| {
                bad(
                    &self.path,
                    format!(
                        "package {}: meta record {t:#x} runs past its block",
                        k.index
                    ),
                )
            })?;
            out.entry(t).or_insert_with(|| v.to_vec());
            p += n;
        }
        Ok(out)
    }
}

/// The ACI0 program ID of an NPDM, after checking its header and complete ACI0 range.
pub fn program_id(npdm: &[u8], where_: &str) -> Result<u64> {
    if npdm.len() < 0x80 || npdm.get(..4) != Some(b"META") {
        return Err(ExtractError::format(format!("{where_}: not an NPDM")));
    }
    let (off, size) = (u32le(npdm, 0x70) as usize, u32le(npdm, 0x74) as usize);
    let aci = off
        .checked_add(size)
        .and_then(|end| npdm.get(off..end))
        .ok_or_else(|| ExtractError::format(format!("{where_}: ACI0 outside file")))?;
    if aci.len() < 0x18 || aci.get(..4) != Some(b"ACI0") {
        return Err(ExtractError::format(format!("{where_}: no ACI0 section")));
    }
    Ok(u64le(aci, 0x10))
}

/// What a container holds: the title ID, the base package (none when an update holds the whole game), the updates by
/// version and the DLC by title ID.
pub struct Scan {
    /// Program ID of the game.
    pub app: u64,
    /// Base package index.
    pub base: Option<usize>,
    /// Updates: version -> package index.
    pub updates: BTreeMap<u32, usize>,
    /// DLC: title ID -> package index.
    pub dlc: BTreeMap<u64, usize>,
}

/// Complete addon trees, keyed by title and version. No delta layering is implied.
/// Reserved `addon_update` records are refused because NUP v1 defines no extraction semantics for them.
pub fn addon_records(n: &Nup) -> Result<BTreeMap<(u64, u32), usize>> {
    let mut records = BTreeMap::new();
    for package in &n.packages {
        if package.kind == 3 {
            return Err(ExtractError::Unsupported(
                "addon_update is reserved by NUP v1 and has no defined extraction semantics".into(),
            ));
        }
        if package.kind != ADDON {
            continue;
        }
        let index = package.title_id & 0xfff;
        if !(1..=2000).contains(&index) || (package.title_id & !0xfff) < 0x1000 {
            return Err(bad(
                &n.path,
                format!("invalid DLC title ID {:016X}", package.title_id),
            ));
        }
        if package.romfs == NO_DIR {
            return Err(bad(
                &n.path,
                format!("DLC {:016X} has no RomFS", package.title_id),
            ));
        }
        if records
            .insert((package.title_id, package.version), package.index)
            .is_some()
        {
            return Err(bad(
                &n.path,
                format!(
                    "DLC {:016X} version {} is duplicated",
                    package.title_id, package.version
                ),
            ));
        }
    }
    Ok(records)
}

/// Validates the application and selects the newest complete DLC tree for each title.
pub fn scan(n: &Nup) -> Result<Scan> {
    let p = &n.path;
    let bases: Vec<&Package> = n.packages.iter().filter(|k| k.kind == BASE).collect();
    if bases.len() > 1 {
        return Err(bad(p, format!("{} base packages", bases.len())));
    }
    let updates: Vec<&Package> = n.packages.iter().filter(|k| k.kind == UPDATE).collect();
    if bases.is_empty() && (updates.is_empty() || updates.iter().any(|k| k.flags & STANDALONE == 0))
    {
        return Err(bad(
            p,
            "no base game: pack the base game and its update into one NUP, or use updates marked standalone_update with complete exefs/ and romfs/",
        ));
    }
    let base = bases
        .first()
        .filter(|b| n.tree_bytes(b.root, 0) > 0)
        .map(|b| b.index);
    let mut ids = std::collections::BTreeSet::new();
    for k in n
        .packages
        .iter()
        .filter(|k| (k.kind == BASE || k.kind == UPDATE) && k.exefs != NO_DIR)
    {
        if let Some(i) = n.child(k.exefs, b"main.npdm") {
            ids.insert(program_id(
                &n.read(i)?,
                &format!("{} exefs/main.npdm", k.name),
            )?);
        }
    }
    if ids.is_empty() {
        return Err(bad(p, "no exefs/main.npdm in the base or updates"));
    }
    if ids.len() != 1 {
        let l: Vec<String> = ids.iter().map(|x| format!("{x:016X}")).collect();
        return Err(bad(
            p,
            format!(
                "base and updates have different program IDs: {}",
                l.join(", ")
            ),
        ));
    }
    let app = ids.into_iter().next().unwrap_or(0);
    if app & 0xFFF != 0 {
        return Err(bad(
            p,
            format!("base program ID {app:016X} is not a base title ID"),
        ));
    }
    let mut dlc = BTreeMap::new();
    // Ordered title/version keys make the newest complete tree win for each title.
    for ((title, _), index) in addon_records(n)? {
        let d = title.wrapping_sub(app).wrapping_sub(0x1000);
        if !(1..=2000).contains(&d) || title < app.saturating_add(0x1000) {
            return Err(bad(p, format!("{title:016X} is not a DLC of {app:016X}")));
        }
        dlc.insert(title, index);
    }
    let mut vers = BTreeMap::new();
    for k in &n.packages {
        if k.kind == UPDATE {
            if vers.contains_key(&k.version) {
                return Err(bad(p, format!("two updates of version {}", k.version)));
            }
            if k.version == 0 {
                return Err(bad(p, "an update of version 0"));
            }
            vers.insert(k.version, k.index);
        }
    }
    Ok(Scan {
        app,
        base,
        updates: vers,
        dlc,
    })
}
