//! A RomFS image built from the romfs folder, for `OpenDataStorageByCurrentProcess`.
//!
//! The game reads the storage of its own romfs when it parses the RomFS itself, or when the SDK mounts it. The folder is
//! the source (the build merged base, update and DLC layers there), so the host makes the image of it: the header, the
//! hash tables and the entries as in the RomFS format, and the file data read from the files on demand. Nothing of the
//! file data is copied until the game asks for it.
//!
//! The same scan gives the size of the work buffer for `QueryMountAddOnContentCacheSize`.
//!
//! The layout follows the RomFS format as the tools that build it write it. It is not checked against an image of a
//! real title.

use crate::names;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

const NONE: u32 = 0xFFFF_FFFF;
const HEADER_SIZE: u64 = 0x50;
const TABLES_AT: u64 = 0x200;
const DATA_ALIGN: u64 = 0x200;
const FILE_ALIGN: u64 = 0x10;
const DIR_ENTRY: u64 = 0x18;
const FILE_ENTRY: u64 = 0x20;

struct FileRec {
    name: Vec<u8>,
    path: PathBuf,
    size: u64,
}

struct Node {
    parent: usize,
    name: Vec<u8>,
    path: PathBuf,
    dirs: Vec<usize>,
    files: Vec<FileRec>,
}

struct Placed {
    start: u64,
    size: u64,
    path: PathBuf,
}

/// The image of a folder.
pub(crate) struct Image {
    meta: Vec<u8>,
    files: Vec<Placed>,
    size: u64,
    work: u64,
}

fn align(v: u64, a: u64) -> u64 {
    v.div_ceil(a) * a
}

/// The number of buckets of a hash table of `n` entries, as the RomFS tools choose it.
pub(crate) fn buckets(n: u64) -> u64 {
    if n < 3 {
        return 3;
    }
    if n < 19 {
        return n | 1;
    }
    let mut c = n;
    while [2, 3, 5, 7, 11, 13, 17].iter().any(|p| c % p == 0) {
        c += 1;
    }
    c
}

fn hash(parent: u32, name: &[u8]) -> u32 {
    let mut h = parent ^ 123_456_789;
    for &b in name {
        h = h.rotate_right(5) ^ u32::from(b);
    }
    h
}

/// The folders and files below `root`, in the order of their entries. A folder is visited after the folders before it,
/// so the scan needs no recursion.
fn scan(root: &Path) -> io::Result<Vec<Node>> {
    let mut nodes = vec![Node { parent: 0, name: Vec::new(), path: root.to_path_buf(), dirs: Vec::new(), files: Vec::new() }];
    let mut i = 0;
    while i < nodes.len() {
        let dir = nodes[i].path.clone();
        let mut found: Vec<(Vec<u8>, PathBuf, std::fs::Metadata)> = Vec::new();
        for e in std::fs::read_dir(&dir)? {
            let physical = e?.file_name();
            let path = dir.join(&physical);
            found.push((names::guest_name(&dir, &physical)?, path.clone(), std::fs::metadata(&path)?));
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, path, meta) in found {
            if meta.is_dir() {
                let child = nodes.len();
                nodes[i].dirs.push(child);
                nodes.push(Node { parent: i, name, path, dirs: Vec::new(), files: Vec::new() });
            } else {
                nodes[i].files.push(FileRec { name, path, size: meta.len() });
            }
        }
        i += 1;
    }
    Ok(nodes)
}

fn put32(buf: &mut [u8], at: u64, v: u32) {
    let at = at as usize;
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(buf: &mut [u8], at: u64, v: u64) {
    let at = at as usize;
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn offset32(v: u64) -> io::Result<u32> {
    u32::try_from(v).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
}

impl Image {
    /// Builds the image of the folder `root`.
    pub(crate) fn build(root: &Path) -> io::Result<Image> {
        let nodes = scan(root)?;
        let nfiles: u64 = nodes.iter().map(|n| n.files.len() as u64).sum();
        let (dir_buckets, file_buckets) = (buckets(nodes.len() as u64), buckets(nfiles));

        let mut dir_off = Vec::with_capacity(nodes.len());
        let mut dmeta = 0u64;
        for n in &nodes {
            dir_off.push(dmeta);
            dmeta += DIR_ENTRY + align(n.name.len() as u64, 4);
        }
        let mut fmeta = 0u64;
        let mut file_off: Vec<Vec<u64>> = Vec::with_capacity(nodes.len());
        for n in &nodes {
            let mut offs = Vec::with_capacity(n.files.len());
            for f in &n.files {
                offs.push(fmeta);
                fmeta += FILE_ENTRY + align(f.name.len() as u64, 4);
            }
            file_off.push(offs);
        }

        let dir_hash_at = TABLES_AT;
        let dir_meta_at = dir_hash_at + 4 * dir_buckets;
        let file_hash_at = dir_meta_at + dmeta;
        let file_meta_at = file_hash_at + 4 * file_buckets;
        let data_at = align(file_meta_at + fmeta, DATA_ALIGN);
        let work = 4 * dir_buckets + dmeta + 4 * file_buckets + fmeta;

        let mut next_dir = vec![NONE; nodes.len()];
        for n in &nodes {
            for w in n.dirs.windows(2) {
                next_dir[w[0]] = offset32(dir_off[w[1]])?;
            }
        }

        let mut meta = vec![0u8; data_at as usize];
        meta[dir_hash_at as usize..dir_meta_at as usize].fill(0xFF);
        meta[file_hash_at as usize..file_meta_at as usize].fill(0xFF);
        let mut files = Vec::new();
        let mut cursor = 0u64;

        for (i, n) in nodes.iter().enumerate() {
            let at = dir_meta_at + dir_off[i];
            let parent = offset32(dir_off[n.parent])?;
            put32(&mut meta, at, parent);
            put32(&mut meta, at + 4, next_dir[i]);
            put32(&mut meta, at + 8, n.dirs.first().map_or(Ok(NONE), |&c| offset32(dir_off[c]))?);
            put32(&mut meta, at + 12, n.files.first().map_or(Ok(NONE), |_| offset32(file_off[i][0]))?);
            put32(&mut meta, at + 20, offset32(n.name.len() as u64)?);
            meta[(at + DIR_ENTRY) as usize..(at + DIR_ENTRY) as usize + n.name.len()].copy_from_slice(&n.name);
            let own = offset32(dir_off[i])?;
            let bucket = u64::from(hash(parent, &n.name)) % dir_buckets;
            let slot = dir_hash_at + 4 * bucket;
            let head = u32::from_le_bytes(meta[slot as usize..slot as usize + 4].try_into().map_err(|_| io::ErrorKind::InvalidData)?);
            put32(&mut meta, at + 16, head);
            put32(&mut meta, slot, own);

            for (k, f) in n.files.iter().enumerate() {
                let at = file_meta_at + file_off[i][k];
                let start = align(cursor, FILE_ALIGN);
                cursor = start + f.size;
                let own = offset32(file_off[i][k])?;
                let sibling = file_off[i].get(k + 1).map_or(Ok(NONE), |&s| offset32(s))?;
                put32(&mut meta, at, offset32(dir_off[i])?);
                put32(&mut meta, at + 4, sibling);
                put64(&mut meta, at + 8, start);
                put64(&mut meta, at + 16, f.size);
                put32(&mut meta, at + 28, offset32(f.name.len() as u64)?);
                meta[(at + FILE_ENTRY) as usize..(at + FILE_ENTRY) as usize + f.name.len()].copy_from_slice(&f.name);
                let bucket = u64::from(hash(offset32(dir_off[i])?, &f.name)) % file_buckets;
                let slot = file_hash_at + 4 * bucket;
                let head = u32::from_le_bytes(meta[slot as usize..slot as usize + 4].try_into().map_err(|_| io::ErrorKind::InvalidData)?);
                put32(&mut meta, at + 24, head);
                put32(&mut meta, slot, own);
                files.push(Placed { start: data_at + start, size: f.size, path: f.path.clone() });
            }
        }

        for (k, v) in [
            HEADER_SIZE,
            dir_hash_at,
            4 * dir_buckets,
            dir_meta_at,
            dmeta,
            file_hash_at,
            4 * file_buckets,
            file_meta_at,
            fmeta,
        ]
        .into_iter()
        .enumerate()
        {
            put64(&mut meta, 8 * k as u64, v);
        }
        put64(&mut meta, 8 * 9, data_at);
        Ok(Image { meta, files, size: data_at + cursor, work })
    }

    /// The size of the image.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    /// The size of the work buffer the SDK asks for to mount this folder.
    pub(crate) fn work_size(&self) -> u64 {
        self.work
    }

    /// Fills `out` from `offset`. The range must lie inside the image. Bytes between two files read as zero.
    pub(crate) fn read(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        let end = offset.checked_add(out.len() as u64).ok_or(io::ErrorKind::InvalidInput)?;
        if end > self.size {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        out.fill(0);
        let meta_len = self.meta.len() as u64;
        if offset < meta_len {
            let n = (meta_len.min(end) - offset) as usize;
            out[..n].copy_from_slice(&self.meta[offset as usize..offset as usize + n]);
        }
        let first = self.files.partition_point(|f| f.start + f.size <= offset);
        for f in &self.files[first..] {
            if f.start >= end {
                break;
            }
            let from = offset.max(f.start);
            let to = end.min(f.start + f.size);
            if from >= to {
                continue;
            }
            let file = std::fs::File::open(&f.path)?;
            let dst = &mut out[(from - offset) as usize..(to - offset) as usize];
            file.read_exact_at(dst, from - f.start)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    fn u32_at(img: &Image, at: u64) -> u32 {
        u32::from_le_bytes(img.meta[at as usize..at as usize + 4].try_into().unwrap())
    }

    fn u64_at(img: &Image, at: u64) -> u64 {
        u64::from_le_bytes(img.meta[at as usize..at as usize + 8].try_into().unwrap())
    }

    /// Finds a file by its hash chain, as a game that parses the image does.
    fn find_file(img: &Image, parent: u32, name: &[u8]) -> Option<(u64, u64)> {
        let (hash_at, hash_size, meta_at) = (u64_at(img, 0x28), u64_at(img, 0x30), u64_at(img, 0x38));
        let buckets = hash_size / 4;
        let mut off = u32_at(img, hash_at + 4 * (u64::from(hash(parent, name)) % buckets));
        while off != NONE {
            let at = meta_at + u64::from(off);
            let len = u64::from(u32_at(img, at + 28));
            let n = &img.meta[(at + FILE_ENTRY) as usize..(at + FILE_ENTRY + len) as usize];
            if u32_at(img, at) == parent && n == name {
                return Some((u64_at(img, at + 8), u64_at(img, at + 16)));
            }
            off = u32_at(img, at + 24);
        }
        None
    }

    #[test]
    fn bucket_counts_follow_the_rule_of_the_tools() {
        assert_eq!(buckets(0), 3);
        assert_eq!(buckets(2), 3);
        assert_eq!(buckets(4), 5);
        assert_eq!(buckets(18), 19);
        assert_eq!(buckets(19), 19);
        assert_eq!(buckets(30), 31);
        assert_eq!(buckets(34), 37);
    }

    #[test]
    fn the_image_holds_the_files_of_the_folder() {
        let root = scratch("image");
        std::fs::create_dir_all(root.join("Pack")).unwrap();
        std::fs::write(root.join("a.bin"), b"AAAA").unwrap();
        std::fs::write(root.join("Pack").join("b.bin"), vec![7u8; 100]).unwrap();
        let img = Image::build(&root).unwrap();
        assert_eq!(u64_at(&img, 0), HEADER_SIZE);
        let data_at = u64_at(&img, 0x48);
        assert_eq!(data_at % DATA_ALIGN, 0);
        assert_eq!(img.size(), data_at + 4 + 12 + 100);

        let (off, size) = find_file(&img, 0, b"a.bin").unwrap();
        assert_eq!(size, 4);
        let mut buf = [0u8; 4];
        img.read(data_at + off, &mut buf).unwrap();
        assert_eq!(&buf, b"AAAA");

        let (dir_hash, dir_hash_size, dir_meta) = (u64_at(&img, 8), u64_at(&img, 16), u64_at(&img, 24));
        let name = b"Pack";
        let root_off = 0u32;
        let mut at = u32_at(&img, dir_hash + 4 * (u64::from(hash(root_off, name)) % (dir_hash_size / 4)));
        while at != NONE && &img.meta[(dir_meta + u64::from(at) + DIR_ENTRY) as usize..][..4] != name {
            at = u32_at(&img, dir_meta + u64::from(at) + 16);
        }
        assert_ne!(at, NONE);
        let (off, size) = find_file(&img, at, b"b.bin").unwrap();
        assert_eq!(size, 100);
        let mut buf = [0u8; 100];
        img.read(data_at + off, &mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 7));
    }

    #[test]
    fn a_read_across_the_gap_between_files_is_zero_there() {
        let root = scratch("image-gap");
        std::fs::write(root.join("a"), b"AAAA").unwrap();
        std::fs::write(root.join("b"), b"BBBB").unwrap();
        let img = Image::build(&root).unwrap();
        let data_at = u64_at(&img, 0x48);
        let mut buf = [9u8; 20];
        img.read(data_at, &mut buf).unwrap();
        assert_eq!(&buf[..4], b"AAAA");
        assert!(buf[4..16].iter().all(|&b| b == 0));
        assert_eq!(&buf[16..], b"BBBB");
        assert!(img.read(img.size() - 1, &mut [0u8; 2]).is_err());
    }

    #[test]
    fn an_empty_folder_has_a_work_size_and_no_data() {
        let root = scratch("image-empty");
        let img = Image::build(&root).unwrap();
        assert_eq!(img.size(), u64_at(&img, 0x48));
        assert_eq!(img.work_size(), 4 * 3 + 0x18 + 4 * 3);
        assert!(img.read(0, &mut [0u8; 8]).is_ok());
    }
}
