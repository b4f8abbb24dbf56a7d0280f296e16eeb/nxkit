//! Unpacks a RomFS image (`RomFS.img` of the extract stage) into a folder of files.
//!
//! The extract stage has already layered the base and the update, so one image is the merged RomFS. DLC images are
//! unpacked on their own, each into its own folder (see R50).

use nxb_extract::romfs::RomEntry;
use nxb_extract::storage::{FileStorage, Storage};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// The path of an entry below `dest`. A name with `..`, a drive or an absolute part is refused.
fn target(dest: &Path, e: &RomEntry) -> Result<PathBuf, String> {
    let p = std::str::from_utf8(&e.path)
        .map_err(|_| format!("RomFS path is not UTF-8: {}", String::from_utf8_lossy(&e.path)))?;
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
pub fn unpack(img: &Path, dest: &Path, mut each: impl FnMut(u64, u64)) -> Result<(usize, u64), String> {
    const CHUNK: u64 = 8 << 20;
    let s = FileStorage::open(img).map_err(|e| e.to_string())?;
    let files = crate::host::survey_extract::image_files(&s, img)?;
    let total: u64 = files.iter().map(|e| e.size).sum();
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
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
            let buf = s.read_vec(e.offset + at, n as usize).map_err(|x| x.to_string())?;
            out.write_all(&buf).map_err(|x| format!("{}: {x}", path.display()))?;
            at += n;
            done += n;
            each(done, total);
        }
        out.flush().map_err(|x| format!("{}: {x}", path.display()))?;
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
        assert_eq!(target(d, &entry("/Pack/a.pack")).unwrap(), d.join("Pack/a.pack"));
        assert!(target(d, &entry("/../etc/passwd")).is_err());
    }
}
