//! The stand-ins for the console's six shared fonts, which `nxb build` puts into every app.
//!
//! The console fonts are not in the game, and a game draws button icons and some system text with them. Types 0 to 4
//! (standard, Simplified Chinese, its extension, Traditional Chinese, Korean) are unhinted cuts of one source font
//! (macOS's Arial Unicode), each with the characters of its type. Type 5 (the Nintendo extension) holds the controller
//! button icons at the console's private-use code points, drawn with the letters of a second font (macOS's Arial
//! Rounded Bold). The output is that of NX2Apple's `make_shared_fonts.py` and `make_ext_font.py` (refer to
//! THIRD_PARTY.md). The crate does no I/O: it takes the bytes of the two source fonts and gives the bytes of the six
//! fonts.

mod charsets;
mod cmap;
mod ext;
mod glyph;
mod pen;
mod sfnt;
mod subset;

use std::fmt;

/// A font that cannot be read or made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    fn new(s: impl Into<String>) -> Self {
        Error(s.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

/// The macOS font that types 0 to 4 are cut from.
pub const SOURCE: &str = "/System/Library/Fonts/Supplemental/Arial Unicode.ttf";
/// The macOS font that the letters of the button icons come from.
pub const LETTERS: &str = "/System/Library/Fonts/Supplemental/Arial Rounded Bold.ttf";

/// The file of each type (the names that `nxkit-svc-system` reads) and, for types 0 to 4, the name of the cut.
pub const TYPES: [(&str, &str); 6] = [
    ("standard.ttf", "DM Standard"),
    ("chinese_simplified.ttf", "DM Chinese Simplified"),
    ("chinese_simplified_ext.ttf", "DM Chinese Simplified Ext"),
    ("chinese_traditional.ttf", "DM Chinese Traditional"),
    ("korean.ttf", "DM Korean"),
    ("nintendo_ext.ttf", "DM Nintendo Extension"),
];

/// One made font.
pub struct SharedFont {
    /// The shared font type, 0 to 5.
    pub kind: usize,
    /// The file name, from [`TYPES`].
    pub file: &'static str,
    /// The TrueType font.
    pub bytes: Vec<u8>,
    /// The count of characters the font maps.
    pub chars: usize,
}

/// The six fonts from the bytes of the source font and of the letter font, in type order.
pub fn shared_fonts(source: &[u8], letters: &[u8]) -> std::result::Result<Vec<SharedFont>, Error> {
    let data = charsets::Data::load()?;
    let mut out = Vec::with_capacity(6);
    for (kind, set) in data.types.iter().enumerate() {
        let (file, name) = TYPES[kind];
        let (bytes, chars) = subset::cut(source, set, name, &data)?;
        out.push(SharedFont {
            kind,
            file,
            bytes,
            chars,
        });
    }
    let (bytes, chars) = ext::build(letters, &data)?;
    out.push(SharedFont {
        kind: 5,
        file: TYPES[5].0,
        bytes,
        chars,
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sfnt::Font;

    /// The six fonts from the macOS system fonts, or `None` (with a note) on a machine without them.
    fn made() -> Option<Vec<SharedFont>> {
        match (std::fs::read(SOURCE), std::fs::read(LETTERS)) {
            (Ok(s), Ok(l)) => Some(shared_fonts(&s, &l).unwrap()),
            _ => {
                eprintln!("skipped: the macOS system fonts are missing");
                None
            }
        }
    }

    fn cmap(f: &SharedFont) -> std::collections::BTreeMap<u32, u16> {
        cmap::read(Font::parse(&f.bytes).unwrap().need(b"cmap").unwrap()).unwrap()
    }

    #[test]
    fn the_cuts_have_no_hinting_and_the_characters_of_their_type() {
        let Some(fonts) = made() else { return };
        for f in &fonts[..5] {
            let font = Font::parse(&f.bytes).unwrap();
            for tag in [b"fpgm", b"prep", b"cvt ", b"hdmx"] {
                assert!(font.table(tag).is_none(), "{} has {tag:?}", f.file);
            }
            let maxp = font.need(b"maxp").unwrap();
            assert_eq!(
                maxp.u16(26).unwrap(),
                0,
                "{}: maxSizeOfInstructions",
                f.file
            );
            assert!(cmap(f).contains_key(&0x41), "{} lacks A", f.file);
        }
        let has = |k: usize, c: u32| cmap(&fonts[k]).contains_key(&c);
        // 辻 in the standard font, 们 in Simplified Chinese, 丂 in its extension, 們 in Traditional, 한 in Korean
        assert!(has(0, 0x8fbb) && !has(4, 0x8fbb));
        assert!(has(1, 0x4eec) && !has(2, 0x4eec));
        assert!(has(2, 0x4e02) && !has(1, 0x4e02));
        assert!(has(3, 0x5011) && !has(0, 0xd55c));
        assert!(has(4, 0xd55c));
    }

    #[test]
    fn the_extension_font_maps_the_button_icons() {
        let Some(fonts) = made() else { return };
        let map = cmap(&fonts[5]);
        assert_eq!(map.len(), 512);
        assert_eq!(fonts[5].chars, 512);
        // A at both of its code points, ZL and ZR, plus, and a code point without an icon (the ring)
        assert_eq!(map[&0xe0a0], map[&0xe0e0]);
        let ring = map[&0xe000];
        for c in [0xe0e0, 0xe0e6, 0xe0e7, 0xe0ef] {
            assert_ne!(map[&c], ring, "{c:#x}");
        }
        assert_ne!(map[&0xe0e6], map[&0xe0e7]);
    }
}
