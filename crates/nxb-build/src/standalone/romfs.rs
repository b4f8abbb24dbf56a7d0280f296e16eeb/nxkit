//! Unpacks a RomFS image (`RomFS.img` of the extract stage) into a folder of files, or links the files of a RomFS
//! folder (a NUP's, which the extract stage writes as files) into the app.
//!
//! The extract stage has already layered the base and the update, so one image is the merged RomFS. DLC images are
//! unpacked on their own, each into its own folder (see R50).

use nxb_extract::romfs::RomEntry;
use nxb_extract::storage::{FileStorage, Storage};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// The path of an entry below `dest`. A name with `..`, a drive or an absolute part is refused.
fn target(dest: &Path, e: &RomEntry) -> Result<PathBuf, String> {
    let p = std::str::from_utf8(&e.path).map_err(|_| {
        format!(
            "RomFS path is not UTF-8: {}",
            String::from_utf8_lossy(&e.path)
        )
    })?;
    let mut out = dest.to_path_buf();
    for c in Path::new(p.trim_start_matches('/')).components() {
        match c {
            Component::Normal(n) => out.push(n),
            _ => return Err(format!("RomFS path is not safe to unpack: {p}")),
        }
    }
    Ok(out)
}

/// Writes every file of the image below `dest`. Returns the number of files and bytes.
pub(crate) fn unpack(
    img: &Path,
    dest: &Path,
    mut each: impl FnMut(u64, u64),
) -> Result<(usize, u64), String> {
    const CHUNK: u64 = 8 << 20;
    let s = FileStorage::open(img).map_err(|e| e.to_string())?;
    let files = super::image::image_files(&s, img)?;
    let total: u64 = files.iter().map(|e| e.size).sum();
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut done = 0u64;
    for e in &files {
        let path = target(dest, e)?;
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|x| format!("{}: {x}", d.display()))?;
        }
        let mut out = std::io::BufWriter::new(
            std::fs::File::create(&path).map_err(|x| format!("{}: {x}", path.display()))?,
        );
        let mut at = 0u64;
        while at < e.size {
            let n = CHUNK.min(e.size - at);
            let buf = s
                .read_vec(e.offset + at, n as usize)
                .map_err(|x| x.to_string())?;
            out.write_all(&buf)
                .map_err(|x| format!("{}: {x}", path.display()))?;
            at += n;
            done += n;
            each(done, total);
        }
        out.flush()
            .map_err(|x| format!("{}: {x}", path.display()))?;
    }
    Ok((files.len(), total))
}

/// Puts every file of the folder `src` into `dest` (replaced) without copying bytes: a hard link, or where that fails
/// (another volume) `std::fs::copy`, which clones on APFS. The files of a build's work folder are written once and
/// never rewritten in place, so the app and the work folder can share them. A file the `overlay` folder also has
/// (same relative path) is taken from there. Returns the number of files and bytes.
pub(super) fn link_tree(
    src: &Path,
    overlay: Option<&Path>,
    dest: &Path,
    threads: usize,
    each: impl Fn(u64, u64) + Sync,
) -> Result<(usize, u64), String> {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    // the folders and the files, walked once
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let here = src.join(&rel);
        for e in std::fs::read_dir(&here).map_err(|e| io(&here, e))? {
            let e = e.map_err(|e| io(&here, e))?;
            let t = e.file_type().map_err(|x| io(&e.path(), x))?;
            let r = rel.join(e.file_name());
            if t.is_dir() {
                dirs.push(r.clone());
                stack.push(r);
            } else if t.is_file() {
                let size = e.metadata().map_err(|x| io(&e.path(), x))?.len();
                files.push((r, size));
            } else {
                return Err(format!("{}: not a file or a folder", e.path().display()));
            }
        }
    }
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|e| io(dest, e))?;
    }
    std::fs::create_dir_all(dest).map_err(|e| io(dest, e))?;
    for d in &dirs {
        std::fs::create_dir_all(dest.join(d)).map_err(|e| io(&dest.join(d), e))?;
    }
    let total: u64 = files.iter().map(|f| f.1).sum();
    let (next, done) = (AtomicUsize::new(0), AtomicU64::new(0));
    let fatal = std::sync::Mutex::new(None::<String>);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| {
                while let Some((rel, size)) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let (mut from, to) = (src.join(rel), dest.join(rel));
                    if let Some(o) = overlay.map(|o| o.join(rel)).filter(|o| o.is_file()) {
                        from = o;
                    }
                    let r = std::fs::hard_link(&from, &to)
                        .or_else(|_| std::fs::copy(&from, &to).map(|_| ()))
                        .map_err(|e| io(&from, e));
                    if let Err(e) = r {
                        if let Ok(mut g) = fatal.lock() {
                            g.get_or_insert(e);
                        }
                        break;
                    }
                    each(done.fetch_add(*size, Ordering::Relaxed) + size, total);
                }
            });
        }
    });
    if let Some(e) = fatal.into_inner().ok().flatten() {
        return Err(e);
    }
    Ok((files.len(), total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(p: &str) -> RomEntry {
        RomEntry {
            path: p.as_bytes().to_vec(),
            dir: false,
            size: 0,
            offset: 0,
        }
    }

    #[test]
    fn entries_stay_below_the_folder() {
        let d = Path::new("/x/romfs");
        assert_eq!(
            target(d, &entry("/Pack/a.pack")).unwrap(),
            d.join("Pack/a.pack")
        );
        assert!(target(d, &entry("/../etc/passwd")).is_err());
    }
}
