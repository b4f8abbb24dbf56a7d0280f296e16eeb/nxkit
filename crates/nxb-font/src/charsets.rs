//! The character sets of types 0 to 4, the bidi mirror pairs and the Unicode blocks of the OS/2 range bits, read from
//! `data/charsets.txt` (made by `tools/gen_font_charsets.py`).

use crate::{Error, Result};
use std::collections::BTreeMap;

static TEXT: &str = include_str!("../data/charsets.txt");

/// The parsed data file.
pub(crate) struct Data {
    /// Inclusive code point ranges of each type.
    pub(crate) types: Vec<Vec<(u32, u32)>>,
    /// A character and its bidi mirror.
    pub(crate) mirrors: BTreeMap<u32, u32>,
    /// Start, end and ulUnicodeRange bit of each block, sorted by start.
    pub(crate) blocks: Vec<(u32, u32, u32)>,
}

fn hex(s: &str) -> Result<u32> {
    u32::from_str_radix(s, 16).map_err(|_| Error::new(format!("charsets.txt: bad number {s:?}")))
}

fn range(s: &str) -> Result<(u32, u32)> {
    match s.split_once('-') {
        Some((a, b)) => Ok((hex(a)?, hex(b)?)),
        None => hex(s).map(|a| (a, a)),
    }
}

impl Data {
    pub(crate) fn load() -> Result<Data> {
        let mut d = Data {
            types: Vec::new(),
            mirrors: BTreeMap::new(),
            blocks: Vec::new(),
        };
        let mut part = "";
        for line in TEXT
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            if line.starts_with("type ") || line == "mirror" || line == "os2" {
                part = line;
                if line.starts_with("type ") {
                    d.types.push(Vec::new());
                }
                continue;
            }
            match part {
                "mirror" => {
                    for w in line.split(' ') {
                        let (a, b) = w
                            .split_once(':')
                            .ok_or_else(|| Error::new(format!("charsets.txt: bad pair {w:?}")))?;
                        d.mirrors.insert(hex(a)?, hex(b)?);
                    }
                }
                "os2" => {
                    let (r, bit) = line
                        .split_once(' ')
                        .ok_or_else(|| Error::new(format!("charsets.txt: bad block {line:?}")))?;
                    let (a, b) = range(r)?;
                    let bit = bit
                        .parse()
                        .map_err(|_| Error::new(format!("charsets.txt: bad bit {bit:?}")))?;
                    d.blocks.push((a, b, bit));
                }
                _ => {
                    let set = d
                        .types
                        .last_mut()
                        .ok_or_else(|| Error::new("charsets.txt: a range before the first type"))?;
                    for w in line.split(' ') {
                        set.push(range(w)?);
                    }
                }
            }
        }
        if d.types.len() != 5 {
            return Err(Error::new("charsets.txt: not five types"));
        }
        Ok(d)
    }

    /// The ulUnicodeRange bits (bit 0 is bit 0 of ulUnicodeRange1) of the blocks that hold a code point of `codes`.
    pub(crate) fn unicode_ranges(&self, codes: impl Iterator<Item = u32>) -> u128 {
        let mut bits = 0u128;
        for c in codes {
            let i = self.blocks.partition_point(|b| b.0 <= c);
            if i > 0 && c <= self.blocks[i - 1].1 {
                bits |= 1 << self.blocks[i - 1].2;
            }
            if (0x10000..0x110000).contains(&c) {
                bits |= 1 << 57;
            }
        }
        bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_has_the_five_sets() {
        let d = Data::load().unwrap();
        let has = |t: usize, c: u32| d.types[t].iter().any(|&(a, b)| a <= c && c <= b);
        // 辻 (JIS, not GB 2312, Big5 or KS X 1001), 们 (GB 2312), 丂 (GBK only), 們 (Big5), 한 (Hangul)
        assert!(has(0, 0x8fbb) && !has(1, 0x8fbb) && !has(3, 0x8fbb) && !has(4, 0x8fbb));
        assert!(has(1, 0x4eec) && !has(2, 0x4eec));
        assert!(has(2, 0x4e02) && !has(1, 0x4e02));
        assert!(has(3, 0x5011));
        assert!(has(4, 0xd55c) && !has(0, 0xd55c));
        assert_eq!(d.mirrors.get(&0x2208), Some(&0x220b));
        assert_eq!(
            d.unicode_ranges([0x41, 0xe000, 0x410].into_iter()),
            1 | 1 << 9 | 1 << 60
        );
    }
}
