//! Bound Maxwell TMML for plane, volume, and cube textures with live converted LOD.

#![cfg(feature = "air-front")]

use nxb_tool::{FrontEnd, FrontEndError, airfront::RawFrontEnd};
use nxb_xlate::{
    job::parse_jobs,
    maxwell::insts::InstTmml,
    msl::{Context, translate},
};

#[test]
fn maxwell_tmml_plane_volume_cube_and_cube_array_lod() {
    const PT: u64 = 7 << 16;
    const EXIT: u64 = 0xe300_0000_0000_000f | PT;
    const MOV32I: u64 = 0x0100_0000_0000_0000 | PT | 15 << 12;
    const FRAGMENT_STAGE: u32 = 5 << 10;
    const RT0_X: u32 = 1;
    const VOLUME_UNCLAMPED: u64 = 0xdf58_0080_cff7_0800;
    const VOLUME_CLAMPED: u64 = 0xdf58_0081_4ff7_0800;
    const PLANE_UNCLAMPED: u64 = 0xdf58_0080_aff7_0800;
    const PLANE_CLAMPED: u64 = 0xdf58_0081_2ff7_0800;
    const DIM_SHIFT: u32 = 28;
    const CUBE_DIM: i32 = 6;
    const CUBE_ARRAY_DIM: i32 = 7;
    const DIM_MASK: u64 = 0b111 << DIM_SHIFT;
    let dimension = |word, dim| (word & !DIM_MASK) | (dim as u64) << DIM_SHIFT;
    let mac = RawFrontEnd::new(false);
    let ios = RawFrontEnd::new(true);
    for (word, kind, clamped) in [
        (VOLUME_UNCLAMPED, "texture3d", false),
        (VOLUME_CLAMPED, "texture3d", true),
        (PLANE_UNCLAMPED, "texture2d", false),
        (PLANE_CLAMPED, "texture2d", true),
        (dimension(VOLUME_UNCLAMPED, CUBE_DIM), "texturecube", false),
        (dimension(VOLUME_CLAMPED, CUBE_DIM), "texturecube", true),
        (
            dimension(VOLUME_UNCLAMPED, CUBE_ARRAY_DIM),
            "texturecube_array",
            false,
        ),
        (
            dimension(VOLUME_CLAMPED, CUBE_ARRAY_DIM),
            "texturecube_array",
            true,
        ),
    ] {
        let inst = InstTmml(word);
        assert_eq!(inst.src_a(), 8);
        assert_eq!(inst.dest(), 0);
        if kind == "texturecube" {
            assert_eq!(inst.dim().0, CUBE_DIM);
        } else if kind == "texturecube_array" {
            assert_eq!(inst.dim().0, CUBE_ARRAY_DIM);
        }
        let mov = |reg: u64, f: f32| MOV32I | reg | u64::from(f.to_bits()) << 20;
        let mut header = [0u32; 20];
        header[0] = FRAGMENT_STAGE;
        header[18] = RT0_X;
        let mut code: Vec<u8> = header.into_iter().flat_map(u32::to_le_bytes).collect();
        let coords = [mov(8, 0.25), mov(9, 0.5), mov(10, 0.75)];
        let plain = [coords, [word, EXIT, 0]];
        let cube_array = [
            [MOV32I | 8 | 3 << 20, mov(9, 0.25), mov(10, 0.5)],
            [mov(11, 0.75), word, EXIT],
        ];
        for bundle in if kind == "texturecube_array" {
            cube_array
        } else {
            plain
        } {
            code.extend(0u64.to_le_bytes());
            code.extend(bundle.into_iter().flat_map(u64::to_le_bytes));
        }
        let job = parse_jobs(r#"[{"key":"authored_tmml","stage":"ps"}]"#)
            .unwrap()
            .remove(0);
        let result = translate(&job, &code, &Context::default()).unwrap();
        let msl = &result
            .libraries
            .iter()
            .find(|l| l.suffix == "_b")
            .unwrap()
            .msl;
        let shape = format!("{kind}<float>");
        let mode = if clamped {
            ".calculate_clamped_lod("
        } else {
            ".calculate_unclamped_lod("
        };
        assert!(msl.contains(&shape) && msl.contains(mode), "{msl}");
        if kind == "texturecube_array" {
            let call = msl
                .lines()
                .find(|line| line.contains(mode))
                .expect("cube-array LOD");
            assert!(call.contains(", float3(0.25f, 0.5f, 0.75f))"), "{call}");
        }
        assert!(msl.contains("out.c0.x ="), "{msl}");
        let mac_admits_air28 = mac.id().ends_with("+macosx26");
        for (frontend, admits_air28) in [(&mac, mac_admits_air28), (&ios, false)] {
            let result = frontend.compile(msl.as_bytes());
            if kind == "texture2d" || admits_air28 {
                assert!(result.unwrap().metallib.starts_with(b"MTLB"));
            } else {
                let Err(FrontEndError::Reject(reason)) = result else {
                    panic!("expected {kind} LOD target rejection");
                };
                assert_eq!(reason, format!("unsupported:lod:{kind}"));
            }
        }
    }
}
