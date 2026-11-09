//! Facts from the program's `main.npdm` that the build does not copy into `game.json`.
//!
//! The NPDM starts with a `META` header; at 0x70 it gives the offset and size of the `ACI0` block, whose kernel
//! capabilities (offset and size at 0x30 and 0x34 of `ACI0`) are 32-bit words. A word whose low four bits are 0b0111
//! is the thread info capability: lowest and highest priority, then the lowest and highest core the process may use.

use crate::manifest::Game;

/// The cores the program may use, one bit per core, from the thread info capability of its NPDM. `None` when the
/// game has no NPDM or it has no such capability.
pub fn core_mask(game: &Game) -> Option<u64> {
    let path = game.npdm.as_ref()?;
    let bytes = std::fs::read(game.path(path)).ok()?;
    let mask = core_mask_of(&bytes)?;
    eprintln!("nxkit-host: the NPDM allows cores {mask:#b}");
    Some(mask)
}

/// [`core_mask`] over the bytes of an NPDM.
pub fn core_mask_of(npdm: &[u8]) -> Option<u64> {
    let u32_at = |b: &[u8], o: usize| {
        b.get(o..o + 4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
    };
    if npdm.get(0..4)? != b"META" {
        return None;
    }
    let aci_off = u32_at(npdm, 0x70)? as usize;
    let aci = npdm.get(aci_off..)?;
    if aci.get(0..4)? != b"ACI0" {
        return None;
    }
    let (kc_off, kc_size) = (u32_at(aci, 0x30)? as usize, u32_at(aci, 0x34)? as usize);
    let caps = aci.get(kc_off..kc_off.checked_add(kc_size)?)?;
    caps.as_chunks::<4>().0.iter().find_map(|w| {
        let cap = u32::from_le_bytes(*w);
        (cap & 0xF == 0b0111).then(|| {
            let (lo, hi) = ((cap >> 16) & 0xFF, (cap >> 24) & 0xFF);
            (lo.min(63)..=hi.min(63)).fold(0u64, |m, c| m | 1 << c)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::core_mask_of;

    fn npdm(cap: u32) -> Vec<u8> {
        let mut b = vec![0u8; 0x200];
        b[0..4].copy_from_slice(b"META");
        b[0x70..0x74].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"ACI0");
        b[0x80 + 0x30..0x80 + 0x34].copy_from_slice(&0x40u32.to_le_bytes());
        b[0x80 + 0x34..0x80 + 0x38].copy_from_slice(&8u32.to_le_bytes());
        b[0xC0..0xC4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        b[0xC4..0xC8].copy_from_slice(&cap.to_le_bytes());
        b
    }

    #[test]
    fn the_thread_info_capability_gives_the_cores() {
        // cores 0 to 5, priorities 24 to 59
        let cap = 0b0111 | 59 << 4 | 24 << 10 | 5 << 24;
        assert_eq!(core_mask_of(&npdm(cap)), Some(0b11_1111));
    }

    #[test]
    fn no_meta_header_gives_none() {
        assert_eq!(core_mask_of(&[0u8; 0x100]), None);
    }
}
