// Synthetic Maxwell shader records. The wire constants here are independent of the decoder.
pub(super) const CODE_START: usize = 0x38 + 0x30;
pub(super) const CONTROL_LEN: usize = 0x748;
pub(super) const CTL_MAGIC: [u8; 4] = [0x34, 0x12, 0x76, 0x98];
pub(super) const CTL_VERSION_OFFSET: usize = 4;
pub(super) const NVN2_CTL_VERSION: u32 = 0xc9;
pub(super) const TRI_ODD_CCW: [u32; 3] = [4, 0x8e7b, 0x0901];
const CTL_MODE_OFFSETS: [usize; 3] = [0x738, 0x73c, 0x744];
const SPH_WORDS: usize = 20;
const SPH_STAGE_SHIFT: u32 = 10;
pub(super) const SPH_VERTEX: u32 = 1;
pub(super) const SPH_HULL: u32 = 2;
pub(super) const SPH_DOMAIN: u32 = 3;
const SPH_FRAGMENT: u32 = 5;
const ZERO_REGISTER: u64 = 255;
const POSITION_ATTR: u64 = 0x70;
const USER_ATTR: u64 = 0x80;
const TESS_COORD_ATTR: u64 = 0x2f0;
const ATTR_COMPONENT_BYTES: u64 = 4;
const VECTOR_COMPONENTS: u64 = 4;
const ATTR_XY: u64 = 1 << 47;
const ATTR_INDEX_REG_SHIFT: u32 = 39;
const SASS_OPS_PER_CONTROL_WORD: usize = 3;
const PT: u64 = 7 << 16;
const EXIT: u64 = 0xe300_0000_0007_000f;
fn ald(r: u64, a: u64) -> u64 {
    (0b11101111110110u64 << 50)
        | r
        | (ZERO_REGISTER << 8)
        | PT
        | (a << 20)
        | (ZERO_REGISTER << ATTR_INDEX_REG_SHIFT)
}
fn ast(r: u64, a: u64) -> u64 {
    (0b11101111111100u64 << 50)
        | r
        | (ZERO_REGISTER << 8)
        | PT
        | (a << 20)
        | (ZERO_REGISTER << ATTR_INDEX_REG_SHIFT)
}
fn mov(r: u64, bits: u32) -> u64 {
    (0b00000001000000u64 << 50) | r | (15 << 12) | PT | (u64::from(bits) << 20)
}
// InstIpa fields, table_gen IPA template; Pass, nonindexed, nonsaturating.
fn ipa(r: u64, a: u64) -> u64 {
    (0b11100000000000u64 << 50)
        | r
        | (ZERO_REGISTER << 8)
        | PT
        | (ZERO_REGISTER << 20)
        | (a << 28)
        | (ZERO_REGISTER << ATTR_INDEX_REG_SHIFT)
        | (7 << 47)
}
fn code(h: [u32; SPH_WORDS], ops: &[u64]) -> Vec<u8> {
    let mut c: Vec<u8> = h.into_iter().flat_map(u32::to_le_bytes).collect();
    for (i, w) in ops.iter().chain([EXIT].iter()).enumerate() {
        if i % SASS_OPS_PER_CONTROL_WORD == 0 {
            c.extend(0u64.to_le_bytes());
        }
        c.extend(w.to_le_bytes());
    }
    c
}
pub(super) fn vs_capture() -> Vec<u8> {
    let mut h = [0; SPH_WORDS];
    h[0] = SPH_VERTEX << SPH_STAGE_SHIFT;
    let ops: Vec<_> = (0..VECTOR_COMPONENTS)
        .flat_map(|c| {
            [
                ald(c, USER_ATTR + ATTR_COMPONENT_BYTES * c),
                ast(c, POSITION_ATTR + ATTR_COMPONENT_BYTES * c),
            ]
        })
        .collect();
    code(h, &ops)
}

fn ffma(rd: u64, a: u64, b: u64, c: u64) -> u64 {
    (0b010110011u64 << 55) | rd | (a << 8) | PT | (b << 20) | (c << 39)
}

pub(super) fn ds_partial_x() -> Vec<u8> {
    let mut h = [0; SPH_WORDS];
    h[0] = SPH_DOMAIN << SPH_STAGE_SHIFT;
    // Read actual captured clip positions. Interpolate all four components with
    // barycentric weights (1-u-v,u,v), no built-in clip coordinates in the DS.
    let mut ops = vec![
        ald(4, TESS_COORD_ATTR),
        ald(5, TESS_COORD_ATTR + ATTR_COMPONENT_BYTES),
        mov(6, 1.0f32.to_bits()),
        mov(7, (-1.0f32).to_bits()),
        ffma(6, 4, 7, 6),
        ffma(6, 5, 7, 6),
        mov(10, 1),
        mov(11, 2),
    ];
    for c in 0..VECTOR_COMPONENTS {
        let slot = POSITION_ATTR + ATTR_COMPONENT_BYTES * c;
        ops.extend([
            ald(0, slot),
            (ald(1, slot) & !(ZERO_REGISTER << ATTR_INDEX_REG_SHIFT))
                | (10 << ATTR_INDEX_REG_SHIFT),
            (ald(2, slot) & !(ZERO_REGISTER << ATTR_INDEX_REG_SHIFT))
                | (11 << ATTR_INDEX_REG_SHIFT),
            ffma(3, 0, 6, ZERO_REGISTER),
            ffma(3, 1, 4, 3),
            ffma(3, 2, 5, 3),
            ast(3, slot),
        ]);
    }
    // Only loc0.x is written. loc0.yzw must retain (0,0,1).
    ops.extend([mov(8, 0.25f32.to_bits()), ast(8, USER_ATTR)]);
    code(h, &ops)
}
pub(super) fn fs_xyzw(location: u32) -> Vec<u8> {
    assert!(location < 2);
    let mut h = [0; SPH_WORDS];
    h[0] = SPH_FRAGMENT << SPH_STAGE_SHIFT;
    h[18] = 15;
    h[6] = 0xaau32 << (location * 8); // Perspective for each XYZW component.
    let a = USER_ATTR + u64::from(location) * VECTOR_COMPONENTS * ATTR_COMPONENT_BYTES;
    code(
        h,
        &[0, 1, 2, 3].map(|c| ipa(c, a + c * ATTR_COMPONENT_BYTES)),
    )
}
// Record envelope read by job_for. The packaging test adds the split record header.
pub(super) fn record(code: &[u8]) -> Vec<u8> {
    record_with_mode(code, TRI_ODD_CCW)
}

pub(super) fn coordinate_record(sph_stage: u32, mode: [u32; 3]) -> Vec<u8> {
    let mut h = [0; SPH_WORDS];
    h[0] = sph_stage << SPH_STAGE_SHIFT;
    // ALD R0, tessellation coordinate u,v; AST clip position x,y, R0.
    let ops = [
        ald(0, TESS_COORD_ATTR) | ATTR_XY,
        ast(0, POSITION_ATTR) | ATTR_XY,
    ];
    record_with_mode(&code(h, &ops), mode)
}

fn record_with_mode(code: &[u8], mode: [u32; 3]) -> Vec<u8> {
    let mut rec = vec![0; CODE_START];
    rec.extend_from_slice(code);
    let mut ctl = vec![0; CONTROL_LEN];
    ctl[..CTL_MAGIC.len()].copy_from_slice(&CTL_MAGIC);
    for (a, w) in CTL_MODE_OFFSETS.into_iter().zip(mode) {
        ctl[a..a + 4].copy_from_slice(&w.to_le_bytes());
    }
    rec.extend(ctl);
    rec
}
