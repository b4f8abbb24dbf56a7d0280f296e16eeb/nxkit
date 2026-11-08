//! The decompressed `Movie/Ndve/*.dat` container (section 2 of `docs/plans/ndve-native.md`).
//!
//! This is for tests and tools. At run time the game uploads the weights itself and `nxkit-gpu` reads them from the
//! bound buffers; no game file is read. [`Network::parse`] walks the header and the network tables and finds the
//! weight blobs. The shader bytes are not interpreted.

use std::fmt;

/// The four bytes at the start of the file.
pub const MAGIC: [u8; 4] = *b"NDVE";
/// The version word after the magic.
pub const VERSION: u32 = 0x0ff0_0005;
/// File offset of the first model record.
const MODELS_AT: usize = 0x18;
/// Offset of the resampler's weights from the start of the NDSR pack.
const RESAMPLER_IN_PACK: usize = 0x101f00;

/// A parse error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The file ends before a field or table.
    Truncated,
    /// The first four bytes are not `NDVE`.
    BadMagic,
    /// The version word is not [`VERSION`].
    BadVersion(u32),
    /// A table is not what the format says (the field named).
    BadTable(&'static str),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Truncated => f.write_str("the file is truncated"),
            ParseError::BadMagic => f.write_str("the file does not start with NDVE"),
            ParseError::BadVersion(v) => write!(f, "unknown version {v:#x}"),
            ParseError::BadTable(what) => write!(f, "bad network table: {what}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// A model record: a kind word and the scale factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Model {
    /// 0x01000000 for network A (scale 4/3), 0x01000004 for network B (scale 1.0).
    pub kind: u32,
    /// The scale of the picture the model produces.
    pub scale: f32,
}

/// One shader entry of a table. The offset is relative to the start of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shader {
    /// Offset of the control section from the table start; the code follows it.
    pub offset: u32,
    /// Bytes of the control section.
    pub control_len: u32,
    /// Bytes of the code.
    pub code_len: u32,
}

/// A network table: the weights and the shaders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// File offset of the table.
    pub offset: usize,
    /// File offset of the weight blob.
    pub weights_offset: usize,
    /// Bytes of the weight blob.
    pub weights_len: usize,
    /// The shader entries.
    pub shaders: Vec<Shader>,
}

/// The parsed container.
#[derive(Clone, Debug, PartialEq)]
pub struct Network {
    /// Length of the file.
    pub len: usize,
    /// File offset of the NDSR pack.
    pub pack_offset: usize,
    /// Bytes of the NDSR pack.
    pub pack_len: usize,
    /// The model records.
    pub models: Vec<Model>,
    /// One network table per model, in order (A first).
    pub tables: Vec<Table>,
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, ParseError> {
    let s = b
        .get(at..at.checked_add(4).ok_or(ParseError::Truncated)?)
        .ok_or(ParseError::Truncated)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

impl Network {
    /// Parse `bytes`, the decompressed file.
    pub fn parse(bytes: &[u8]) -> Result<Network, ParseError> {
        if bytes.get(..4).ok_or(ParseError::Truncated)? != MAGIC {
            return Err(ParseError::BadMagic);
        }
        let version = u32_at(bytes, 4)?;
        if version != VERSION {
            return Err(ParseError::BadVersion(version));
        }
        let pack_len = u32_at(bytes, 0xc)? as usize;
        let count = u32_at(bytes, 0x14)? as usize;
        if count > 16 {
            return Err(ParseError::BadTable("model count"));
        }
        let mut models = Vec::with_capacity(count);
        for i in 0..count {
            let at = MODELS_AT + 8 * i;
            models.push(Model {
                kind: u32_at(bytes, at)?,
                scale: f32::from_bits(u32_at(bytes, at + 4)?),
            });
        }
        let pack_offset = MODELS_AT + 8 * count;
        let mut at = pack_offset
            .checked_add(pack_len)
            .ok_or(ParseError::Truncated)?;
        let mut tables = Vec::with_capacity(count);
        for _ in 0..count {
            let table = parse_table(bytes, at)?;
            // The next table starts where the last shader of this one ends.
            at = table
                .shaders
                .iter()
                .map(|s| (s.offset as usize) + (s.control_len as usize) + (s.code_len as usize))
                .max()
                .and_then(|end| table.offset.checked_add(end))
                .ok_or(ParseError::BadTable("shader ends"))?;
            if at > bytes.len() {
                return Err(ParseError::Truncated);
            }
            tables.push(table);
        }
        Ok(Network {
            len: bytes.len(),
            pack_offset,
            pack_len,
            models,
            tables,
        })
    }

    /// The weight blob of table `table` in `bytes`.
    pub fn weights<'a>(&self, bytes: &'a [u8], table: usize) -> Option<&'a [u8]> {
        let t = self.tables.get(table)?;
        bytes.get(t.weights_offset..t.weights_offset.checked_add(t.weights_len)?)
    }

    /// The resampler's weights in `bytes`: 0x3000 bytes at 0x101f00 in the NDSR pack. The position is the one found
    /// in the file of the Ocarina movie (V); the pack's entry table is not decoded. Check the result with
    /// [`crate::fingerprint::weights_ok`].
    pub fn resampler<'a>(&self, bytes: &'a [u8]) -> Option<&'a [u8]> {
        let at = self.pack_offset.checked_add(RESAMPLER_IN_PACK)?;
        bytes.get(at..at.checked_add(crate::net::RESAMPLER_LEN)?)
    }
}

fn parse_table(b: &[u8], at: usize) -> Result<Table, ParseError> {
    let count = u32_at(b, at)? as usize;
    if count == 0 || count > 64 {
        return Err(ParseError::BadTable("shader count"));
    }
    let weights_rel = u32_at(b, at + 8)? as usize;
    let weights_len = u32_at(b, at + 12)? as usize;
    let weights_offset = at.checked_add(weights_rel).ok_or(ParseError::Truncated)?;
    if weights_rel != 0x10 + 16 * count {
        return Err(ParseError::BadTable("weights offset"));
    }
    if weights_offset
        .checked_add(weights_len)
        .is_none_or(|e| e > b.len())
    {
        return Err(ParseError::Truncated);
    }
    let mut shaders = Vec::with_capacity(count);
    for i in 0..count {
        let e = at + 0x10 + 16 * i;
        shaders.push(Shader {
            offset: u32_at(b, e)?,
            control_len: u32_at(b, e + 4)?,
            code_len: u32_at(b, e + 8)?,
        });
    }
    Ok(Table {
        offset: at,
        weights_offset,
        weights_len,
        shaders,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(v: &mut Vec<u8>, w: u32) {
        v.extend_from_slice(&w.to_le_bytes());
    }

    /// A small file: two models, a 4-byte pack and two tables of one shader each.
    fn small() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&MAGIC);
        for w in [VERSION, 0, 4, 0, 2] {
            put(&mut v, w);
        }
        for w in [
            0x0100_0000,
            (4.0f32 / 3.0).to_bits(),
            0x0100_0004,
            1.0f32.to_bits(),
        ] {
            put(&mut v, w);
        }
        put(&mut v, 0x5253_444e);
        for weights in [8u32, 12] {
            let t = v.len();
            for w in [1, 1, 0x20, weights, 0x20 + weights, 6, 10, 0] {
                put(&mut v, w);
            }
            v.resize(t + 0x20 + weights as usize + 16, 7);
        }
        v
    }

    #[test]
    fn parses_a_small_file() {
        let b = small();
        let n = Network::parse(&b).expect("parses");
        assert_eq!(n.models.len(), 2);
        assert_eq!(
            n.models[1],
            Model {
                kind: 0x0100_0004,
                scale: 1.0
            }
        );
        assert_eq!(n.tables.len(), 2);
        assert_eq!(n.tables[0].weights_len, 8);
        assert_eq!(n.weights(&b, 1).map(<[u8]>::len), Some(12));
        assert_eq!(n.weights(&b, 2), None);
    }

    #[test]
    fn rejects_bad_files() {
        let b = small();
        assert_eq!(Network::parse(&b[..3]), Err(ParseError::Truncated));
        assert_eq!(Network::parse(&b[..60]), Err(ParseError::Truncated));
        let mut c = b.clone();
        c[0] = b'X';
        assert_eq!(Network::parse(&c), Err(ParseError::BadMagic));
        let mut c = b;
        c[4] = 9;
        assert!(matches!(Network::parse(&c), Err(ParseError::BadVersion(_))));
    }
}
