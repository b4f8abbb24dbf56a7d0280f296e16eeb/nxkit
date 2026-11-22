//! Unreal pak files, versions 3 and 4: read the index, read one entry, write a pak of uncompressed entries.
//!
//! Layout of a pak: the entries (each a copy of its index record, then the data), the index, and a 45 byte footer
//! (`encrypted: u8, magic: u32, version: u32, index offset: u64, index size: u64, index SHA-1`). All numbers are little
//! endian. The records of the index hold the offset, size, uncompressed size, compression method, SHA-1, the block
//! list when compressed, flags and the block size. The block offsets of a compressed entry are absolute in these two
//! versions.

use nxb_model::sha1;
use nxtitle::RomfsRead;

const MAGIC: u32 = 0x5A6F_12E1;
const FOOTER: u64 = 45;
/// The largest index that is read.
const MAX_INDEX: u64 = 1 << 30;
/// The largest entry that is read.
const MAX_ENTRY: u64 = 64 << 20;
const ZLIB: u32 = 1;

/// One record of the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Mount point and name, as the engine joins them (`../../../Game/Config/Engine.ini`).
    pub name: String,
    offset: u64,
    size: u64,
    usize: u64,
    method: u32,
    blocks: Vec<(u64, u64)>,
    flags: u8,
    block_size: u32,
}

impl Entry {
    /// The name without the leading `../` parts: the path from the game's root folder.
    pub(crate) fn path(&self) -> &str {
        let mut p = self.name.as_str();
        while let Some(r) = p.strip_prefix("../").or_else(|| p.strip_prefix("./")) {
            p = r;
        }
        p.trim_start_matches('/')
    }

    /// The size of the record that precedes the data of the entry in the file.
    fn header_len(&self) -> u64 {
        let blocks = if self.method != 0 {
            4 + 16 * self.blocks.len() as u64
        } else {
            0
        };
        8 * 3 + 4 + 20 + blocks + 1 + 4
    }
}

struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.b.len());
        let end = end.ok_or("the pak index is cut short")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| "i32")?,
        ))
    }

    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| "i64")?,
        ))
    }

    fn u64(&mut self) -> Result<u64, String> {
        u64::try_from(self.i64()?).map_err(|_| "a negative size or offset in the pak index".into())
    }

    fn string(&mut self) -> Result<String, String> {
        let n = self.i32()?;
        if n >= 0 {
            let b = self.take(n as usize)?;
            let b = b.strip_suffix(&[0]).unwrap_or(b);
            return Ok(b.iter().map(|&c| c as char).collect());
        }
        let units = n.checked_neg().ok_or("bad string length")? as usize;
        let b = self.take(units.checked_mul(2).ok_or("bad string length")?)?;
        let u: Vec<u16> = b
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let u = u.strip_suffix(&[0]).unwrap_or(&u);
        Ok(String::from_utf16_lossy(u))
    }
}

/// The entries of the pak at `path` in the RomFS.
pub(crate) fn read_index(romfs: &dyn RomfsRead, path: &str) -> Result<Vec<Entry>, String> {
    let at = |e: String| format!("{path}: {e}");
    let len = romfs.len(path)?.ok_or_else(|| at("no such file".into()))?;
    if len < FOOTER {
        return Err(at("too short for a pak".into()));
    }
    let f = romfs.read_range(path, len - FOOTER, FOOTER as usize)?;
    let mut c = Cursor { b: &f, at: 0 };
    let encrypted = c.take(1).map_err(at)?[0];
    let magic = c.i32().map_err(at)? as u32;
    let version = c.i32().map_err(at)?;
    let (off, size) = (c.u64().map_err(at)?, c.u64().map_err(at)?);
    if magic != MAGIC {
        return Err(at("not a pak (the footer has no magic)".into()));
    }
    if !(3..=4).contains(&version) {
        return Err(at(format!(
            "pak version {version} is not supported (3 and 4 are)"
        )));
    }
    if encrypted != 0 {
        return Err(at("the index is encrypted".into()));
    }
    if size > MAX_INDEX || off.checked_add(size).is_none_or(|e| e > len - FOOTER) {
        return Err(at("the index lies outside the file".into()));
    }
    let idx = romfs.read_range(path, off, size as usize)?;
    parse_index(&idx).map_err(at)
}

fn parse_index(idx: &[u8]) -> Result<Vec<Entry>, String> {
    let mut c = Cursor { b: idx, at: 0 };
    let mount = c.string()?;
    let count = c.i32()?;
    let mut out = Vec::new();
    for _ in 0..count.max(0) {
        let name = format!("{mount}{}", c.string()?);
        let (offset, size, usize) = (c.u64()?, c.u64()?, c.u64()?);
        let method = c.i32()? as u32;
        c.take(20)?;
        let mut blocks = Vec::new();
        if method != 0 {
            for _ in 0..c.i32()?.max(0) {
                blocks.push((c.u64()?, c.u64()?));
            }
        }
        let flags = c.take(1)?[0];
        let block_size = c.i32()? as u32;
        out.push(Entry {
            name,
            offset,
            size,
            usize,
            method,
            blocks,
            flags,
            block_size,
        });
    }
    Ok(out)
}

/// The bytes of one entry of the pak at `path`, decompressed.
pub(crate) fn read_entry(romfs: &dyn RomfsRead, path: &str, e: &Entry) -> Result<Vec<u8>, String> {
    let at = |m: String| format!("{path}, {}: {m}", e.name);
    if e.flags & 1 != 0 {
        return Err(at("the entry is encrypted".into()));
    }
    if e.usize > MAX_ENTRY {
        return Err(at(format!("the entry is {} bytes, too large", e.usize)));
    }
    if e.method == 0 {
        if e.size != e.usize {
            return Err(at("size and uncompressed size differ".into()));
        }
        let start = e
            .offset
            .checked_add(e.header_len())
            .ok_or_else(|| at("offset".into()))?;
        return romfs.read_range(path, start, e.size as usize);
    }
    if e.method != ZLIB {
        return Err(at(format!("compression method {} is not zlib", e.method)));
    }
    let mut out = Vec::with_capacity(e.usize as usize);
    for &(s, t) in &e.blocks {
        let n = t
            .checked_sub(s)
            .filter(|n| *n <= MAX_ENTRY)
            .ok_or_else(|| at("bad block".into()))?;
        let raw = romfs.read_range(path, s, n as usize)?;
        let limit = (e.block_size as usize).max(1);
        let part = nxb_sys::zlib::inflate_bytes(&raw, 15, limit)
            .ok_or_else(|| at("a block does not inflate".into()))?;
        out.extend_from_slice(&part);
    }
    if out.len() as u64 != e.usize {
        return Err(at(
            "the blocks do not add up to the uncompressed size".into()
        ));
    }
    Ok(out)
}

fn fstring(out: &mut Vec<u8>, s: &str) -> Result<(), String> {
    if !s.is_ascii() {
        return Err(format!("{s}: pak names are ASCII"));
    }
    out.extend_from_slice(&(s.len() as i32 + 1).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    out.push(0);
    Ok(())
}

/// The record of an uncompressed entry at `offset`, as in the index and before the data.
fn record(offset: u64, data: &[u8]) -> Vec<u8> {
    let n = (data.len() as i64).to_le_bytes();
    let mut r = Vec::with_capacity(53);
    r.extend_from_slice(&(offset as i64).to_le_bytes());
    r.extend_from_slice(&n);
    r.extend_from_slice(&n);
    r.extend_from_slice(&0i32.to_le_bytes());
    r.extend_from_slice(&sha1(data).0);
    r.push(0);
    r.extend_from_slice(&0u32.to_le_bytes());
    r
}

/// A version 4 pak of uncompressed entries. `files` are names below `mount`.
pub(crate) fn build(mount: &str, files: &[(&str, &[u8])]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let mut index = Vec::new();
    fstring(&mut index, mount)?;
    index.extend_from_slice(&(files.len() as i32).to_le_bytes());
    for (name, data) in files {
        let offset = body.len() as u64;
        // the record in front of the data has offset 0, the index record has the real one
        body.extend_from_slice(&record(0, data));
        body.extend_from_slice(data);
        fstring(&mut index, name)?;
        index.extend_from_slice(&record(offset, data));
    }
    let mut out = body;
    let index_at = out.len() as i64;
    out.extend_from_slice(&index);
    out.push(0);
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&4i32.to_le_bytes());
    out.extend_from_slice(&index_at.to_le_bytes());
    out.extend_from_slice(&(index.len() as i64).to_le_bytes());
    out.extend_from_slice(&sha1(&index).0);
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nxtitle::RomfsEntry;
    use std::collections::BTreeMap;

    /// A RomFS in memory: path to bytes.
    #[derive(Default)]
    pub(crate) struct Mem(pub BTreeMap<String, Vec<u8>>);

    impl RomfsRead for Mem {
        fn list(&self, dir: &str) -> Result<Option<Vec<RomfsEntry>>, String> {
            let d = dir.trim_matches('/');
            let prefix = if d.is_empty() {
                String::new()
            } else {
                format!("{d}/")
            };
            let mut out: Vec<RomfsEntry> = Vec::new();
            for k in self.0.keys().filter_map(|k| k.strip_prefix(&prefix)) {
                let (name, dir) = match k.split_once('/') {
                    Some((n, _)) => (n, true),
                    None => (k, false),
                };
                if !out.iter().any(|e| e.name == name) {
                    out.push(RomfsEntry {
                        name: name.into(),
                        dir,
                    });
                }
            }
            Ok((!out.is_empty()).then_some(out))
        }

        fn len(&self, path: &str) -> Result<Option<u64>, String> {
            Ok(self
                .0
                .get(path.trim_start_matches('/'))
                .map(|d| d.len() as u64))
        }

        fn read_range(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, String> {
            let d = self.0.get(path.trim_start_matches('/')).ok_or("no file")?;
            let o = offset as usize;
            d.get(o..o + len).map(<[u8]>::to_vec).ok_or("range".into())
        }
    }

    #[test]
    fn a_pak_reads_back() {
        let pak = build(
            "../../../",
            &[("G/Config/a.ini", b"hello"), ("G/b.bin", &[1, 2, 3])],
        )
        .unwrap();
        let mem = Mem([("G/Content/Paks/G-Switch.pak".to_string(), pak)].into());
        let path = "G/Content/Paks/G-Switch.pak";
        let es = read_index(&mem, path).unwrap();
        let names: Vec<_> = es.iter().map(|e| e.path()).collect();
        assert_eq!(names, ["G/Config/a.ini", "G/b.bin"]);
        assert_eq!(es[0].name, "../../../G/Config/a.ini");
        assert_eq!(read_entry(&mem, path, &es[0]).unwrap(), b"hello");
        assert_eq!(read_entry(&mem, path, &es[1]).unwrap(), [1, 2, 3]);
    }

    #[test]
    fn the_layout_is_the_one_of_the_reference_writer() {
        let pak = build("../../../", &[("a", b"x")]).unwrap();
        // entry: 53 byte record and 1 byte of data, then the index: mount (4+10), count (4), name (4+2), record (53)
        let index_len = 14 + 4 + 6 + 53;
        assert_eq!(pak.len(), 54 + index_len + 45);
        let f = &pak[pak.len() - 45..];
        assert_eq!(f[0], 0);
        assert_eq!(u32::from_le_bytes(f[1..5].try_into().unwrap()), MAGIC);
        assert_eq!(u32::from_le_bytes(f[5..9].try_into().unwrap()), 4);
        assert_eq!(u64::from_le_bytes(f[9..17].try_into().unwrap()), 54);
        assert_eq!(
            u64::from_le_bytes(f[17..25].try_into().unwrap()),
            index_len as u64
        );
        assert_eq!(f[25..], sha1(&pak[54..54 + index_len]).0);
        assert_eq!(&pak[..8], &[0; 8]);
        assert_eq!(&pak[53..54], b"x");
    }

    /// A version 3 pak with one zlib entry of two blocks.
    #[test]
    fn a_compressed_entry_inflates() {
        let data = b"abcdefghij".repeat(10);
        let blocks: Vec<Vec<u8>> = data
            .chunks(60)
            .map(|c| nxb_sys::zlib::deflate_bytes(c, 6, 15).unwrap())
            .collect();
        let hdr = 8 * 3 + 4 + 20 + 4 + 16 * blocks.len() + 1 + 4;
        let mut body = vec![0u8; hdr];
        let mut spans = Vec::new();
        for b in &blocks {
            spans.push((body.len() as i64, (body.len() + b.len()) as i64));
            body.extend_from_slice(b);
        }
        let mut rec = Vec::new();
        rec.extend_from_slice(&0i64.to_le_bytes());
        rec.extend_from_slice(&((body.len() - hdr) as i64).to_le_bytes());
        rec.extend_from_slice(&(data.len() as i64).to_le_bytes());
        rec.extend_from_slice(&1i32.to_le_bytes());
        rec.extend_from_slice(&[0; 20]);
        rec.extend_from_slice(&(spans.len() as i32).to_le_bytes());
        for (s, e) in &spans {
            rec.extend_from_slice(&s.to_le_bytes());
            rec.extend_from_slice(&e.to_le_bytes());
        }
        rec.push(0);
        rec.extend_from_slice(&60u32.to_le_bytes());
        assert_eq!(rec.len(), hdr);
        let mut index = Vec::new();
        fstring(&mut index, "../../../").unwrap();
        index.extend_from_slice(&1i32.to_le_bytes());
        fstring(&mut index, "G/c.txt").unwrap();
        index.extend_from_slice(&rec);
        let mut pak = body.clone();
        pak.extend_from_slice(&index);
        pak.push(0);
        pak.extend_from_slice(&MAGIC.to_le_bytes());
        pak.extend_from_slice(&3i32.to_le_bytes());
        pak.extend_from_slice(&(body.len() as i64).to_le_bytes());
        pak.extend_from_slice(&(index.len() as i64).to_le_bytes());
        pak.extend_from_slice(&[0; 20]);
        let mem = Mem([("p.pak".to_string(), pak)].into());
        let es = read_index(&mem, "p.pak").unwrap();
        assert_eq!(read_entry(&mem, "p.pak", &es[0]).unwrap(), data);
    }

    #[test]
    fn an_encrypted_index_is_refused() {
        let mut pak = build("../", &[("a", b"x")]).unwrap();
        let n = pak.len();
        pak[n - 45] = 1;
        let mem = Mem([("p.pak".to_string(), pak)].into());
        assert!(read_index(&mem, "p.pak").unwrap_err().contains("encrypted"));
    }
}
