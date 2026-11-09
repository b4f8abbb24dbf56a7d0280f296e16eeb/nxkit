//! The checks that a game runs exactly the network of this crate.
//!
//! Two checks, as in section 11 of `docs/plans/ndve-native.md`: the sequence of shader keys of a frame's dispatches
//! ([`matches()`]) and the weight bytes bound to them ([`weights_ok`]). The picture sizes are checked by the caller, which
//! sees the textures.

use nxb_model::{sha256, to_hex};

pub use crate::net::{BLOB_LEN, RESAMPLER_LEN};

/// The shader keys of passes 3 to 16 in order, as the first 8 hex characters of the 16 of a key. The index of a key
/// is the index of the dispatch in the run: 0 is the colour conversion, 1 to 12 are the layers, 13 is the resampler.
pub const KEYS: [&str; 14] = [
    "49dff010b9832251",
    "ba079f68ae288dba",
    "a8b4fb48fee0ccf8",
    "e2eebd7ba18fe35f",
    "e6be82d835fbe4d3",
    "9442573ee4ace011",
    "0a56002cdc7e1fce",
    "0a56002cdc7e1fce",
    "d15370a382630d4e",
    "e6be82d835fbe4d3",
    "7f7ff0f36b54d90f",
    "a8b4fb48fee0ccf8",
    "5a7d69b067a7f011",
    "4f41ccf24f163e0d",
];

/// SHA-256 of the weight blob (hex, lower case).
pub const BLOB_SHA256: &str = "50bac9b08e056e16d297353113368b38acdda167749a094a6db88ecc31dfc023";
/// SHA-256 of the resampler's weights (hex, lower case).
pub const RESAMPLER_SHA256: &str =
    "44af61fdf2f9ab40700eb182308eec328e2e7cefb4bc107fadd44fa40fbc1dab";
/// Length of the decompressed `.dat` file.
pub const FILE_LEN: usize = 2_038_760;
/// SHA-256 of the decompressed `.dat` file (hex, lower case).
pub const FILE_SHA256: &str = "27f940cb2f52b8f8df822b686a2fd4f938d8df4fca9cbf68b0b4bfeeac3e1399";

/// Where the network's run sits in a frame's dispatches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    /// Index of the colour conversion in the list of keys.
    pub start: usize,
    /// Number of dispatches in the run.
    pub len: usize,
}

/// Find the first run of [`KEYS`] in `keys`, the program keys of a frame's compute dispatches in order. A key matches
/// when it equals the key of the table (any case).
pub fn matches(keys: &[&str]) -> Option<Match> {
    let n = KEYS.len();
    if keys.len() < n {
        return None;
    }
    (0..=keys.len() - n)
        .find(|&start| {
            keys[start..start + n]
                .iter()
                .zip(KEYS)
                .all(|(k, want)| k.eq_ignore_ascii_case(want))
        })
        .map(|start| Match { start, len: n })
}

/// The weight blob and the resampler's weights are the ones of the fingerprint: the lengths and the SHA-256 agree.
pub fn weights_ok(blob: &[u8], resampler: &[u8]) -> bool {
    blob.len() == BLOB_LEN
        && resampler.len() == RESAMPLER_LEN
        && to_hex(&sha256(blob).0) == BLOB_SHA256
        && to_hex(&sha256(resampler).0) == RESAMPLER_SHA256
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(prefix: usize, suffix: usize) -> Vec<String> {
        let mut v: Vec<String> = (0..prefix).map(|i| format!("{i:016x}")).collect();
        v.extend(KEYS.iter().map(|k| (*k).to_string()));
        v.extend((0..suffix).map(|i| format!("ffff{i:012x}")));
        v
    }

    #[test]
    fn finds_the_run() {
        for (p, s) in [(0, 0), (3, 0), (0, 5), (7, 2)] {
            let f = frame(p, s);
            let refs: Vec<&str> = f.iter().map(String::as_str).collect();
            assert_eq!(matches(&refs), Some(Match { start: p, len: 14 }), "{p} {s}");
        }
    }

    #[test]
    fn rejects_other_sequences() {
        let f = frame(2, 2);
        let mut refs: Vec<&str> = f.iter().map(String::as_str).collect();
        refs.swap(5, 6);
        assert_eq!(matches(&refs), None);
        assert_eq!(matches(&refs[..10]), None);
        assert_eq!(matches(&[]), None);
        // Short and non-ASCII keys do not panic.
        assert_eq!(matches(&["49d", "é", "49dff010"].repeat(5)), None);
    }

    #[test]
    fn weights_need_the_right_bytes() {
        assert!(!weights_ok(&[], &[]));
        assert!(!weights_ok(&vec![0; BLOB_LEN], &vec![0; RESAMPLER_LEN]));
    }
}
