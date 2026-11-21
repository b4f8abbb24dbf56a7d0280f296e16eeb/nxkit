//! Title resource patches (`--resource-patch <option>=<value>`): edits of one member of a game archive in the app's
//! RomFS, declared by the title module ([`nxtitle::ResourcePatch`]) and applied here.
//!
//! The archive is a zstd frame holding a SARC. It is decoded (with the game's dictionary when the patch names one),
//! the member is checked against its digest and changed in place: same layout, same sizes. The SARC is written back as
//! a zstd frame of raw blocks, so no compressor is needed, and the game's dictionary decoder accepts a frame without
//! a dictionary ID. Files with other digests (another game version) are left as they are, with a warning.
//!
//! The standalone build does not clone the extract folder: [`overlay`] writes only the patched files into a small
//! folder, and the romfs stage takes a file from there when the overlay has one.
//!
//! The patched package folder is `<work>/resources/<package>`: a clone of `extract/<package>` in which only the
//! patched archive is new. In a `DMROMFS1` image the new archive goes after the old file data and the index moves
//! behind it, so every other byte of the image stays as the extract stage wrote it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nxb_extract::fileext::FileExt;
use nxb_model::{Digest, FileDigest};
use nxb_romfs::image::{Header, Index};
use nxtitle::{Edit, ResourcePatch};

use crate::Args;
use crate::graph::Build;

/// A patch with the value it applies.
type Chosen = (ResourcePatch, f32);
/// Reads a file of the RomFS by its path: `None` when there is none.
type Read<'a> = dyn Fn(&str) -> Result<Option<Vec<u8>>, String> + 'a;

/// The value that leaves a patch off.
const STOCK: &str = "stock";
/// The stamp in a patched folder: what it was made from.
const STAMP: &str = ".resource-patches";

/// The patches of a title with their values: the build options first, then each patch's default. Options the title
/// has no patch for come back as warnings. A value the patch does not take is an error.
fn choices(
    title: &str,
    options: &[(String, String)],
) -> Result<(Vec<Chosen>, Vec<String>), String> {
    choose(&patches_for(title)?, options)
}

fn patches_for(title: &str) -> Result<Vec<ResourcePatch>, String> {
    let patches = match u64::from_str_radix(title, 16) {
        Ok(id) => nxtitles::catalog()?
            .resolve(&nxtitle::TitleFacts::new(nxtitle::TitleId(id)))
            .resource_patches(),
        Err(_) => Vec::new(),
    };
    Ok(patches.iter().map(|f| f.value).collect())
}

/// Generic builds apply only named patches, and reject options this title does not provide.
fn choose_explicit(
    patches: &[ResourcePatch],
    options: &[(String, String)],
) -> Result<Vec<Chosen>, String> {
    for (name, _) in options {
        if !patches.iter().any(|p| p.option == *name) {
            return Err(format!(
                "--resource-patch {name}: this title has no such patch"
            ));
        }
    }
    let requested: Vec<_> = patches
        .iter()
        .filter(|p| options.iter().any(|(name, _)| name == p.option))
        .copied()
        .collect();
    choose(&requested, options).map(|(chosen, _)| chosen)
}

fn choose(
    patches: &[ResourcePatch],
    options: &[(String, String)],
) -> Result<(Vec<Chosen>, Vec<String>), String> {
    let mut warnings = Vec::new();
    for (name, _) in options {
        if !patches.iter().any(|p| p.option == *name) {
            let have: Vec<&str> = patches.iter().map(|p| p.option).collect();
            warnings.push(format!(
                "--resource-patch {name}: this title has no such patch (it has: {}), ignored",
                if have.is_empty() {
                    "none".into()
                } else {
                    have.join(", ")
                }
            ));
        }
    }
    let mut out = Vec::new();
    for p in patches {
        // the last option of a name wins
        let given = options.iter().rev().find(|(n, _)| n == p.option);
        let value = match given {
            Some((_, v)) if v == STOCK => None,
            Some((_, v)) => {
                let x: f32 = v
                    .parse()
                    .map_err(|_| format!("--resource-patch {}={v}: not a number", p.option))?;
                Some(x)
            }
            None => p.default,
        };
        let Some(x) = value else { continue };
        if !p.edit.accepts(x) {
            let Edit::F32Words { above, up_to, .. } = p.edit;
            return Err(format!(
                "--resource-patch {}={x}: the value must be greater than {above} and at most {up_to}",
                p.option
            ));
        }
        out.push((p, x));
    }
    Ok((out.into_iter().map(|(p, x)| (*p, x)).collect(), warnings))
}

/// The folder of patched RomFS files for the standalone build, or `None` when no patch applies. `ex` is the extract
/// folder (`extract/<package>`); the RomFS is its `RomFS.img`, or the loose folder `loose` (a NUP's). The folder is
/// `<work>/resources/<name of ex>.overlay` and holds only the new files, at their RomFS paths: nothing in `extract/`
/// is written, and the caller takes each file from the overlay instead of the RomFS. Warnings go to `say`.
pub(crate) fn overlay(
    a: &Args,
    b: &Build,
    say: &dyn Fn(&str),
    ex: &Path,
    loose: &Path,
) -> Result<Option<PathBuf>, String> {
    let (chosen, warnings) = choices(&b.title, &a.resource_patches)?;
    for w in &warnings {
        say(w);
    }
    write_overlay(b, say, ex, loose, &chosen)
}

/// Like [`overlay`], but applies only explicitly requested patches and rejects unknown options.
pub(crate) fn overlay_explicit(
    a: &Args,
    b: &Build,
    say: &dyn Fn(&str),
    ex: &Path,
    loose: &Path,
) -> Result<Option<PathBuf>, String> {
    if a.resource_patches.is_empty() {
        return Ok(None);
    }
    let chosen = choose_explicit(&patches_for(&b.title)?, &a.resource_patches)?;
    write_overlay(b, say, ex, loose, &chosen)
}

fn write_overlay(
    b: &Build,
    say: &dyn Fn(&str),
    ex: &Path,
    loose: &Path,
    chosen: &[Chosen],
) -> Result<Option<PathBuf>, String> {
    let work = ex.parent().and_then(Path::parent).unwrap_or(&b.work);
    let res = work.join("resources");
    let name = ex
        .file_name()
        .map_or_else(|| b.package.clone(), |n| n.to_string_lossy().into_owned());
    let out = res.join(format!("{name}.overlay"));
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    // an overlay of an earlier build is not used when this one has nothing to patch
    let none = || -> Result<Option<PathBuf>, String> {
        if out.exists() {
            std::fs::remove_dir_all(&out).map_err(|e| io(&out, e))?;
        }
        Ok(None)
    };
    if chosen.is_empty() {
        return none();
    }
    let img = ex.join("RomFS.img");
    let romfs = if img.is_file() {
        Romfs::image(&img)?
    } else if loose.is_dir() {
        Romfs::Dir(loose.to_path_buf())
    } else {
        say(&format!(
            "resource patches: no RomFS in {}, the game keeps its stock resources",
            ex.display()
        ));
        return none();
    };
    let stamp = stamp_of(&romfs, chosen)?;
    if std::fs::read_to_string(out.join(STAMP)).is_ok_and(|s| s == stamp) {
        return Ok(Some(out));
    }
    let (files, warnings) = edited(&|path| romfs.read(path), chosen);
    for w in &warnings {
        say(w);
    }
    if files.is_empty() {
        return none();
    }
    let tmp = res.join(format!(".{name}.overlay.tmp"));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).map_err(|e| io(&tmp, e))?;
    }
    for (path, data) in &files {
        let f = tmp.join(path);
        if let Some(d) = f.parent() {
            std::fs::create_dir_all(d).map_err(|e| io(d, e))?;
        }
        std::fs::write(&f, data).map_err(|e| io(&f, e))?;
    }
    std::fs::write(tmp.join(STAMP), &stamp).map_err(|e| io(&tmp, e))?;
    if out.exists() {
        std::fs::remove_dir_all(&out).map_err(|e| io(&out, e))?;
    }
    std::fs::rename(&tmp, &out).map_err(|e| io(&out, e))?;
    let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
    say(&format!(
        "resource patches: {} rewritten in {}",
        names.join(", "),
        out.display()
    ));
    Ok(Some(out))
}

/// Where the files of the RomFS are read from.
enum Romfs {
    /// A `DMROMFS1` image.
    Image { file: std::fs::File, index: Index },
    /// A loose folder.
    Dir(PathBuf),
}

impl Romfs {
    fn image(p: &Path) -> Result<Romfs, String> {
        let at = |e: String| format!("{}: {e}", p.display());
        let file = std::fs::File::open(p).map_err(|e| at(e.to_string()))?;
        let mut h = [0u8; 32];
        file.read_exact_at(&mut h, 0)
            .map_err(|e| at(e.to_string()))?;
        let h = Header::parse(&h).map_err(|e| at(e.0))?;
        let mut idx = vec![0u8; usize::try_from(h.index_len).map_err(|e| at(e.to_string()))?];
        file.read_exact_at(&mut idx, h.index_offset)
            .map_err(|e| at(e.to_string()))?;
        let index = Index::parse(&h, idx).map_err(|e| at(e.0))?;
        Ok(Romfs::Image { file, index })
    }

    /// The bytes of the file at `path` (from the RomFS root), `None` when there is none.
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        match self {
            Romfs::Image { file, index } => {
                let Some(f) = index.find(path) else {
                    return Ok(None);
                };
                let mut d = vec![0u8; usize::try_from(f.size).map_err(|e| e.to_string())?];
                file.read_exact_at(&mut d, f.offset)
                    .map_err(|e| format!("RomFS image, {path}: {e}"))?;
                Ok(Some(d))
            }
            Romfs::Dir(root) => {
                let p = root.join(path);
                if !p.is_file() {
                    return Ok(None);
                }
                std::fs::read(&p)
                    .map(Some)
                    .map_err(|e| format!("{}: {e}", p.display()))
            }
        }
    }
}

/// What a patched folder is made from: the RomFS (size and time), each patch's option, value and archive digest.
fn stamp_of(romfs: &Romfs, chosen: &[(ResourcePatch, f32)]) -> Result<String, String> {
    let meta = match romfs {
        Romfs::Image { file, .. } => file.metadata(),
        Romfs::Dir(d) => std::fs::metadata(d),
    }
    .map_err(|e| e.to_string())?;
    let time = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let mut s = format!("romfs {} {time}\n", meta.len());
    for (p, x) in chosen {
        s.push_str(&format!(
            "{} {} {}\n",
            p.option,
            x.to_bits(),
            p.archive_sha256.to_hex()
        ));
        // a folder's own time does not change when a file in it does: the archive's size and time do
        if let Romfs::Dir(d) = romfs
            && let Ok(m) = std::fs::metadata(d.join(p.archive))
        {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            s.push_str(&format!("{} {} {t}\n", p.archive, m.len()));
        }
    }
    Ok(s)
}

fn sha(d: &[u8]) -> Digest {
    FileDigest::of(d).sha256
}

/// The new archive files (path, bytes) of the chosen patches, and warnings for each patch that was not applied.
fn edited(
    read: &Read<'_>,
    chosen: &[(ResourcePatch, f32)],
) -> (Vec<(String, Vec<u8>)>, Vec<String>) {
    // the decoded SARCs, by archive: two patches of one archive edit the same bytes
    let mut sarcs: BTreeMap<&str, (Digest, Vec<u8>, bool)> = BTreeMap::new();
    let mut warnings = Vec::new();
    for (p, x) in chosen {
        let skip = |why: String| {
            format!(
                "resource patch {}: {why}; not applied, the game keeps its stock resources",
                p.option
            )
        };
        if let Some((digest, ..)) = sarcs.get(p.archive) {
            if *digest != p.archive_sha256 {
                warnings.push(skip(format!(
                    "{} is not the archive it was recorded on",
                    p.archive
                )));
                continue;
            }
        } else {
            match decode(read, p) {
                Ok(d) => {
                    sarcs.insert(p.archive, (p.archive_sha256, d, false));
                }
                Err(why) => {
                    warnings.push(skip(why));
                    continue;
                }
            }
        }
        let Some((_, sarc, changed)) = sarcs.get_mut(p.archive) else {
            continue;
        };
        match edit_member(sarc, p, *x) {
            Ok(()) => *changed = true,
            Err(why) => warnings.push(skip(why)),
        }
    }
    let files = sarcs
        .into_iter()
        .filter(|(_, (.., changed))| *changed)
        .map(|(path, (_, sarc, _))| (path.to_string(), nxb_split::zstd::raw_frame(&sarc)))
        .collect();
    (files, warnings)
}

/// The SARC of a patch's archive, decoded, after checking the archive and dictionary digests.
fn decode(read: &Read<'_>, p: &ResourcePatch) -> Result<Vec<u8>, String> {
    let file = |path: &str| read(path)?.ok_or_else(|| format!("the RomFS has no {path}"));
    let raw = file(p.archive)?;
    let got = sha(&raw);
    if got != p.archive_sha256 {
        return Err(format!(
            "{} has SHA-256 {}, the patch was recorded on {} (another game version?)",
            p.archive,
            got.to_hex(),
            p.archive_sha256.to_hex()
        ));
    }
    let mut dicts = nxb_split::zstd::Dicts::new();
    if let Some(d) = &p.dictionary {
        let pack = nxb_split::zstd::decompress(&file(d.archive)?)
            .map_err(|e| format!("{}: {e}", d.archive))?;
        let r =
            nxb_romfs::sarc::member(&pack, d.member).map_err(|e| format!("{}: {e}", d.archive))?;
        let dict = &pack[r];
        if sha(dict) != d.sha256 {
            return Err(format!(
                "the dictionary {} in {} is not the one recorded",
                d.member, d.archive
            ));
        }
        dicts
            .add(dict)
            .map_err(|e| format!("{} in {}: {e}", d.member, d.archive))?;
    }
    nxb_split::zstd::decompress_dict(&raw, &dicts).map_err(|e| format!("{}: {e}", p.archive))
}

/// Applies the edit to the member of the decoded SARC, after checking the member's digest. All checks come before
/// the first write.
fn edit_member(sarc: &mut [u8], p: &ResourcePatch, x: f32) -> Result<(), String> {
    let r = nxb_romfs::sarc::member(sarc, p.member).map_err(|e| format!("{}: {e}", p.archive))?;
    let member = &mut sarc[r];
    if sha(member) != p.member_sha256 {
        return Err(format!("{} is not the member it was recorded on", p.member));
    }
    let Edit::F32Words { offsets, .. } = p.edit;
    let ok = !offsets.is_empty()
        && offsets.windows(2).all(|w| w[0] < w[1])
        && offsets
            .iter()
            .all(|&o| o % 4 == 0 && o.checked_add(4).is_some_and(|e| e <= member.len()));
    if !ok {
        return Err(format!("bad word offsets for {}", p.member));
    }
    for &o in offsets {
        member[o..o + 4].copy_from_slice(&x.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
