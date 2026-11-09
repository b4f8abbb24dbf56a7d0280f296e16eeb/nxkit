//! The checks that a game runs one of the networks of this crate.
//!
//! As in section 9 of `docs/plans/ndsr-native.md`: the shader key of a network dispatch ([`find`]) and the weight bytes
//! bound to it ([`weights_ok`]). The sizes and formats are checked by the caller, which sees the textures.

use crate::net::{NETS, Net};
use nxb_model::{sha256, to_hex};

/// Find the first dispatch whose program key is a network of the table. `keys` are the program keys of a frame's
/// compute dispatches in order (16 hex characters, any case). Returns the index in `keys` and the table entry.
pub fn find(keys: &[&str]) -> Option<(usize, &'static Net)> {
    keys.iter()
        .enumerate()
        .find_map(|(i, k)| Net::by_key(k).map(|(_, n)| (i, n)))
}

/// The weights bound to the network's dispatch are the ones of the table: the length and the SHA-256 agree.
pub fn weights_ok(net: &Net, blob: &[u8]) -> bool {
    let want = net.blob();
    blob.len() == want.len as usize && to_hex(&sha256(blob).0) == want.sha256
}

/// The number of programs in the table.
pub const fn net_count() -> usize {
    NETS.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_any_key_of_the_table() {
        for (i, n) in NETS.iter().enumerate() {
            let keys = ["f8965bffbdf51d78", "d17eefaff4f0bc23", n.key];
            let (at, got) = find(&keys).unwrap();
            assert_eq!((at, got.key), (2, n.key), "{i}");
        }
    }

    #[test]
    fn any_case_and_first_match() {
        let (at, n) = find(&["zz", "E4B49569547E3D47", "14c10aa70b81f04e"]).unwrap();
        assert_eq!((at, n.key), (1, "e4b49569547e3d47"));
    }

    #[test]
    fn rejects_other_keys() {
        assert!(find(&[]).is_none());
        assert!(find(&["f8965bffbdf51d78", "92918e9b7e2f89a4", "é", "14c1"]).is_none());
    }

    #[test]
    fn weights_need_the_right_bytes() {
        let n = &NETS[22];
        assert!(!weights_ok(n, &[]));
        assert!(!weights_ok(n, &vec![0; 0x3000]));
        assert!(!weights_ok(n, &vec![0; 0x1800]));
        assert_eq!(net_count(), 52);
    }
}
