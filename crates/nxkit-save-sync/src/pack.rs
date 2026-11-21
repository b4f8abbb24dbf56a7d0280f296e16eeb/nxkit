//! Reading a save from disk: the scan and the pack, as `ss_scan` and `ss_pack` of `save_sync.c`.
//!
//! The walk visits every entry depth first, a folder before what it holds, names in byte order. That order gives the
//! same save the same hash on every device. Finder's `.DS_Store` is left out, and so are links, which the host never
//! makes in a save.

use crate::format::{Entry, FOLDERS, Hasher, Header, Kind};
use crate::{Meta, files, sys};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// The copy buffer.
const CHUNK: usize = 1 << 16;

struct Packer<'a> {
    hash: Hasher,
    out: Option<&'a mut File>,
    meta: Meta,
    entries: u32,
    buf: Vec<u8>,
}

impl Packer<'_> {
    fn entry(&mut self, abs: &Path, rel: &[u8], md: &std::fs::Metadata) -> io::Result<()> {
        let guest = sys::guest_name(abs);
        let file = md.is_file();
        let path_len =
            u16::try_from(rel.len()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let size = if file { md.len() } else { 0 };
        let mtime = md.mtime();
        let h = Entry {
            kind: if file { Kind::File } else { Kind::Folder },
            path_len,
            guest_len: guest.len() as u16,
            size,
            mtime,
        }
        .encode();
        self.hash.entry(&h, rel, &guest);
        if let Some(out) = self.out.as_deref_mut() {
            out.write_all(&h)?;
            out.write_all(rel)?;
            out.write_all(&guest)?;
        }
        self.entries += 1;
        if !file {
            return Ok(());
        }
        let mut f = File::open(abs)?;
        let mut left = size;
        while left > 0 {
            let want = usize::try_from(left).unwrap_or(CHUNK).min(CHUNK);
            let got = match f.read(&mut self.buf[..want]) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            self.hash.content(&self.buf[..got]);
            if let Some(out) = self.out.as_deref_mut() {
                out.write_all(&self.buf[..got])?;
            }
            left -= got as u64;
        }
        if left > 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "a file is shorter than its size",
            ));
        }
        self.meta.files += 1;
        self.meta.bytes += size as i64;
        self.meta.saved = self.meta.saved.max(mtime);
        Ok(())
    }

    fn walk(&mut self, abs: &Path, rel: &[u8]) -> io::Result<()> {
        for name in files::names(abs)? {
            if name.as_bytes() == b".DS_Store" {
                continue;
            }
            let a = abs.join(&name);
            let mut p = rel.to_vec();
            p.push(b'/');
            p.extend_from_slice(name.as_bytes());
            let md = std::fs::symlink_metadata(&a)?;
            if md.is_dir() {
                self.entry(&a, &p, &md)?;
                self.walk(&a, &p)?;
            } else if md.is_file() {
                self.entry(&a, &p, &md)?;
            }
        }
        Ok(())
    }

    /// Walks the save and its device save.
    fn save(&mut self, save: &Path) -> io::Result<()> {
        for (i, folder) in FOLDERS.iter().enumerate() {
            let dir = files::folder(save, i);
            if files::has_entries(&dir)? {
                self.walk(&dir, folder.as_bytes())?;
            }
        }
        Ok(())
    }

    fn new(out: Option<&mut File>) -> Packer<'_> {
        Packer {
            hash: Hasher::default(),
            out,
            meta: Meta::default(),
            entries: 0,
            buf: vec![0u8; CHUNK],
        }
    }

    fn finish(self) -> (Meta, u32) {
        let mut meta = self.meta;
        meta.hash = self.hash.finish(meta.files);
        (meta, self.entries)
    }
}

/// The committed save as it is on disk: `save` and `<save>_device`. `files` 0: no save yet.
pub fn scan(save: &Path) -> io::Result<Meta> {
    let mut k = Packer::new(None);
    k.save(save)?;
    Ok(k.finish().0)
}

/// The title ID in capitals: one spelling in the cloud, whoever packed the app. At most 23 characters.
pub(crate) fn upper_title(title: &str) -> String {
    title.to_ascii_uppercase().chars().take(23).collect()
}

/// Packs the save into the file `out`. Gives what was packed. A file that is not complete is removed.
pub fn pack(save: &Path, title: &str, out: &Path) -> io::Result<Meta> {
    pack_as(save, title, &sys::device_name(), out)
}

/// [`pack`] with the device name given.
pub(crate) fn pack_as(save: &Path, title: &str, device: &str, out: &Path) -> io::Result<Meta> {
    let r = (|| {
        let mut f = File::create(out)?;
        f.write_all(&[0u8; crate::format::HDR])?;
        let mut k = Packer::new(Some(&mut f));
        k.save(save)?;
        let (mut meta, entries) = k.finish();
        f.write_all(crate::format::END)?;
        meta.title = upper_title(title);
        meta.device = device.chars().take(63).collect();
        let h = Header {
            entries,
            hash: meta.hash.clone(),
            saved: meta.saved,
            title: meta.title.clone(),
            device: meta.device.clone(),
        };
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&h.encode())?;
        f.sync_all()?;
        Ok(meta)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(out);
    }
    r
}
