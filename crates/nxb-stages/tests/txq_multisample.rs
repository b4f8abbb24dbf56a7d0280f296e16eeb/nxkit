//! Encoded Maxwell TXQ multisample, texel-buffer and ordinary dimensions.
#![cfg(feature = "air-front")]
use nxb_tool::{FrontEnd, FrontEndError, airfront::RawFrontEnd};
use nxb_xlate::{
    job::parse_jobs,
    maxwell::insts::InstTxq,
    msl::{Context, translate},
};

#[test]
fn txq_multisample_queries_use_the_supported_raw_profile() {
    const PT: u64 = 7 << 16;
    const EXIT: u64 = 0xe300_0000_0000_000f | PT;
    const MOV32I: u64 = 0x0100_0000_0000_0000 | PT | 15 << 12;
    const TXQ: u64 = 0xdf48_0000_0000_0000 | PT;
    const HANDLE: i32 = 8;
    const DIMENSIONS: u64 = 1;
    const SAMPLE_COUNT: u64 = 2;
    const WIDTH: u64 = 1;
    const WIDTH_HEIGHT: u64 = 3;
    const WIDTH_HEIGHT_LAYERS: u64 = 7;
    const SAMPLES: u64 = 4;
    const FRAGMENT_STAGE: u32 = 5 << 10;
    const RT0_X: u32 = 1;
    const RT0_XY: u32 = 3;
    const RT0_XYZ: u32 = 7;
    let mac = RawFrontEnd::new(false);
    let ios = RawFrontEnd::new(true);
    for (name, ty, query, mask, sig) in [
        (
            "msaa-samples",
            "2dms",
            SAMPLE_COUNT,
            SAMPLES,
            ".get_num_samples()",
        ),
        ("msaa-wh", "2dms", DIMENSIONS, WIDTH_HEIGHT, ".get_width()"),
        (
            "msaa-array-samples",
            "2dmsarray",
            SAMPLE_COUNT,
            SAMPLES,
            ".get_num_samples()",
        ),
        (
            "msaa-array-whl",
            "2dmsarray",
            DIMENSIONS,
            WIDTH_HEIGHT_LAYERS,
            ".get_array_size()",
        ),
        ("buffer-width", "buffer", DIMENSIONS, WIDTH, ".get_width()"),
        ("ordinary-wh", "2d", DIMENSIONS, WIDTH_HEIGHT, ".get_width("),
    ] {
        let ins = TXQ | 8 << 8 | query << 22 | mask << 31 | (HANDLE as u64) << 36;
        let decoded = InstTxq(ins);
        assert_eq!(
            (decoded.tid_b(), decoded.tex_query().0, decoded.w_mask()),
            (HANDLE, query as i32, mask as i32)
        );
        let mov = |r: u64, x: u64| MOV32I | r | x << 20;
        let ops = [mov(8, 2), ins, EXIT];
        let mut header = [0u32; 20];
        header[0] = FRAGMENT_STAGE;
        header[18] = match mask {
            WIDTH_HEIGHT => RT0_XY,
            WIDTH_HEIGHT_LAYERS => RT0_XYZ,
            _ => RT0_X,
        };
        let mut code: Vec<u8> = header.into_iter().flat_map(u32::to_le_bytes).collect();
        for bundle in ops.chunks(3) {
            code.extend(0u64.to_le_bytes());
            for op in bundle {
                code.extend(op.to_le_bytes());
            }
        }
        let tex_type = format!(r#"[{{"key":"authored_txq","stage":"ps","tex":{{"0":"{ty}"}}}}]"#);
        let job = parse_jobs(&tex_type).unwrap().remove(0);
        let result = translate(&job, &code, &Context::default()).unwrap();
        let lib = result.libraries.iter().find(|l| l.suffix == "_b").unwrap();
        let msl = &lib.msl;
        assert!(
            msl.contains(sig)
                && msl.contains(match ty {
                    "2dms" => "texture2d_ms<float",
                    "2dmsarray" => "texture2d_ms_array<float",
                    "buffer" => "texture_buffer<float>",
                    _ => "texture2d<float",
                }),
            "{name}: {msl}"
        );
        if matches!(ty, "2dms" | "2dmsarray") && query == DIMENSIONS {
            assert!(msl.contains(".get_height()"), "{msl}");
        }
        for (frontend, air28) in [(&mac, mac.id().ends_with("+macosx26")), (&ios, false)] {
            let raw = frontend.compile(msl.as_bytes());
            if ty == "2d" || air28 {
                assert!(raw.expect(name).metallib.starts_with(b"MTLB"));
            } else {
                assert!(matches!(
                    raw,
                    Err(FrontEndError::Reject(reason))
                        if reason == match ty {
                            "buffer" => "unsupported:texture_buffer:target",
                            "2dmsarray" => "unsupported:texture2d_ms_array:target",
                            _ => "unsupported:texture2d_ms:target",
                        }
                ));
            }
        }
    }
}
