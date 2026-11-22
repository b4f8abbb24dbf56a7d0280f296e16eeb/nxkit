//! Authored Maxwell STS.U16/S16 and a matched STS.B32 control.
#![cfg(feature = "air-front")]

mod shader_fixture;

use nxb_xlate::{job::parse_jobs, maxwell::insts::InstSts};
use shader_fixture::check_shader;

#[test]
fn maxwell_shared_halfword_stores_build_raw_libraries() {
    const PT: u64 = 7 << 16;
    // MOV R0, 0x12345678; STS [RZ+offset], R0; EXIT.
    const MOV: u64 = 0x0101_2345_6787_f000;
    const EXIT: u64 = 0xe300_0000_0000_0000 | PT | 15;
    let job = parse_jobs(r#"[{"key":"authored_sts16","stage":"cs","local":[1,1,1],"shared":16}]"#)
        .unwrap()
        .remove(0);
    for (size, offset) in [(2, 0), (2, 2), (3, 0), (3, 2), (4, 0)] {
        let op = 0xef58_0000_0000_0000 | size << 48 | offset << 20 | PT | 0xff << 8;
        let sts = InstSts(op);
        assert_eq!(sts.ls_size().0 as u64, size);
        assert_eq!(sts.imm24() as u64, offset);
        let code: Vec<u8> = [0, MOV, op, EXIT]
            .into_iter()
            .flat_map(u64::to_le_bytes)
            .collect();
        let msl = check_shader(&job, &code).unwrap();
        assert_eq!(msl.contains("dm_ushort_alias"), size != 4, "{msl}");
    }
}
