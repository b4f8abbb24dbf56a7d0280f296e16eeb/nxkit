//! Authored Maxwell TLD.LL with distinct XYZ coordinates and mip.
#![cfg(feature = "air-front")]

use nxb_tool::{FrontEnd, FrontEndError, airfront::RawFrontEnd};
use nxb_xlate::{
    job::parse_jobs,
    maxwell::insts::InstTld,
    msl::{Context, translate},
};

#[test]
fn maxwell_tld_emits_volume_read_with_z_and_mip() {
    const PT: u64 = 7 << 16;
    const EXIT: u64 = 0xe300_0000_0000_000f | PT;
    const MOV32I: u64 = 0x0100_0000_0000_0000 | PT | 15 << 12;
    const TLD: u64 = 0xdc80_0000_0000_0000 | 7 << 51 | PT;
    const VOLUME: u64 = 4;
    const RT0_XYZW: u32 = 15;
    let ins = TLD | u64::from(RT0_XYZW) << 31 | 8 << 8 | 16 << 20 | VOLUME << 28;
    let decoded = InstTld(ins);
    assert!(decoded.lod() && !decoded.ms() && !decoded.toff());
    assert_eq!(
        (decoded.src_a(), decoded.src_b(), decoded.dim().0),
        (8, 16, VOLUME as i32)
    );
    let mov = |r: u64, x: u64| MOV32I | r | x << 20;
    let mut header = [0u32; 20];
    header[0] = 5 << 10;
    header[18] = RT0_XYZW;
    let mut code: Vec<u8> = header.into_iter().flat_map(u32::to_le_bytes).collect();
    for bundle in [[mov(8, 3), mov(9, 5), mov(10, 7)], [mov(16, 2), ins, EXIT]] {
        code.extend(0u64.to_le_bytes());
        for op in bundle {
            code.extend(op.to_le_bytes());
        }
    }
    let job = parse_jobs(r#"[{"key":"authored_tld","stage":"ps"}]"#)
        .unwrap()
        .remove(0);
    let result = translate(&job, &code, &Context::default()).unwrap();
    let lib = result.libraries.iter().find(|l| l.suffix == "_b").unwrap();
    let msl = &lib.msl;
    assert!(msl.contains("texture3d<float>"), "{msl}");
    assert!(msl.contains(".read(uint3(3, 5, 7), uint(2))"), "{msl}");
    let mac = RawFrontEnd::new(false);
    let ios = RawFrontEnd::new(true);
    for (frontend, supports_volume) in [(&mac, mac.id().ends_with("+macosx26")), (&ios, false)] {
        let raw = frontend.compile(msl.as_bytes());
        if supports_volume {
            assert!(raw.unwrap().metallib.starts_with(b"MTLB"));
        } else {
            assert!(matches!(
                raw,
                Err(FrontEndError::Reject(reason)) if reason == "unsupported:read:texture3d"
            ));
        }
    }
}
