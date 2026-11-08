//! Where a member of a little-endian SARC archive lies, so that it can be changed in place.
//!
//! A SARC is a header (`SARC`, header length, byte order mark, file size, data offset), the SFAT table (`SFAT`,
//! header length, entry count, hash key, then 16 bytes per entry: name hash, attributes, data start, data end), the
//! SFNT name table and the data. Member offsets are relative to the data offset. An attribute with a nonzero top
//! byte names the member: its low 24 bits are the name's offset in the table, in 4-byte units.

use std::ops::Range;

fn u16le(d: &[u8], o: usize) -> Option<usize> {
    d.get(o..o + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
}

fn u32le(d: &[u8], o: usize) -> Option<usize> {
    d.get(o..o + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// The byte range of the member `name` in the archive `d`. `Err` says what is wrong, also for a missing member.
pub fn member(d: &[u8], name: &str) -> Result<Range<usize>, String> {
    if d.get(..4) != Some(b"SARC".as_slice()) {
        return Err("not a SARC archive".into());
    }
    if d.get(6..8) != Some([0xff, 0xfe].as_slice()) {
        return Err("a big-endian SARC archive".into());
    }
    let hl = u16le(d, 4).ok_or("SARC header")?;
    let data = u32le(d, 12).ok_or("SARC header")?;
    if d.get(hl..hl + 4) != Some(b"SFAT".as_slice()) {
        return Err("no SFAT table".into());
    }
    let n = u16le(d, hl + 6).ok_or("SFAT header")?;
    let names = hl + 12 + 16 * n + 8;
    if names > data || data > d.len() {
        return Err("SARC tables overlap the data".into());
    }
    for i in 0..n {
        let e = hl + 12 + 16 * i;
        let attr = u32le(d, e + 4).ok_or("SFAT entry")?;
        if attr >> 24 == 0 {
            continue;
        }
        let at = names + (attr & 0xff_ffff) * 4;
        let rest = d.get(at..data).unwrap_or(&[]);
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        if &rest[..end] != name.as_bytes() {
            continue;
        }
        let (s, en) = (
            u32le(d, e + 8).ok_or("SFAT entry")?,
            u32le(d, e + 12).ok_or("SFAT entry")?,
        );
        if s > en || data + en > d.len() {
            return Err(format!("SARC member {name} lies outside the archive"));
        }
        return Ok(data + s..data + en);
    }
    Err(format!("the archive has no member {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SARC with members `a` (`xyz`) and `bb` (`12345678`).
    fn sample() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(b"SARC");
        d.extend_from_slice(&0x14u16.to_le_bytes());
        d.extend_from_slice(&[0xff, 0xfe]);
        d.extend_from_slice(&[0; 4]); // file size, unused here
        d.extend_from_slice(&0u32.to_le_bytes()); // data offset, set below
        d.extend_from_slice(&[0; 4]);
        d.extend_from_slice(b"SFAT");
        d.extend_from_slice(&0xcu16.to_le_bytes());
        d.extend_from_slice(&2u16.to_le_bytes());
        d.extend_from_slice(&0x65u32.to_le_bytes());
        for (attr, s, e) in [(0x0100_0000u32, 0u32, 3u32), (0x0100_0001, 8, 16)] {
            d.extend_from_slice(&[0; 4]);
            d.extend_from_slice(&attr.to_le_bytes());
            d.extend_from_slice(&s.to_le_bytes());
            d.extend_from_slice(&e.to_le_bytes());
        }
        d.extend_from_slice(b"SFNT");
        d.extend_from_slice(&8u16.to_le_bytes());
        d.extend_from_slice(&[0; 2]);
        d.extend_from_slice(b"a\0\0\0bb\0\0");
        let data = d.len() as u32;
        d[12..16].copy_from_slice(&data.to_le_bytes());
        d.extend_from_slice(b"xyz\0\0\0\0\x0012345678");
        d
    }

    #[test]
    fn members_are_found_by_name() {
        let d = sample();
        assert_eq!(&d[member(&d, "a").unwrap()], b"xyz");
        assert_eq!(&d[member(&d, "bb").unwrap()], b"12345678");
        assert!(member(&d, "b").is_err());
        assert!(member(b"SARD", "a").is_err());
    }
}
