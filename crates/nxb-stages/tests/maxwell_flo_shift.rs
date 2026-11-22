//! An authored Maxwell shader with FLO.SH, which requires reverse_bits and ctz.
#![cfg(feature = "air-front")]

mod shader_fixture;

use nxb_xlate::{job::parse_jobs, maxwell::insts::InstFloR};
use shader_fixture::check_shader;

#[test]
fn maxwell_flo_shift_builds_raw_libraries() {
    const PT: u64 = 7 << 16;
    // MOV R8, 0x100; MOV R9, 0; LDG.E.B32 R10, [R8]; FLO.U32.SH R11, R10;
    // STG.E.B32 [R8+4], R11; EXIT. The toy address must not be dispatched.
    let mov = |r, v: u32| 0x0100_0000_0000_0000 | PT | 15 << 12 | r | (u64::from(v) << 20);
    let load = 0xeed0_0000_0000_0000 | 4 << 48 | 1 << 45 | PT | 8 << 8 | 10;
    let flo = 0x5c30_0000_0000_0000 | 1 << 41 | PT | 10 << 20 | 11;
    let store = 0xeed8_0000_0000_0000 | 4 << 48 | 1 << 45 | 4 << 20 | PT | 8 << 8 | 11;
    let exit = 0xe300_0000_0000_0000 | PT | 15;
    let op = InstFloR(flo);
    assert!(op.sh());
    assert_eq!((op.src_b(), op.dest()), (10, 11));
    let words = [0, mov(8, 0x100), mov(9, 0), load, 0, flo, store, exit];
    let code: Vec<u8> = words.into_iter().flat_map(u64::to_le_bytes).collect();
    let job = parse_jobs(r#"[{"key":"authored_flo_shift","stage":"cs","local":[1,1,1]}]"#)
        .unwrap()
        .remove(0);
    let msl = check_shader(&job, &code).unwrap();
    assert!(msl.contains("reverse_bits("), "{}", msl);
    assert!(msl.contains("0xffffffffu : ctz("), "{}", msl);
}
