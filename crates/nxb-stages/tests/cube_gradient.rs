//! Authored bound Maxwell TXD, with a live sample result.
#![cfg(feature = "air-front")]

use nxb_tool::{FrontEnd, FrontEndError, airfront::RawFrontEnd};
use nxb_xlate::{
    job::parse_jobs,
    maxwell::insts::InstTxd,
    msl::{Context, translate},
};

#[test]
fn maxwell_txd_emits_derivatives_for_both_sampler_paths() -> Result<(), Box<dyn std::error::Error>>
{
    for (dimension, kind) in [(6, "cube"), (7, "cube_array"), (4, "3d"), (2, "2d")] {
        check_txd(dimension, kind)?;
    }
    Ok(())
}

fn check_txd(dimension: u64, kind: &str) -> Result<(), Box<dyn std::error::Error>> {
    const PT: u64 = 7 << 16;
    const EXIT: u64 = 0xe300_0000_0000_000f | PT;
    const MOV32I: u64 = 0x0100_0000_0000_0000 | PT | 15 << 12;
    const TXD: u64 = 0xde00_0000_0000_0000 | 7 << 51 | PT;
    let txd = TXD | dimension << 28 | 1 << 31 | 8 << 8 | 16 << 20;
    let ins = InstTxd(txd);
    assert_eq!(
        (ins.dim().0, ins.src_a(), ins.src_b(), ins.w_mask()),
        (dimension as i32, 8, 16, 1)
    );
    let mov = |reg: u64, f: f32| MOV32I | reg | u64::from(f.to_bits()) << 20;
    let mut header = [0u32; 20];
    header[0] = 5 << 10;
    header[18] = 1;
    let mut code: Vec<u8> = header.into_iter().flat_map(u32::to_le_bytes).collect();
    let ops = [
        mov(8, 0.25),
        mov(9, 0.5),
        mov(10, 0.75),
        if kind == "cube_array" {
            MOV32I | 11 | 3 << 20
        } else {
            mov(11, 0.0)
        },
        mov(16, 0.125),
        mov(17, 0.0),
        mov(18, 0.0),
        mov(19, 0.125),
        mov(20, 0.0),
        mov(21, 0.25),
        txd,
        EXIT,
    ];
    for bundle in ops.chunks(3) {
        code.extend(0u64.to_le_bytes());
        for op in bundle {
            code.extend(op.to_le_bytes());
        }
    }
    let jobs = parse_jobs(r#"[{"key":"authored_txd","stage":"ps"}]"#)?;
    let job = jobs.first().ok_or("missing TXD fixture job")?;
    let result = translate(job, &code, &Context::default()).map_err(|err| format!("{err:?}"))?;
    let msl = &result
        .libraries
        .iter()
        .find(|lib| lib.suffix == "_b")
        .ok_or("missing TXD shader variant")?
        .msl;
    assert!(msl.contains(&format!("texture{kind}<float")), "{msl}");
    assert!(msl.contains(" = dm_bs("), "{msl}");
    let derivative = kind.strip_suffix("_array").unwrap_or(kind);
    assert_eq!(
        msl.matches(&format!("gradient{derivative}(")).count(),
        2,
        "{msl}"
    );
    if kind == "cube_array" {
        assert_eq!(
            msl.matches("uint(max(3, 0)), gradientcube(").count(),
            2,
            "{msl}"
        );
    }
    if matches!(kind, "2d" | "cube_array") {
        let mac = RawFrontEnd::new(false);
        let ios = RawFrontEnd::new(true);
        let mac_admits_air28 = mac.id().ends_with("+macosx26");
        for (frontend, admits) in [(&mac, mac_admits_air28), (&ios, false)] {
            let result = frontend.compile(msl.as_bytes());
            if admits {
                assert!(result?.metallib.starts_with(b"MTLB"));
            } else {
                assert!(
                    matches!(
                        result,
                        Err(FrontEndError::Reject(reason))
                            if reason == format!("unsupported:gradient{derivative}:target")
                    ),
                    "expected {kind} gradient target rejection"
                );
            }
        }
    }
    Ok(())
}
