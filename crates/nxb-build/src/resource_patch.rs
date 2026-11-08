//! Title resource patches (`--resource-patch <option>=<value>`): edits of one member of a game archive in the app's
//! RomFS, declared by the title module ([`nxtitle::ResourcePatch`]) and applied here.
//!
//! The archive is a zstd frame holding a SARC. It is decoded (with the game's dictionary when the patch names one),
//! the member is checked against its digest and changed in place: same layout, same sizes. The SARC is written back as
//! a zstd frame of raw blocks, so no compressor is needed, and the game's dictionary decoder accepts a frame without
//! a dictionary ID. Files with other digests (another game version) are left as they are, with a warning.
//!
//! The patched package folder is `<work>/resources/<package>`: a clone of `extract/<package>` in which only the
//! patched archive is new. In a `DMROMFS1` image the new archive goes after the old file data and the index moves
//! behind it, so every other byte of the image stays as the extract stage wrote it.

use std::collections::BTreeMap;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

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
    let patches = match u64::from_str_radix(title, 16) {
        Ok(id) => nxtitles::catalog()?
            .resolve(&nxtitle::TitleFacts::new(nxtitle::TitleId(id)))
            .resource_patches(),
        Err(_) => Vec::new(),
    };
    choose(
        &patches.iter().map(|f| f.value).collect::<Vec<_>>(),
        options,
    )
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

/// The package folder the app takes its RomFS and game files from: `ex` (`extract/<package>`) when no patch applies,
/// otherwise `<work>/resources/<package>` with the patched RomFS. Warnings go to `say`, they never fail the build.
pub(crate) fn package_dir(
    a: &Args,
    b: &Build,
    say: &dyn Fn(&str),
    ex: &Path,
) -> Result<PathBuf, String> {
    let (chosen, warnings) = choices(&b.title, &a.resource_patches)?;
    for w in &warnings {
        say(w);
    }
    // `<work>/resources/<package>` beside `<work>/extract/<package>`
    let work = ex.parent().and_then(Path::parent).unwrap_or(&b.work);
    let res = work.join("resources");
    let out = res.join(&b.package);
    let unpatched = || -> Result<PathBuf, String> {
        // a folder of an earlier build with patches is not used any more
        if out.exists() {
            std::fs::remove_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        }
        Ok(ex.to_path_buf())
    };
    if chosen.is_empty() {
        return unpatched();
    }
    let (img, dir) = (ex.join("RomFS.img"), ex.join("RomFS"));
    let romfs = if img.is_file() {
        Romfs::image(&img)?
    } else if dir.is_dir() {
        Romfs::Dir(dir)
    } else {
        say(&format!(
            "resource patches: no RomFS in {}, the game keeps its stock resources",
            ex.display()
        ));
        return unpatched();
    };
    let stamp = stamp_of(&romfs, &chosen)?;
    if std::fs::read_to_string(out.join(STAMP)).is_ok_and(|s| s == stamp) {
        return Ok(out);
    }
    let (files, warnings) = edited(&|path| romfs.read(path), &chosen);
    for w in &warnings {
        say(w);
    }
    if files.is_empty() {
        return unpatched();
    }
    let tmp = res.join(format!(".{}.tmp", b.package));
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).map_err(|e| io(&tmp, e))?;
    }
    std::fs::create_dir_all(&res).map_err(|e| io(&res, e))?;
    nxb_sys::clone::clone_or_copy(ex, &tmp).map_err(|e| io(ex, e))?;
    match romfs {
        Romfs::Image { .. } => write_image(&tmp.join("RomFS.img"), &files)?,
        Romfs::Dir(_) => {
            for (path, data) in &files {
                let f = tmp.join("RomFS").join(path);
                // a new file, not a write through the clone or a hard link
                std::fs::remove_file(&f).map_err(|e| io(&f, e))?;
                std::fs::write(&f, data).map_err(|e| io(&f, e))?;
            }
        }
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
    Ok(out)
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

/// Puts new bytes for files of a `DMROMFS1` image (a clone): each new file after the file data, then the index with
/// their new places, then the header. The old bytes of those files stay unused in the image.
fn write_image(img: &Path, files: &[(String, Vec<u8>)]) -> Result<(), String> {
    let at = |e: String| format!("{}: {e}", img.display());
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(img)
        .map_err(|e| at(e.to_string()))?;
    let Romfs::Image { mut index, .. } = Romfs::image(img)? else {
        return Err(at("not an image".into()));
    };
    let mut h = [0u8; 32];
    f.read_exact_at(&mut h, 0).map_err(|e| at(e.to_string()))?;
    let mut header = Header::parse(&h).map_err(|e| at(e.0))?;
    let mut end = header.index_offset;
    for (path, data) in files {
        let r = index
            .find(path)
            .ok_or_else(|| at(format!("no file {path}")))?;
        f.write_all_at(data, end).map_err(|e| at(e.to_string()))?;
        index
            .set_file(r.entry, end, data.len() as u64)
            .map_err(|e| at(e.0))?;
        end += data.len() as u64;
    }
    f.write_all_at(index.bytes(), end)
        .map_err(|e| at(e.to_string()))?;
    f.set_len(end + index.bytes().len() as u64)
        .map_err(|e| at(e.to_string()))?;
    header.index_offset = end;
    f.write_all_at(&header.bytes(), 0)
        .map_err(|e| at(e.to_string()))?;
    f.sync_all().map_err(|e| at(e.to_string()))
}

#[cfg(test)]
mod tests;
