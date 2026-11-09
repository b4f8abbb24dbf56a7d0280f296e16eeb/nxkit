//! The extract stage's reuse check and the RomFS folders: unpack an image once, link a tree into the app.

use super::{Say, identity, io, romfs, shaders};
use crate::Args;
use crate::events::Update;
use crate::graph::Build;
use crate::inputs::Source;
use std::path::{Path, PathBuf};

/// The extract and the split, reused while the packages are as they were. Packages give `RomFS.img`, a NUP gives the
/// RomFS as a folder (`romfs/`) and no image.
pub(super) fn extract(
    a: &Args,
    b: &Build,
    p: &Say,
    version: u32,
    marker: &str,
) -> Result<String, String> {
    let dir = b.work.join("extract").join(format!("v{version}"));
    let nup = b.src.source == Source::Nup;
    let key_file = b.work.join("extract.key");
    let key = std::iter::once(identity(&b.base))
        .chain(b.updates.iter().map(|u| identity(&u.1)))
        .collect::<Vec<_>>()
        .join(";");
    let romfs = if nup {
        dir.join("romfs")
            .join(marker.trim_start_matches('/'))
            .is_file()
    } else {
        dir.join("RomFS.img").is_file()
    };
    let have = romfs
        && dir.join("ExeFS/main.npdm").is_file()
        && std::fs::read_dir(b.work.join("shader/pkg")).is_ok_and(|mut d| d.next().is_some());
    let reused = have && std::fs::read_to_string(&key_file).is_ok_and(|k| k == key);
    let mut text = if reused {
        "reused".to_string()
    } else {
        let _ = std::fs::remove_file(&key_file);
        let (text, unsupported) = if nup {
            crate::extract_stage::nup_loose(a, b, p, version)?
        } else {
            let (images, why) = crate::extract_stage::packages(a, b, p)?;
            (format!("{images} images"), why)
        };
        if let Some(why) = unsupported {
            return Err(crate::exit::fail(
                4,
                format!("unsupported: the shader split needs the game's own decoder: {why}"),
            ));
        }
        std::fs::write(&key_file, &key).map_err(|e| io(&key_file, e))?;
        text
    };
    if let Some(d) = crate::extract_stage::dlc(a, b, p)? {
        text.push_str(&format!(", {d}"));
    }
    shaders::take_tables(&b.work, &b.profile.0)?;
    Ok(text)
}

/// Unpacks a RomFS image into `dest`, reporting the bytes.
fn unpack(p: &Say, img: &Path, dest: &Path, what: &str) -> Result<(usize, u64), String> {
    let _ = std::fs::remove_dir_all(dest);
    p.set(Update {
        step: Some(what),
        ..Default::default()
    });
    let count = crate::events::Count::default();
    p.counted(None, "B", &count, || {
        romfs::unpack(img, dest, |done, total| {
            count.done.store(done, std::sync::atomic::Ordering::Relaxed);
            count.set_total(total);
        })
    })
}

/// Unpacks the image `img` into the folder `dest` unless `dest` already holds this image: a key file beside the
/// folder records the image's size and change time. A new image is unpacked into a temporary folder that then takes
/// the place of the old one, so an interrupted unpack is never reused. The folder's files are linked into apps and
/// never written in place.
pub(super) fn unpack_once(p: &Say, img: &Path, dest: &Path, what: &str) -> Result<(), String> {
    let meta = std::fs::metadata(img).map_err(|e| format!("{}: {e}", img.display()))?;
    let changed = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let key = format!("{} {changed}\n", meta.len());
    let key_file = {
        let mut n = dest.as_os_str().to_owned();
        n.push(".key");
        PathBuf::from(n)
    };
    if dest.is_dir() && std::fs::read_to_string(&key_file).is_ok_and(|k| k == key) {
        p.say(&format!("{what}: reused"));
        return Ok(());
    }
    let tmp = {
        let mut n = dest.as_os_str().to_owned();
        n.push(".tmp");
        PathBuf::from(n)
    };
    let _ = std::fs::remove_file(&key_file);
    unpack(p, img, &tmp, what)?;
    let _ = std::fs::remove_dir_all(dest);
    std::fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    std::fs::write(&key_file, key).map_err(|e| format!("{}: {e}", key_file.display()))?;
    Ok(())
}

/// Links the files of the folder `from` into `dest`, reporting the bytes.
/// Files of `overlay` (patched ones) are taken from there.
pub(super) fn link(
    p: &Say,
    threads: usize,
    from: &Path,
    overlay: Option<&Path>,
    dest: &Path,
    what: &str,
) -> Result<(usize, u64), String> {
    p.set(Update {
        step: Some(what),
        ..Default::default()
    });
    let count = crate::events::Count::default();
    p.counted(None, "B", &count, || {
        romfs::link_tree(from, overlay, dest, threads, |done, total| {
            // the workers report out of order
            count
                .done
                .fetch_max(done, std::sync::atomic::Ordering::Relaxed);
            count.set_total(total);
        })
    })
}
