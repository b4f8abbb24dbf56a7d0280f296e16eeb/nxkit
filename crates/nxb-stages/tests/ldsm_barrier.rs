//! Raw front-end coverage for encoded Ampere LDSM subgroup barriers.
#![cfg(feature = "air-front")]

use nxb_tool::{FrontEnd, FrontEndError, airfront::RawFrontEnd};
use nxb_xlate::{
    job::parse_jobs,
    msl::{Context, translate},
};

#[test]
fn ldsm_reaches_macos_raw_compiler() {
    // Same fixture as nxb-xlate/src/msl/tensor_tests.rs::ldsm_reads_lie_between_subgroup_barriers.
    const S2R: (u64, u64) = (0x0000_0000_0009_7919, 0x000e_2600_0000_2100);
    const IMAD: (u64, u64) = (0x0000_0010_0902_7824, 0x000f_e200_078e_02ff);
    const LDSM: (u64, u64) = (0x0000_0000_0224_783b, 0x000e_2200_0000_0200);
    const STS: (u64, u64) = (0x0000_0824_0900_8388, 0x000f_e800_0000_c800);
    // Ampere EXIT with an always-true predicate and its end-of-thread bit.
    const PT: u64 = 7 << 12;
    const EXIT_THREAD: u64 = 1 << 23;
    const EXIT: (u64, u64) = (0x94d | PT | EXIT_THREAD, 0);
    const WAIT_ALL_SCOREBOARDS: u64 = 0x3f << 52;
    let job =
        parse_jobs(r#"[{"key":"k","stage":"cs","arch":"sm86","local":[32,1,1],"shared":4096}]"#)
            .expect("fixture job")
            .remove(0);
    let code: Vec<u8> = [S2R, IMAD, LDSM, STS]
        .into_iter()
        .map(|(lo, hi)| (lo, hi | WAIT_ALL_SCOREBOARDS))
        .chain([EXIT])
        .flat_map(|(lo, hi)| [lo.to_le_bytes(), hi.to_le_bytes()].concat())
        .collect();
    let result = translate(&job, &code, &Context::default()).expect("fixture translates");
    let msl = &result.libraries[0].msl;
    const BARRIER: &str = "simdgroup_barrier(mem_flags::mem_threadgroup);";
    assert_eq!(msl.matches(BARRIER).count(), 2);
    let raw = RawFrontEnd::new(false)
        .compile(msl.as_bytes())
        .expect("macOS raw library");
    assert!(raw.metallib.starts_with(b"MTLB"));
    match RawFrontEnd::new(true).compile(msl.as_bytes()) {
        Err(FrontEndError::Reject(reason)) => {
            assert_eq!(reason, "unsupported:simdgroup-barrier:target")
        }
        other => panic!("expected target rejection, got {other:?}"),
    }
}
