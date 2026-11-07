//! `nn::mii::Database`: the Mii database of the console. Source: nxnative `dm/host/hle_mii.c`.
//!
//! The host database holds no Mii of the player. It serves the six default Miis (three male, three female), reports
//! itself empty, and cancels the Mii editor. `GetIndex` and `UpdateLatest` fail with "not found", and `Import` and
//! `Export` fail because they need the test mode of the console. The `CharInfo` layout is 0x58 bytes. The offsets are
//! from the `nn::mii::CharInfo` layout in nxnative (`mii_charinfo`). The default Mii has the face values of that
//! table, the nickname "no name", and the create ID of a UUID. A default Mii has a fixed ID per index and a random
//! Mii has a new one each time.
//!
//! The Mii model system data (textures and shapes) is a stand-in that a file system serves, so it belongs to the
//! `fs` crate and not here.

use super::{log_once, nothing, ok, put_bytes, put_i32};
use crate::sys::monotonic_ns;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const NOT_FOUND: u32 = 0x87e;
const CANCELED: u32 = 0xe7e;
const TEST_MODE: u32 = 0x1987e;
const CHAR_INFO: usize = 0x58;
const SOURCE_DEFAULT: u32 = 2;
const DEFAULTS: i32 = 6;

/// `(offset, value)` of the face of the default Mii.
const FACE: [(usize, u8); 36] = [
    (41, 64),
    (42, 64),
    (49, 33),
    (50, 1),
    (52, 2),
    (53, 8),
    (54, 4),
    (55, 3),
    (56, 4),
    (57, 2),
    (58, 12),
    (59, 6),
    (60, 1),
    (61, 4),
    (62, 3),
    (63, 6),
    (64, 2),
    (65, 10),
    (66, 1),
    (67, 4),
    (68, 9),
    (69, 23),
    (70, 19),
    (71, 4),
    (72, 3),
    (73, 13),
    (74, 8),
    (77, 4),
    (78, 10),
    (80, 8),
    (81, 4),
    (82, 10),
    (84, 4),
    (85, 2),
    (86, 20),
    (39, 0),
];

static ENTROPY: AtomicU64 = AtomicU64::new(0);

/// A 64-bit value that differs per call: the monotonic clock and a counter through a splitmix step.
fn entropy() -> u64 {
    let n = ENTROPY.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    let mut z = monotonic_ns().wrapping_add(n);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The `CharInfo` of a default Mii. `index` is the default number, or -1 for a random create ID.
fn default_mii(female: bool, index: i32) -> [u8; CHAR_INFO] {
    let mut c = [0u8; CHAR_INFO];
    let (a, b) = if index >= 0 {
        (0x4d49_494e_4445_4641u64 + index as u64, 0x554c_5444_4d48_4f53u64)
    } else {
        (entropy(), entropy())
    };
    c[..8].copy_from_slice(&a.to_le_bytes());
    c[8..16].copy_from_slice(&b.to_le_bytes());
    c[6] = (c[6] & 0x0f) | 0x40;
    c[8] = (c[8] & 0x3f) | 0x80;
    for (i, ch) in "no name".bytes().enumerate() {
        c[16 + 2 * i] = ch;
    }
    for (off, v) in FACE {
        c[off] = v;
    }
    if index >= 0 {
        c[39] = (index * 2) as u8;
    }
    c[40] = u8::from(female);
    c
}

/// `Initialize()`: the six default Miis, none of the console's own.
extern "C" fn initialize(_this: u64) -> u32 {
    log_once(&INIT, "mii: Database::Initialize (the six default Miis, none of the console's own)");
    0
}

static INIT: AtomicBool = AtomicBool::new(false);

extern "C" fn count(_this: u64, source: u32) -> i32 {
    if source & SOURCE_DEFAULT != 0 { DEFAULTS } else { 0 }
}

/// Writes the default Miis. `stride` is the size of one element, and an element of `CharInfoElement` ends with the
/// source (1 for default).
fn get(out: u64, buf: u64, n: i32, source: u32, stride: u64) -> u32 {
    let mut k = 0;
    if source & SOURCE_DEFAULT != 0 && buf != 0 {
        while k < n && k < DEFAULTS {
            let at = buf + k as u64 * stride;
            put_bytes(at, &default_mii(k >= 3, k));
            if stride > CHAR_INFO as u64 {
                put_bytes(at + CHAR_INFO as u64, &1u32.to_le_bytes());
            }
            k += 1;
        }
    }
    put_i32(out, k);
    0
}

extern "C" fn get_element(_this: u64, out: u64, buf: u64, n: u32, source: u32) -> u32 {
    get(out, buf, n as i32, source, CHAR_INFO as u64 + 4)
}

extern "C" fn get_info(_this: u64, out: u64, buf: u64, n: u32, source: u32) -> u32 {
    get(out, buf, n as i32, source, CHAR_INFO as u64)
}

extern "C" fn get_index(_this: u64, out: u64, _info: u64) -> u32 {
    put_i32(out, -1);
    NOT_FOUND
}

extern "C" fn not_found() -> u32 {
    NOT_FOUND
}

/// `BuildRandom(CharInfo*, const RandomDesc&)`: the second word of the description is the gender.
extern "C" fn build_random(_this: u64, out: u64, desc: u64) -> u32 {
    let gender = if desc == 0 { 0 } else { super::get_i32(desc + 4) };
    put_bytes(out, &default_mii(gender == 1, -1));
    0
}

/// `BuildDefault(CharInfo*, int index)`.
extern "C" fn build_default(_this: u64, out: u64, index: u32) -> u32 {
    let index = index as i32;
    let i = if (0..DEFAULTS).contains(&index) { index } else { 0 };
    put_bytes(out, &default_mii(index >= 3, i));
    0
}

extern "C" fn test_mode() -> u32 {
    TEST_MODE
}

extern "C" fn canceled() -> u32 {
    CANCELED
}

table! {
    "_ZN2nn3mii8Database10InitializeEv" => initialize,
    "_ZN2nn3mii8Database8FinalizeEv" => nothing,
    "_ZNK2nn3mii8Database11IsAvailableEv" => super::one,
    "_ZN2nn3mii8Database9IsUpdatedEi" => super::zero,
    "_ZNK2nn3mii8Database14IsFullDatabaseEv" => super::zero,
    "_ZNK2nn3mii8Database8GetCountEi" => count,
    "_ZN2nn3mii8Database3GetEPiPNS0_15CharInfoElementEii" => get_element,
    "_ZN2nn3mii8Database3GetEPiPNS0_8CharInfoEii" => get_info,
    "_ZNK2nn3mii8Database8GetIndexEPiRKNS0_8CharInfoE" => get_index,
    "_ZNK2nn3mii8Database12UpdateLatestEPNS0_8CharInfoEi" => not_found,
    "_ZN2nn3mii8Database11BuildRandomEPNS0_8CharInfoERKNS0_10RandomDescE" => build_random,
    "_ZNK2nn3mii8Database12BuildDefaultEPNS0_8CharInfoEi" => build_default,
    "_ZN2nn3mii8Database6FormatEv" => ok,
    "_ZN2nn3mii8Database6ImportEPKvm" => test_mode,
    "_ZNK2nn3mii8Database6ExportEPvm" => test_mode,
    "_ZN2nn3mii9AppendMiiEPi" => canceled,
    "_ZN2nn3mii11ShowMiiEditEv" => ok,
}
