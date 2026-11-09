//! A minimal protobuf writer: just what the Core ML `Model` and MIL messages need.
//!
//! The field numbers used by [`super::mil`] and [`super::package`] come from the public Core ML format definition
//! (`Model.proto`, `MIL.proto`, `FeatureTypes.proto` of coremltools, BSD-3-Clause; see `THIRD_PARTY.md`).
//! Messages are written in field order and maps in the order the caller adds the entries, so equal input gives equal
//! bytes.

/// A message under construction.
#[derive(Clone, Debug, Default)]
pub struct Msg(Vec<u8>);

const VARINT: u64 = 0;
const LEN: u64 = 2;

impl Msg {
    /// An empty message.
    #[must_use]
    pub fn new() -> Msg {
        Msg(Vec::new())
    }

    fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push((v & 0x7f) as u8 | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }

    fn tag(&mut self, field: u64, wire: u64) {
        self.varint(field << 3 | wire);
    }

    /// A varint field (`uint32`, `uint64`, `int32` of a non-negative value, `enum`). Zero is written too: callers that
    /// want proto3 defaults omitted do not call this.
    pub fn uint(&mut self, field: u64, v: u64) -> &mut Msg {
        self.tag(field, VARINT);
        self.varint(v);
        self
    }

    /// A length-delimited field of raw bytes.
    pub fn bytes(&mut self, field: u64, b: &[u8]) -> &mut Msg {
        self.tag(field, LEN);
        self.varint(b.len() as u64);
        self.0.extend_from_slice(b);
        self
    }

    /// A string field.
    pub fn string(&mut self, field: u64, s: &str) -> &mut Msg {
        self.bytes(field, s.as_bytes())
    }

    /// A nested message field.
    pub fn msg(&mut self, field: u64, m: &Msg) -> &mut Msg {
        self.bytes(field, &m.0)
    }

    /// A packed repeated field of non-negative varints.
    pub fn packed(&mut self, field: u64, values: &[u64]) -> &mut Msg {
        let mut inner = Msg::new();
        for &v in values {
            inner.varint(v);
        }
        self.bytes(field, &inner.0)
    }

    /// A packed repeated `int32` field (negative values are sign-extended to ten bytes, as protobuf says).
    pub fn packed_i32(&mut self, field: u64, values: &[i32]) -> &mut Msg {
        let mut inner = Msg::new();
        for &v in values {
            inner.varint(i64::from(v) as u64);
        }
        self.bytes(field, &inner.0)
    }

    /// A `map<string, Message>` entry (`key` is field 1, `value` field 2).
    pub fn map_entry(&mut self, field: u64, key: &str, value: &Msg) -> &mut Msg {
        let mut e = Msg::new();
        e.string(1, key).msg(2, value);
        self.msg(field, &e)
    }

    /// Append the fields of `other` (concatenating two encodings is merging the messages).
    pub fn merge(&mut self, other: &Msg) -> &mut Msg {
        self.0.extend_from_slice(&other.0);
        self
    }

    /// The encoded bytes.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.0
    }

    /// The encoded bytes without consuming the message.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_fields() {
        let mut m = Msg::new();
        m.uint(1, 300).string(2, "hi").packed(3, &[1, 2, 128]);
        assert_eq!(
            m.finish(),
            [
                0x08, 0xac, 0x02, 0x12, 2, b'h', b'i', 0x1a, 4, 1, 2, 0x80, 0x01
            ]
        );
    }

    #[test]
    fn negative_int32_takes_ten_bytes() {
        let mut m = Msg::new();
        m.packed_i32(1, &[-1]);
        let b = m.finish();
        assert_eq!(b.len(), 2 + 10);
        assert_eq!(&b[..2], &[0x0a, 10]);
    }

    #[test]
    fn map_entry_nests_key_and_value() {
        let mut v = Msg::new();
        v.uint(1, 5);
        let mut m = Msg::new();
        m.map_entry(4, "k", &v);
        assert_eq!(m.finish(), [0x22, 7, 0x0a, 1, b'k', 0x12, 2, 0x08, 5]);
    }
}
