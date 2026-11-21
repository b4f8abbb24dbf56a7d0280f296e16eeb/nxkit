//! The sfnt container: the table directory of a TrueType font (or of the first font of a collection), big-endian
//! readers, and the writer that puts the tables of a new font together with their checksums.

use crate::{Error, Result};

/// A checked big-endian reader over a byte slice.
#[derive(Clone, Copy)]
pub(crate) struct Bytes<'a>(pub(crate) &'a [u8]);

impl<'a> Bytes<'a> {
    pub(crate) fn slice(self, at: usize, len: usize) -> Result<&'a [u8]> {
        at.checked_add(len)
            .and_then(|end| self.0.get(at..end))
            .ok_or_else(|| Error::new(format!("read of {len} bytes at {at} past the end")))
    }

    pub(crate) fn u8(self, at: usize) -> Result<u8> {
        Ok(self.slice(at, 1)?[0])
    }

    pub(crate) fn u16(self, at: usize) -> Result<u16> {
        let b = self.slice(at, 2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub(crate) fn i16(self, at: usize) -> Result<i16> {
        Ok(self.u16(at)? as i16)
    }

    pub(crate) fn u32(self, at: usize) -> Result<u32> {
        let b = self.slice(at, 4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// A parsed font: its bytes and the place of each table.
pub(crate) struct Font<'a> {
    data: &'a [u8],
    tables: Vec<([u8; 4], usize, usize)>,
}

impl<'a> Font<'a> {
    /// Reads the table directory. A collection (`ttcf`) gives its first font, as fontTools' `fontNumber=0`.
    pub(crate) fn parse(data: &'a [u8]) -> Result<Font<'a>> {
        let b = Bytes(data);
        let mut at = 0;
        if b.slice(0, 4)? == b"ttcf" {
            at = b.u32(12)? as usize;
        }
        let version = b.u32(at)?;
        if version != 0x0001_0000 && version != u32::from_be_bytes(*b"true") {
            return Err(Error::new(format!(
                "not a TrueType font (sfnt version {version:#010x})"
            )));
        }
        let count = b.u16(at + 4)? as usize;
        let mut tables = Vec::with_capacity(count);
        for i in 0..count {
            let rec = at + 12 + 16 * i;
            let tag: [u8; 4] = b.slice(rec, 4)?.try_into().unwrap_or_default();
            let off = b.u32(rec + 8)? as usize;
            let len = b.u32(rec + 12)? as usize;
            b.slice(off, len)?;
            tables.push((tag, off, len));
        }
        Ok(Font { data, tables })
    }

    pub(crate) fn table(&self, tag: &[u8; 4]) -> Option<Bytes<'a>> {
        self.tables
            .iter()
            .find(|(t, _, _)| t == tag)
            .map(|&(_, off, len)| Bytes(&self.data[off..off + len]))
    }

    pub(crate) fn need(&self, tag: &[u8; 4]) -> Result<Bytes<'a>> {
        self.table(tag).ok_or_else(|| {
            Error::new(format!(
                "the font has no '{}' table",
                String::from_utf8_lossy(tag)
            ))
        })
    }
}

/// The order of the table data in the file, as fontTools writes it (the tables of the list first, the rest by tag).
const ORDER: [&[u8; 4]; 19] = [
    b"head", b"hhea", b"maxp", b"OS/2", b"hmtx", b"LTSH", b"VDMX", b"hdmx", b"cmap", b"fpgm",
    b"prep", b"cvt ", b"loca", b"glyf", b"kern", b"name", b"post", b"gasp", b"PCLT",
];

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, c| {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        sum.wrapping_add(u32::from_be_bytes(w))
    })
}

fn pad4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

/// Writes a TrueType font from its tables. The directory is sorted by tag, the data follows `ORDER`, every table
/// starts on 4 bytes, and `head.checkSumAdjustment` makes the sum of the file 0xB1B0AFBA.
pub(crate) fn assemble(mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    let rank = |t: &[u8; 4]| ORDER.iter().position(|o| *o == t).unwrap_or(ORDER.len());
    tables.sort_by_key(|a| (rank(&a.0), a.0));
    let n = tables.len();
    let mut power = 1usize;
    let mut selector = 0u16;
    while power * 2 <= n {
        power *= 2;
        selector += 1;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&((power * 16) as u16).to_be_bytes());
    out.extend_from_slice(&selector.to_be_bytes());
    out.extend_from_slice(&((n * 16 - power * 16) as u16).to_be_bytes());
    let mut records = Vec::with_capacity(n);
    let mut body = Vec::new();
    let base = 12 + 16 * n;
    let mut head_at = None;
    for (tag, mut data) in tables {
        if &tag == b"head" && data.len() >= 12 {
            data[8..12].fill(0);
            head_at = Some(base + body.len());
        }
        records.push((tag, checksum(&data), base + body.len(), data.len()));
        body.extend_from_slice(&data);
        pad4(&mut body);
    }
    records.sort_by_key(|r| r.0);
    for (tag, sum, off, len) in records {
        out.extend_from_slice(&tag);
        out.extend_from_slice(&sum.to_be_bytes());
        out.extend_from_slice(&(off as u32).to_be_bytes());
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
    out.extend_from_slice(&body);
    if let Some(at) = head_at {
        let adjust = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out[at + 8..at + 12].copy_from_slice(&adjust.to_be_bytes());
    }
    out
}

/// Big-endian writers for the table builders.
pub(crate) trait Put {
    fn u16(&mut self, v: u16);
    fn i16(&mut self, v: i16);
    fn u32(&mut self, v: u32);
}

impl Put for Vec<u8> {
    fn u16(&mut self, v: u16) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.extend_from_slice(&v.to_be_bytes());
    }
}

/// Overwrites a big-endian u16 at `at`.
pub(crate) fn set_u16(data: &mut [u8], at: usize, v: u16) {
    data[at..at + 2].copy_from_slice(&v.to_be_bytes());
}

/// `hmtx` from (advance, left side bearing) per glyph, and the count of full metrics: a run of equal advances at the
/// end keeps only the first of them, as fontTools compiles it.
pub(crate) fn hmtx(metrics: &[(u16, i16)]) -> (Vec<u8>, u16) {
    let mut last = metrics.len();
    if let Some(&(adv, _)) = metrics.last() {
        while last > 1 && metrics[last - 2].0 == adv {
            last -= 1;
        }
    }
    let mut out = Vec::new();
    for (i, &(adv, lsb)) in metrics.iter().enumerate() {
        if i < last {
            out.u16(adv);
        }
        out.i16(lsb);
    }
    (out, last as u16)
}

/// `post` format 3 or 2 from a 32-byte header (version, italic angle, underline, fixed pitch, memory fields).
pub(crate) fn post(mut head: [u8; 32], names: Option<&[&str]>) -> Vec<u8> {
    let Some(names) = names else {
        head[0..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
        return head.to_vec();
    };
    head[0..4].copy_from_slice(&0x0002_0000u32.to_be_bytes());
    let mut out = head.to_vec();
    out.u16(names.len() as u16);
    let mut extra = Vec::new();
    for n in names {
        if *n == ".notdef" {
            out.u16(0);
        } else {
            out.u16(258 + extra.len() as u16);
            extra.push(*n);
        }
    }
    for n in extra {
        out.push(n.len() as u8);
        out.extend_from_slice(n.as_bytes());
    }
    out
}

/// One record of a `name` table.
pub(crate) struct Name {
    pub(crate) platform: u16,
    pub(crate) encoding: u16,
    pub(crate) language: u16,
    pub(crate) id: u16,
    pub(crate) string: Vec<u8>,
}

/// A `name` table of format 0. The records are sorted by platform, encoding, language and name ID, and equal strings
/// share their storage.
pub(crate) fn name(mut names: Vec<Name>) -> Vec<u8> {
    names.sort_by_key(|n| (n.platform, n.encoding, n.language, n.id));
    let mut out = Vec::new();
    out.u16(0);
    out.u16(names.len() as u16);
    out.u16((6 + 12 * names.len()) as u16);
    let mut storage: Vec<u8> = Vec::new();
    let mut seen: Vec<(&[u8], usize)> = Vec::new();
    for n in &names {
        let off = match seen.iter().find(|(s, _)| *s == n.string.as_slice()) {
            Some(&(_, off)) => off,
            None => {
                let off = storage.len();
                storage.extend_from_slice(&n.string);
                seen.push((&n.string, off));
                off
            }
        };
        out.u16(n.platform);
        out.u16(n.encoding);
        out.u16(n.language);
        out.u16(n.id);
        out.u16(n.string.len() as u16);
        out.u16(off as u16);
    }
    out.extend_from_slice(&storage);
    out
}

/// UTF-16BE, the encoding of Unicode and Windows name records.
pub(crate) fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// `floor(v + 0.5)`, fontTools' `otRound`.
pub(crate) fn ot_round(v: f64) -> i32 {
    (v + 0.5).floor() as i32
}
