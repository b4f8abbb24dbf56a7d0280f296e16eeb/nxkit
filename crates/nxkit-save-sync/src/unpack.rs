//! Reading a packed save, as `ss_read` of `save_sync.c`: the header, every entry, the hash and the end marker are
//! checked, and with a folder to unpack into, the save is written there as `<into>/save` and `<into>/save_device`.

use crate::format::{END, ENT, Entry, HDR, Hasher, Header, Kind, path_ok};
use crate::{Meta, sys};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

const CHUNK: usize = 1 << 16;

fn bad() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "the packed save is damaged or cut off",
    )
}

fn exact(r: &mut impl Read, buf: &mut [u8]) -> io::Result<()> {
    r.read_exact(buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            bad()
        } else {
            e
        }
    })
}

/// Sets the date of a file: both times, as `futimens` does in `ss_read`.
fn set_mtime(f: &File, t: i64) -> io::Result<()> {
    let at = if t >= 0 {
        UNIX_EPOCH + Duration::from_secs(t as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(t.unsigned_abs())
    };
    f.set_times(std::fs::FileTimes::new().set_accessed(at).set_modified(at))
}

/// Reads and checks the packed save `bundle`. With `into`, unpacks it there. A damaged or cut-off file, or a path
/// that leaves the save, fails. A file that fails may have left part of the save in `into`.
pub fn read(bundle: &Path, into: Option<&Path>) -> io::Result<Meta> {
    let mut f = BufReader::new(File::open(bundle)?);
    let mut hdr = [0u8; HDR];
    exact(&mut f, &mut hdr)?;
    let header = Header::decode(&hdr).ok_or_else(bad)?;
    let mut meta = Meta {
        saved: header.saved,
        title: header.title,
        device: header.device,
        ..Meta::default()
    };
    let mut hash = Hasher::default();
    let mut buf = vec![0u8; CHUNK];
    for _ in 0..header.entries {
        let mut h = [0u8; ENT];
        exact(&mut f, &mut h)?;
        let e = Entry::decode(&h).ok_or_else(bad)?;
        let mut path = vec![0u8; usize::from(e.path_len)];
        let mut guest = vec![0u8; usize::from(e.guest_len)];
        exact(&mut f, &mut path)?;
        exact(&mut f, &mut guest)?;
        if !path_ok(&path) {
            return Err(bad());
        }
        hash.entry(&h, &path, &guest);
        let mut out = None;
        if let Some(into) = into {
            let p = into.join(std::ffi::OsStr::from_bytes(&path));
            match e.kind {
                Kind::Folder => std::fs::create_dir_all(&p)?,
                Kind::File => {
                    if let Some(parent) = p.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    out = Some(OpenOptions::new().write(true).create_new(true).open(&p)?);
                }
            }
            if !guest.is_empty() {
                sys::set_guest_name(&p, &guest)?;
            }
        }
        let mut left = e.size;
        while left > 0 {
            let k = usize::try_from(left).unwrap_or(CHUNK).min(CHUNK);
            exact(&mut f, &mut buf[..k])?;
            hash.content(&buf[..k]);
            if let Some(o) = out.as_mut() {
                o.write_all(&buf[..k])?;
            }
            left -= k as u64;
        }
        if let Some(o) = out {
            // The file's own date: the newest one is the save's age when two changed saves are compared.
            let _ = set_mtime(&o, e.mtime);
        }
        if e.kind == Kind::File {
            meta.files += 1;
            meta.bytes += e.size as i64;
        }
    }
    let mut end = [0u8; 8];
    exact(&mut f, &mut end)?;
    let mut more = [0u8; 1];
    if &end != END || f.read(&mut more)? != 0 {
        return Err(bad());
    }
    meta.hash = hash.finish(meta.files);
    if meta.hash != header.hash {
        return Err(bad());
    }
    Ok(meta)
}
