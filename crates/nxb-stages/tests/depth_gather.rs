//! Authored Maxwell TLD4.DC with all four gathered taps live.
#![cfg(feature = "air-front")]

use nxb_tool::{FrontEnd, FrontEndError, airfront::RawFrontEnd};
use nxb_xlate::{
    job::parse_jobs,
    maxwell::insts::InstTld4,
    msl::{Context, translate},
};

#[test]
fn maxwell_tld4_dc_emits_depth_compare_gather() -> Result<(), Box<dyn std::error::Error>> {
    check_depth_gather(2, "depth2d<float>")?;
    check_depth_gather(3, "depth2d_array<float>")?;
    check_depth_gather(6, "depthcube<float>")?;
    check_depth_gather(7, "depthcube_array<float>")?;
    Ok(())
}

fn check_depth_gather(dim: u64, resource: &str) -> Result<(), Box<dyn std::error::Error>> {
    const PT: u64 = 7 << 16;
    const EXIT: u64 = 0xe300_0000_0000_000f | PT;
    const MOV32I: u64 = 0x0100_0000_0000_0000 | PT | 15 << 12;
    const TLD4: u64 = 0xc800_0000_0000_0000;
    const DC: u64 = 1 << 50;
    const PARAMS: u64 = 7 << 51 | PT | 8 << 8 | 16 << 20;
    const RT0_XYZW: u32 = 15;
    let inst = TLD4 | DC | PARAMS | dim << 28 | u64::from(RT0_XYZW) << 31;
    let decoded = InstTld4(inst);
    assert!(decoded.dc());
    assert_eq!(decoded.dim().0, dim as i32);
    let mov = |reg: u64, f: f32| MOV32I | reg | u64::from(f.to_bits()) << 20;
    let mut header = [0u32; 20];
    header[0] = 5 << 10;
    header[18] = RT0_XYZW;
    let mut code: Vec<u8> = header.into_iter().flat_map(u32::to_le_bytes).collect();
    let coord_z = if dim == 6 {
        mov(10, 0.125)
    } else {
        mov(17, 0.75)
    };
    let plane_or_cube = [
        [mov(8, 0.25), mov(9, 0.5), mov(16, 0.75)],
        [coord_z, inst, EXIT],
    ];
    let array = [
        [MOV32I | 8 | 3 << 20, mov(9, 0.25), mov(10, 0.5)],
        [mov(11, 0.125), mov(16, 0.75), inst],
        [EXIT, 0, 0],
    ];
    let bundles: &[[u64; 3]] = if matches!(dim, 3 | 7) {
        &array
    } else {
        &plane_or_cube
    };
    for bundle in bundles {
        code.extend(0u64.to_le_bytes());
        for op in bundle {
            code.extend(op.to_le_bytes());
        }
    }
    let jobs = parse_jobs(r#"[{"key":"authored_gather","stage":"ps"}]"#)?;
    let job = jobs.first().ok_or("missing gather fixture job")?;
    let result = translate(job, &code, &Context::default()).map_err(|err| format!("{err:?}"))?;
    let msl = &result
        .libraries
        .iter()
        .find(|lib| lib.suffix == "_b")
        .ok_or("missing gather shader variant")?
        .msl;
    assert!(msl.contains(resource), "{msl}");
    let call = msl
        .lines()
        .find(|line| line.contains(".gather_compare("))
        .ok_or("missing gather comparison")?;
    if matches!(dim, 3 | 7) {
        assert!(call.contains(", uint(max(3, 0)), 0.75f)"), "{call}");
    }
    for tap in ["x", "y", "z", "w"] {
        assert!(msl.contains(&format!("out.c0.{tap} =")), "{msl}");
    }
    let mac = RawFrontEnd::new(false);
    let ios = RawFrontEnd::new(true);
    for (frontend, supports_gather) in [(&mac, mac.id().ends_with("+macosx26")), (&ios, false)] {
        let raw = frontend.compile(msl.as_bytes());
        if supports_gather {
            assert!(raw?.metallib.starts_with(b"MTLB"));
        } else {
            assert!(matches!(
                raw,
                Err(FrontEndError::Reject(reason)) if reason == "unsupported:gather_compare:target"
            ));
        }
    }
    Ok(())
}
