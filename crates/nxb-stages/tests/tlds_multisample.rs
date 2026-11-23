//! Encoded Maxwell TLDS multisample read against ordinary mip controls.
#![cfg(feature = "air-front")]
use nxb_tool::{FrontEnd, FrontEndError, airfront::RawFrontEnd};
use nxb_xlate::{
    job::parse_jobs,
    maxwell::insts::InstTlds,
    msl::{Context, translate},
};

#[test]
fn tlds_multisample_read_uses_the_supported_raw_profile() {
    const PT: u64 = 7 << 16;
    const EXIT: u64 = 0xe300_0000_0000_000f | PT;
    const MOV32I: u64 = 0x0100_0000_0000_0000 | PT | 15 << 12;
    const TLDS: u64 = 0xda00_0000_0000_0000 | PT;
    const ORDINARY_2D_MIP: u64 = 5;
    const MULTISAMPLE_2D: u64 = 6;
    for (name, target, sig) in [
        ("mip", ORDINARY_2D_MIP, "texture2d<float"),
        ("msaa", MULTISAMPLE_2D, "texture2d_ms<float"),
    ] {
        let ins = TLDS | 8 << 8 | 16 << 20 | 255 << 28 | target << 53;
        let decoded = InstTlds(ins);
        assert_eq!(
            (decoded.src_a(), decoded.src_b(), decoded.target().0),
            (8, 16, target as i32)
        );
        let mov = |r: u64, x: u64| MOV32I | r | x << 20;
        let value = mov(16, 2);
        let ops = [mov(8, 3), mov(9, 5), value, ins, EXIT];
        let mut header = [0u32; 20];
        header[0] = 5 << 10;
        header[18] = 1;
        let mut code: Vec<u8> = header.into_iter().flat_map(u32::to_le_bytes).collect();
        for bundle in ops.chunks(3) {
            code.extend(0u64.to_le_bytes());
            for op in bundle {
                code.extend(op.to_le_bytes());
            }
        }
        let job = parse_jobs(r#"[{"key":"authored_tlds","stage":"ps"}]"#)
            .unwrap()
            .remove(0);
        let result = translate(&job, &code, &Context::default()).unwrap();
        let lib = result.libraries.iter().find(|l| l.suffix == "_b").unwrap();
        let msl = &lib.msl;
        assert!(msl.contains(sig) && msl.contains(".read("), "{name}: {msl}");
        let mac = RawFrontEnd::new(false);
        let ios = RawFrontEnd::new(true);
        for (frontend, supports_msaa) in [(&mac, mac.id().ends_with("+macosx26")), (&ios, false)] {
            let raw = frontend.compile(msl.as_bytes());
            if target == ORDINARY_2D_MIP || supports_msaa {
                assert!(raw.expect(name).metallib.starts_with(b"MTLB"));
            } else {
                assert!(matches!(
                    raw,
                    Err(FrontEndError::Reject(reason))
                        if reason == "unsupported:texture2d_ms:target"
                ));
            }
        }
    }
}
