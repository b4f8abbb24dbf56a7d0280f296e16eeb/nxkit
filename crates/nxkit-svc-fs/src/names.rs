//! Guest paths and how a path name becomes a host path.
//!
//! APFS is case-insensitive and Horizon is not, so every component is looked up by the exact guest name. An entry
//! whose guest name collides with another one on the host gets the host name `.dm-case-N`, and the guest name is stored
//! in the attribute `org.d2rmobile.guest-name`. The attribute survives a clone, so a commit needs no other record. A
//! directory listing gives the guest names back (R13: the same as `dm/host/hle_fs.c`).
//!
//! A read-only mount does not change while the game runs, so the names of each of its folders are indexed once, sorted
//! by guest name. A writable mount reads the folder at each step.

use crate::sys;
use crate::table::lock;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::ffi::{OsStr, OsString};
use std::hash::{Hash, Hasher};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The longest path and the longest entry name that Horizon takes.
pub(crate) const MAX_NAME: usize = 0x300;

const CASE_PREFIX: &[u8] = b".dm-case-";
const INDEX_SHARDS: usize = 16;

/// A path with `\` as `/`. Hosts before nxnative turned the backslash took it as part of a name, so a save written
/// by a game that uses `\` is found under the `/` form.
pub(crate) fn fwd_slashes(p: &[u8]) -> Cow<'_, [u8]> {
    if p.contains(&b'\\') {
        Cow::Owned(
            p.iter()
                .map(|&b| if b == b'\\' { b'/' } else { b })
                .collect(),
        )
    } else {
        Cow::Borrowed(p)
    }
}

/// Splits `rom:/a/b` into the mount name `rom` and the rest `/a/b`.
pub(crate) fn split_mount(p: &[u8]) -> Option<(&[u8], &[u8])> {
    let c = p.iter().position(|&b| b == b':')?;
    Some((&p[..c], &p[c + 1..]))
}

/// The components of a path below a mount. A `.` and an empty component are dropped and a `..` goes up one level, as
/// Horizon's path normalizer does. A `..` above the root gives `None`.
pub(crate) fn components(rest: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out: Vec<&[u8]> = Vec::new();
    for part in rest.split(|&b| b == b'/') {
        match part {
            b"" | b"." => {}
            b".." => {
                out.pop()?;
            }
            p => out.push(p),
        }
    }
    Some(out)
}

fn too_long() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidInput)
}

/// The guest name of the host entry `physical` in `parent`. Only the names chosen on a collision carry the attribute,
/// so a listing does not ask the system for each entry.
pub(crate) fn guest_name(parent: &Path, physical: &OsStr) -> io::Result<Vec<u8>> {
    let bytes = physical.as_bytes();
    if !bytes.starts_with(CASE_PREFIX) {
        return Ok(bytes.to_vec());
    }
    Ok(sys::get_guest_name(&parent.join(physical))?.unwrap_or_else(|| bytes.to_vec()))
}

/// Stores the guest name of a new or renamed entry. The attribute is set when the host name differs from the guest
/// name and removed when it does not, because a rename can move an old attribute along.
pub(crate) fn set_guest_name(host: &Path, guest: &[u8]) -> io::Result<()> {
    let physical = host.file_name().map(OsStr::as_bytes).unwrap_or_default();
    if physical == guest {
        sys::set_guest_name(host, None)
    } else {
        sys::set_guest_name(host, Some(guest))
    }
}

struct DirIndex {
    entries: Vec<(Vec<u8>, OsString)>,
}

/// The name indexes of the folders of one read-only mount.
pub(crate) struct IndexCache {
    shards: Vec<Mutex<HashMap<PathBuf, Arc<DirIndex>>>>,
}

impl IndexCache {
    /// An empty cache.
    pub(crate) fn new() -> IndexCache {
        IndexCache {
            shards: (0..INDEX_SHARDS)
                .map(|_| Mutex::new(HashMap::new()))
                .collect(),
        }
    }

    fn get(&self, dir: &Path) -> io::Result<Arc<DirIndex>> {
        let mut h = DefaultHasher::new();
        dir.hash(&mut h);
        let shard = &self.shards[(h.finish() % INDEX_SHARDS as u64) as usize];
        if let Some(i) = lock(shard).get(dir) {
            return Ok(Arc::clone(i));
        }
        let mut entries = Vec::new();
        for e in std::fs::read_dir(dir)? {
            let physical = e?.file_name();
            entries.push((guest_name(dir, &physical)?, physical));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let index = Arc::new(DirIndex { entries });
        Ok(Arc::clone(
            lock(shard).entry(dir.to_path_buf()).or_insert(index),
        ))
    }
}

/// The host path of the entry `name` in `dir`. With `create`, a name that does not exist gets a host name that no
/// other entry has.
fn component(
    cache: &IndexCache,
    ro: bool,
    dir: &Path,
    name: &[u8],
    create: bool,
) -> io::Result<PathBuf> {
    if name.len() > MAX_NAME {
        return Err(too_long());
    }
    if ro && !create {
        let index = cache.get(dir)?;
        return match index.entries.binary_search_by(|e| e.0.as_slice().cmp(name)) {
            Ok(i) => Ok(dir.join(&index.entries[i].1)),
            Err(_) => Err(io::Error::from(io::ErrorKind::NotFound)),
        };
    }
    for e in std::fs::read_dir(dir)? {
        let physical = e?.file_name();
        if guest_name(dir, &physical)? == name {
            return Ok(dir.join(physical));
        }
    }
    if !create {
        return Err(io::Error::from(io::ErrorKind::NotFound));
    }
    let plain = dir.join(OsStr::from_bytes(name));
    if plain.symlink_metadata().is_err() {
        return Ok(plain);
    }
    // The host sees another entry under this name (APFS ignores case), so the entry gets a name of its own.
    for n in 1u32.. {
        let mut physical = CASE_PREFIX.to_vec();
        physical.extend_from_slice(n.to_string().as_bytes());
        let candidate = dir.join(OsString::from_vec(physical));
        if candidate.symlink_metadata().is_err() {
            return Ok(candidate);
        }
    }
    Err(too_long())
}

/// The host path of `comps` below `root`. Only the last component may be new, and only with `create`.
pub(crate) fn walk(
    cache: &IndexCache,
    ro: bool,
    root: &Path,
    comps: &[&[u8]],
    create: bool,
) -> io::Result<PathBuf> {
    let mut path = root.to_path_buf();
    for (i, part) in comps.iter().enumerate() {
        path = component(cache, ro, &path, part, create && i + 1 == comps.len())?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    #[test]
    fn components_are_normalised_lexically() {
        assert_eq!(components(b"/a/./b//c"), Some(vec![&b"a"[..], b"b", b"c"]));
        assert_eq!(
            components(b"/system/snd/../../sounds/x"),
            Some(vec![&b"sounds"[..], b"x"])
        );
        assert_eq!(components(b"/.."), None);
        assert_eq!(components(b""), Some(vec![]));
    }

    #[test]
    fn a_path_splits_at_the_first_colon() {
        assert_eq!(split_mount(b"rom:/a:b"), Some((&b"rom"[..], &b"/a:b"[..])));
        assert_eq!(split_mount(b"no mount"), None);
    }

    #[test]
    fn backslashes_become_slashes() {
        assert_eq!(&*fwd_slashes(b"save:\\dir\\f"), b"save:/dir/f");
        assert!(matches!(fwd_slashes(b"save:/f"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_read_only_mount_looks_names_up_in_its_index() {
        let root = scratch("names-ro");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a").join("File.bin"), b"x").unwrap();
        let cache = IndexCache::new();
        let hit = walk(&cache, true, &root, &[b"a", b"File.bin"], false).unwrap();
        assert_eq!(hit, root.join("a").join("File.bin"));
        let miss = walk(&cache, true, &root, &[b"a", b"file.bin"], false);
        assert_eq!(miss.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_writable_mount_finds_the_exact_name_only() {
        let root = scratch("names-rw");
        std::fs::write(root.join("Save.sav"), b"x").unwrap();
        let cache = IndexCache::new();
        assert!(walk(&cache, false, &root, &[b"Save.sav"], false).is_ok());
        #[cfg(not(target_vendor = "apple"))]
        assert!(walk(&cache, false, &root, &[b"save.sav"], false).is_err());
        let fresh = walk(&cache, false, &root, &[b"new.sav"], true).unwrap();
        assert_eq!(fresh, root.join("new.sav"));
    }

    #[test]
    fn a_name_that_is_too_long_is_refused() {
        let root = scratch("names-long");
        let long = vec![b'a'; MAX_NAME + 1];
        let cache = IndexCache::new();
        assert!(walk(&cache, false, &root, &[&long], true).is_err());
    }
}
