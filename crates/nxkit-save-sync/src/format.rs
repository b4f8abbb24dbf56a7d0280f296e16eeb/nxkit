//! The layout of a packed save, as `save_sync.c` of the old engine writes it. Pure, no IO.
//!
//! A packed save (`<folder>/<title ID>/save.dmsync`), little-endian:
//!
//! ```text
//! header  "DMSYNC1\n", u32 version (1), u32 entries, 32 hex digits of the hash, i64 saved, char title[24], char device[64]
//! entry   u8 type (1 folder, 2 file), u8 0, u16 path length, u16 guest-name length, u16 0, u64 size, i64 mtime,
//!         the path (save/... or save_device/...), the guest name, the contents
//! end     "DMSYNCE\n", then the end of the file
//! ```
//!
//! The hash is SHA-256 over each entry's first 16 header bytes (not the date), its path, its guest name and its
//! contents, in the order of the walk. A save is the first 16 bytes of it in lowercase hex. No files: no hash.

use nxb_model::Sha256;

/// The first bytes of a packed save.
pub(crate) const MAGIC: &[u8; 8] = b"DMSYNC1\n";
/// The bytes after the last entry.
pub(crate) const END: &[u8; 8] = b"DMSYNCE\n";
/// The version of the layout.
pub(crate) const VERSION: u32 = 1;
/// The size of the header.
pub(crate) const HDR: usize = 8 + 4 + 4 + HASH_LEN + 8 + 24 + 64;
/// The size of the header of an entry.
pub(crate) const ENT: usize = 24;
/// The hex digits of a hash.
pub const HASH_LEN: usize = 32;
/// The bytes of the title field, its last one a NUL.
const TITLE: usize = 24;
/// The bytes of the device field, its last one a NUL.
const DEVICE: usize = 64;
/// The longest path, as `PATH_MAX` of macOS: a path must be shorter.
pub(crate) const PATH_MAX: usize = 1024;
/// The names of the two folders in a packed save. On disk they are `<save>` and `<save>_device`.
pub(crate) const FOLDERS: [&str; 2] = ["save", "save_device"];

/// The kind of an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A folder, type 1.
    Folder,
    /// A regular file, type 2.
    File,
}

/// The header of one entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) kind: Kind,
    pub(crate) path_len: u16,
    pub(crate) guest_len: u16,
    pub(crate) size: u64,
    pub(crate) mtime: i64,
}

impl Entry {
    /// The 24 bytes of the entry header.
    pub(crate) fn encode(&self) -> [u8; ENT] {
        let mut h = [0u8; ENT];
        h[0] = match self.kind {
            Kind::Folder => 1,
            Kind::File => 2,
        };
        h[2..4].copy_from_slice(&self.path_len.to_le_bytes());
        h[4..6].copy_from_slice(&self.guest_len.to_le_bytes());
        h[8..16].copy_from_slice(&self.size.to_le_bytes());
        h[16..24].copy_from_slice(&self.mtime.to_le_bytes());
        h
    }

    /// The entry of 24 header bytes, or `None` for a type, a path length, a guest-name length or a folder size that
    /// `ss_read` refuses.
    pub(crate) fn decode(h: &[u8; ENT]) -> Option<Entry> {
        let kind = match h[0] {
            1 => Kind::Folder,
            2 => Kind::File,
            _ => return None,
        };
        let path_len = u16::from_le_bytes([h[2], h[3]]);
        let guest_len = u16::from_le_bytes([h[4], h[5]]);
        let size = u64::from_le_bytes(word(&h[8..16]));
        let mtime = i64::from_le_bytes(word(&h[16..24]));
        let bad = path_len == 0
            || usize::from(path_len) >= PATH_MAX
            || usize::from(guest_len) > crate::sys::GUEST_MAX
            || (kind == Kind::Folder && size != 0);
        (!bad).then_some(Entry {
            kind,
            path_len,
            guest_len,
            size,
            mtime,
        })
    }
}

fn word(b: &[u8]) -> [u8; 8] {
    let mut w = [0u8; 8];
    w.copy_from_slice(&b[..8]);
    w
}

/// The header of a packed save.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) entries: u32,
    /// The hash, empty for no save.
    pub(crate) hash: String,
    pub(crate) saved: i64,
    pub(crate) title: String,
    pub(crate) device: String,
}

/// The text of a fixed field: the bytes up to the first NUL.
fn field(b: &[u8]) -> String {
    let n = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

/// Copies at most `max - 1` bytes of `text` to `out`, as the C code leaves room for the NUL.
fn put_field(out: &mut [u8], text: &str, max: usize) {
    let b = text.as_bytes();
    let n = b.len().min(max - 1);
    out[..n].copy_from_slice(&b[..n]);
}

impl Header {
    /// The header bytes.
    pub(crate) fn encode(&self) -> [u8; HDR] {
        let mut h = [0u8; HDR];
        h[..8].copy_from_slice(MAGIC);
        h[8..12].copy_from_slice(&VERSION.to_le_bytes());
        h[12..16].copy_from_slice(&self.entries.to_le_bytes());
        put_field(&mut h[16..48], &self.hash, HASH_LEN + 1);
        h[48..56].copy_from_slice(&self.saved.to_le_bytes());
        put_field(&mut h[56..80], &self.title, TITLE);
        put_field(&mut h[80..144], &self.device, DEVICE);
        h
    }

    /// The header of the bytes, or `None` for another magic or version. The title and the device are read as
    /// `ss_read` reads them: 23 and 63 bytes at most.
    pub(crate) fn decode(h: &[u8; HDR]) -> Option<Header> {
        if &h[..8] != MAGIC || u32::from_le_bytes([h[8], h[9], h[10], h[11]]) != VERSION {
            return None;
        }
        Some(Header {
            entries: u32::from_le_bytes([h[12], h[13], h[14], h[15]]),
            hash: field(&h[16..48]),
            saved: i64::from_le_bytes(word(&h[48..56])),
            title: field(&h[56..56 + TITLE - 1]),
            device: field(&h[80..80 + DEVICE - 1]),
        })
    }
}

/// Whether `p` is a path of a packed save: under `save/` or `save_device/`, with no empty, `.` or `..` part, so a
/// damaged file never writes outside the folder it is unpacked into.
pub(crate) fn path_ok(p: &[u8]) -> bool {
    let rest = if let Some(r) = p.strip_prefix(b"save/") {
        r
    } else if let Some(r) = p.strip_prefix(b"save_device/") {
        r
    } else {
        return false;
    };
    !p.contains(&0)
        && rest
            .split(|c| *c == b'/')
            .all(|part| !part.is_empty() && part != b"." && part != b"..")
}

/// The running hash of a save.
#[derive(Default)]
pub(crate) struct Hasher(Sha256);

impl Hasher {
    /// Adds an entry: its first 16 header bytes, its path and its guest name.
    pub(crate) fn entry(&mut self, h: &[u8; ENT], path: &[u8], guest: &[u8]) {
        self.0.update(&h[..16]);
        self.0.update(path);
        self.0.update(guest);
    }

    /// Adds content bytes.
    pub(crate) fn content(&mut self, b: &[u8]) {
        self.0.update(b);
    }

    /// The hash: the first 16 bytes in lowercase hex, empty when the save has no files.
    pub(crate) fn finish(self, files: u32) -> String {
        if files == 0 {
            return String::new();
        }
        let d = self.0.finalize();
        d.as_bytes()[..HASH_LEN / 2]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// Whether `h` is a hash: 32 lowercase hex digits.
pub fn is_hash(h: &str) -> bool {
    h.len() == HASH_LEN
        && h.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_header_round_trips_and_bad_ones_are_refused() {
        let e = Entry {
            kind: Kind::File,
            path_len: 10,
            guest_len: 1,
            size: 5,
            mtime: 1_000_000_000,
        };
        let h = e.encode();
        assert_eq!(&h[..8], &[2, 0, 10, 0, 1, 0, 0, 0]);
        assert_eq!(Entry::decode(&h), Some(e));
        let mut folder = Entry {
            kind: Kind::Folder,
            ..e
        }
        .encode();
        assert_eq!(Entry::decode(&folder), None, "a folder with a size");
        folder[8] = 0;
        assert!(Entry::decode(&folder).is_some());
        for (i, v) in [(0, 3u8), (2, 0)] {
            let mut bad = h;
            bad[i] = v;
            bad[3] = 0;
            assert_eq!(Entry::decode(&bad), None);
        }
    }

    #[test]
    fn the_header_has_its_fields_where_save_sync_c_puts_them() {
        let h = Header {
            entries: 3,
            hash: "0123456789abcdef0123456789abcdef".into(),
            saved: 7,
            title: "0100ABC".into(),
            device: "Mac16,1".into(),
        };
        let b = h.encode();
        assert_eq!(b.len(), 144);
        assert_eq!(&b[..12], b"DMSYNC1\n\x01\0\0\0");
        assert_eq!(&b[16..48], h.hash.as_bytes());
        assert_eq!(&b[56..63], b"0100ABC");
        assert_eq!(&b[80..87], b"Mac16,1");
        assert_eq!(Header::decode(&b), Some(h));
    }

    #[test]
    fn a_path_must_stay_in_the_save() {
        for ok in ["save/a", "save_device/a/b", "save/.x"] {
            assert!(path_ok(ok.as_bytes()), "{ok}");
        }
        for bad in [
            "save",
            "save/",
            "save//a",
            "save/../a",
            "save/./a",
            "other/a",
            "save/a\0b",
        ] {
            assert!(!path_ok(bad.as_bytes()), "{bad}");
        }
    }

    #[test]
    fn no_files_means_no_hash() {
        assert_eq!(Hasher::default().finish(0), "");
        let h = Hasher::default().finish(1);
        assert!(is_hash(&h), "{h}");
        assert!(!is_hash("0123456789ABCDEF0123456789abcdef"));
    }
}
