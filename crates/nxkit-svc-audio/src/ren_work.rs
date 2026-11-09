//! The work buffer size of the Horizon audio renderer service. It follows `ar_service_work_size` of nxnative.
//!
//! A game sizes its work buffer for the DSP service. The SDK's own CPU renderer needs a different size, so the take-over
//! of `GetAudioRendererWorkBufferSize` answers the service's number.

const WORK_PAGE: usize = 0x1000;

fn align(n: usize, a: usize) -> usize {
    (n + a - 1) & !(a - 1)
}

fn le_u32(p: &[u8], o: usize) -> Option<u32> {
    let b = p.get(o..o + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The work buffer size for the renderer parameter `p` (at least 0x40 bytes), or 0 when the revision is unknown or a
/// field is out of range. Revisions 1 to 15 are known. SDK 8 keeps the revision at +0x38 and SDK 9 and later at +0x34.
pub(crate) fn service_work_size(p: &[u8]) -> usize {
    let Some(mut revision) = le_u32(p, 0x34) else {
        return 0;
    };
    if revision & 0xff_ffff != 0x56_4552 {
        revision = le_u32(p, 0x38).unwrap_or(0);
    }
    if revision & 0xff_ffff != 0x56_4552 || !(0x3156_4552..=0x3f56_4552).contains(&revision) {
        return 0;
    }
    let r = (revision >> 24) - 0x30;
    let mut n = [0usize; 11];
    for (i, slot) in n.iter_mut().enumerate() {
        if i == 8 {
            // the voice drop flag is a byte, the padding after it is not set
            *slot = usize::from(p.get(0x20).copied().unwrap_or(0));
            continue;
        }
        let Some(v) = le_u32(p, i * 4) else { return 0 };
        let v = v as i32;
        if !(0..=0x10_0000).contains(&v) {
            return 0;
        }
        *slot = v as usize;
    }
    let (mix, sub, voices, sinks, effects, perf, splitters, dest) =
        (n[2], n[3], n[4], n[5], n[6], n[7], n[9], n[10]);
    let mixes = sub + 1;
    let mut size = align(((sinks + sub) * 0x3c0 + n[1] * 4) * (mix + 6), 64);
    size += align(mix * 4, 64) + sub * 0x400 + mixes * 0x940 + align(mixes * 8, 16);
    size += voices * 0x1d0
        + align(voices * if r >= 15 { 0x238 } else { 0x220 }, 16)
        + align(voices * 8, 16);
    if r >= 2 {
        let graph = mixes * mixes * 4
            + mixes * 12
            + 2 * align(mixes, 64) / 8
            + align(mixes * mixes, 64) / 8;
        size += align(graph, 16);
        let per_dest = if r >= 12 {
            if r >= 15 { 0x130 } else { 8 }
        } else {
            0xe0
        };
        size += splitters * 0x20 + align(dest * per_dest, 16);
        if r < 5 {
            size += align(dest * 4, 16);
        }
    }
    size += (effects + voices * 4) * 0x20 + 0x50;
    if r >= 12 && splitters != 0 && dest != 0 {
        size += align(dest * if r >= 14 { 0xe0 } else { 0x80 }, 64);
    }
    size = align(size + if r >= 9 { effects * 0x80 } else { 0 }, 64);
    size += (sinks + sub) * 0x280
        + effects * if r >= 9 { 0x530 } else { 0x4b0 }
        + sinks * 0x170
        + voices * 0x100
        + 0x40;
    if perf != 0 {
        let per = if r >= 5 { 0x18 } else { 0x10 };
        let frame = (sub + voices + sinks + effects + 1) * per + if r >= 5 { 0x990 } else { 0x658 };
        size += align(frame * (perf + 1), 64) + 0xc0;
    }
    let mut commands = 0x18000;
    if r >= 5 {
        commands = effects * 0x540
            + sub * 0x3f24
            + sinks * 0x130
            + dest * 0x360
            + voices * (splitters * 0x50 + if r >= 8 { 0x284 } else { 0x250 })
            + (voices + sub + effects + sinks + 0x65) * 0x28
            + 0x2c4;
    }
    align(size + commands + 0x7e, WORK_PAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_revision_gives_zero() {
        assert_eq!(service_work_size(&[0u8; 0x40]), 0);
    }

    #[test]
    fn size_is_page_aligned() {
        let mut p = [0u8; 0x40];
        p[0..4].copy_from_slice(&48000u32.to_le_bytes());
        p[4..8].copy_from_slice(&240u32.to_le_bytes());
        p[8..12].copy_from_slice(&24u32.to_le_bytes());
        p[0x10..0x14].copy_from_slice(&64u32.to_le_bytes());
        p[0x34..0x38].copy_from_slice(&0x3d56_4552u32.to_le_bytes());
        let n = service_work_size(&p);
        assert!(n > 0 && n.is_multiple_of(WORK_PAGE));
    }
}
