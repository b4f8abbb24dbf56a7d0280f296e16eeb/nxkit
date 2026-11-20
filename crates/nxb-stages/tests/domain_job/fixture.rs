// Synthetic Maxwell domain and fragment shaders.
const PT: u64 = 7 << 16;
const EXIT: u64 = 0xe300_0000_0007_000f;
fn ald(r: u64, a: u64) -> u64 {
    (0b11101111110110u64 << 50) | r | (255 << 8) | PT | (a << 20) | (255 << 39)
}
fn ast(r: u64, a: u64) -> u64 {
    (0b11101111111100u64 << 50) | r | (255 << 8) | PT | (a << 20) | (255 << 39)
}
fn mov(r: u64, bits: u32) -> u64 {
    (0b00000001000000u64 << 50) | r | (15 << 12) | PT | (u64::from(bits) << 20)
}
// InstIpa fields, table_gen IPA template; Pass, nonindexed, nonsaturating.
fn ipa(r: u64, a: u64) -> u64 {
    (0b11100000000000u64 << 50)
        | r
        | (255 << 8)
        | PT
        | (255 << 20)
        | (a << 28)
        | (255 << 39)
        | (7 << 47)
}
fn code(h: [u32; 20], ops: &[u64]) -> Vec<u8> {
    let mut c: Vec<u8> = h.into_iter().flat_map(u32::to_le_bytes).collect();
    for (i, w) in ops.iter().chain([EXIT].iter()).enumerate() {
        if i % 3 == 0 {
            c.extend(0u64.to_le_bytes());
        }
        c.extend(w.to_le_bytes());
    }
    c
}
pub(super) fn vs_capture() -> Vec<u8> {
    let mut h = [0; 20];
    h[0] = 1 << 10;
    let ops: Vec<_> = (0..4)
        .flat_map(|c| [ald(c, 0x80 + 4 * c), ast(c, 0x70 + 4 * c)])
        .collect();
    code(h, &ops)
}

fn ffma(rd: u64, a: u64, b: u64, c: u64) -> u64 {
    (0b010110011u64 << 55) | rd | (a << 8) | PT | (b << 20) | (c << 39)
}

pub(super) fn ds_partial_x() -> Vec<u8> {
    let mut h = [0; 20];
    h[0] = 3 << 10;
    // Read actual captured clip positions. Interpolate all four components with
    // barycentric weights (1-u-v,u,v), no built-in clip coordinates in the DS.
    let mut ops = vec![
        ald(4, 0x2f0),
        ald(5, 0x2f4),
        mov(6, 0x3f800000),
        mov(7, 0xbf800000),
        ffma(6, 4, 7, 6),
        ffma(6, 5, 7, 6),
        mov(10, 1),
        mov(11, 2),
    ];
    for c in 0..4 {
        let slot = 0x70 + 4 * c;
        ops.extend([
            ald(0, slot),
            (ald(1, slot) & !(255 << 39)) | (10 << 39),
            (ald(2, slot) & !(255 << 39)) | (11 << 39),
            ffma(3, 0, 6, 255),
            ffma(3, 1, 4, 3),
            ffma(3, 2, 5, 3),
            ast(3, slot),
        ]);
    }
    // Only loc0.x is written. loc0.yzw must retain (0,0,1).
    ops.extend([mov(8, 0x3e800000), ast(8, 0x80)]);
    code(h, &ops)
}
pub(super) fn fs_xyzw(location: u32) -> Vec<u8> {
    assert!(location < 2);
    let mut h = [0; 20];
    h[0] = 5 << 10;
    h[18] = 15;
    h[6] = 0xaau32 << (location * 8); // Perspective for each XYZW component.
    let a = 0x80 + u64::from(location) * 16;
    code(
        h,
        &[ipa(0, a), ipa(1, a + 4), ipa(2, a + 8), ipa(3, a + 12)],
    )
}
// Native producer envelope patterned after nxb-stages/tests/domain_job.rs candidate.
// This is a record for job_for, not a fabricated BNSH stage association.
pub(super) fn record(code: &[u8]) -> Vec<u8> {
    let mut rec = vec![0; 0x38 + 0x30];
    rec.extend_from_slice(code);
    let mut ctl = vec![0; 0x748];
    ctl[..4].copy_from_slice(&[0x34, 0x12, 0x76, 0x98]);
    for (a, w) in [(0x738, 4u32), (0x73c, 0x8e7b), (0x744, 0x0901)] {
        ctl[a..a + 4].copy_from_slice(&w.to_le_bytes());
    }
    rec.extend(ctl);
    rec
}
