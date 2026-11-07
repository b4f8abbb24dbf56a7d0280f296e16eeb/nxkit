//! The `nn::hid` functions that the game gets in place of the SDK's own (R14, R20).
//!
//! The list is that of nxnative `hle_hid.c` (MIT). Each function reads the controllers at the moment of the call
//! and answers. A pointer argument is a guest address. A guest read or write that fails is ignored, because an
//! export must not panic.

use nxkit_abi::export::Export;
use nxkit_abi::guest;

/// A list of functions that answer with a fixed value and ignore their arguments, which are only named by their type.
/// `$list` is the generated function that returns the list of exports.
macro_rules! stubs {
    ($list:ident; $($name:literal => $f:ident($($t:ty),*) $(-> $r:ty)? = $v:expr;)*) => {
        $(
            extern "C" fn $f($(_: $t),*) $(-> $r)? {
                $v
            }
        )*

        pub(super) fn $list() -> Vec<Export> {
            vec![$(Export::new($name, $f as usize)),*]
        }
    };
}

mod controller;
mod devices;
mod nmouse;
mod npad;
mod palma;
mod sixaxis;
mod touch;
mod vibration;

pub(crate) use controller::player_led_pattern;
pub(crate) use vibration::vibration_info;

/// The most entries that one state call writes.
const MAX_STATES: usize = 256;

/// A count argument of a state call, limited so that a wrong value cannot make the host write a lot of memory.
fn count(n: i32) -> usize {
    usize::try_from(n).unwrap_or(0).min(MAX_STATES)
}

/// An npad id or a handle that the game passes by reference. An unreadable address gives an id that no npad has.
fn read_ref(addr: u64) -> u32 {
    guest::read_u32(addr).unwrap_or(u32::MAX)
}

/// Writes equal-sized entries one after the other.
fn put_all<const N: usize>(addr: u64, entries: &[[u8; N]]) {
    let flat: Vec<u8> = entries.iter().flatten().copied().collect();
    let _ = guest::write(addr, &flat);
}

/// `n` copies of one state, newest first. The sampling number is the `i64` at the start of the state and counts down
/// from the number of `base`.
fn history<const N: usize>(base: [u8; N], n: usize) -> Vec<[u8; N]> {
    let mut head = [0u8; 8];
    head.copy_from_slice(&base[..8]);
    let newest = i64::from_le_bytes(head);
    (0..n)
        .map(|i| {
            let mut entry = base;
            entry[..8].copy_from_slice(&(newest - i as i64).to_le_bytes());
            entry
        })
        .collect()
}

/// Every function of this service.
pub(crate) fn all() -> Vec<Export> {
    [
        npad::list(),
        controller::list(),
        sixaxis::list(),
        vibration::list(),
        devices::list(),
        touch::list(),
        nmouse::list(),
        palma::list(),
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_name_is_a_switch_symbol_and_appears_once() {
        let list = all();
        let mut seen = HashSet::new();
        for e in &list {
            assert!(e.name.starts_with("_ZN2nn"), "{}", e.name);
            assert_ne!(e.addr, 0, "{}", e.name);
            assert!(seen.insert(e.name), "{} is listed twice", e.name);
        }
        assert!(list.len() >= 140, "only {} functions", list.len());
    }

    #[test]
    fn history_counts_down_from_the_first_sampling_number() {
        let mut base = [0u8; 16];
        base[..8].copy_from_slice(&10i64.to_le_bytes());
        let h = history(base, 3);
        let numbers: Vec<i64> = h.iter().map(|e| i64::from_le_bytes(e[..8].try_into().unwrap_or([0; 8]))).collect();
        assert_eq!(numbers, [10, 9, 8]);
    }

    #[test]
    fn counts_are_limited() {
        assert_eq!(count(-1), 0);
        assert_eq!(count(5), 5);
        assert_eq!(count(i32::MAX), MAX_STATES);
    }
}
