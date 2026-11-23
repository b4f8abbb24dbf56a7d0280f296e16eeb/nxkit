//! Encoded Maxwell TLDS with a bound texel buffer rather than a 1D texture.
#![cfg(feature = "air-front")]

use nxb_tool::{FrontEnd, FrontEndError, airfront::RawFrontEnd};
use nxb_xlate::{
    job::parse_jobs,
    maxwell::{enums::TldsTarget, insts::InstTlds},
    msl::{Context, translate},
};

#[test]
fn maxwell_tlds_reads_one_bound_texel_buffer_index() {
    const PT: u64 = 7 << 16;
    const EXIT: u64 = 0xe300_0000_0000_000f | PT;
    const MOV32I: u64 = 0x0100_0000_0000_0000 | PT | 15 << 12;
    const TLDS: u64 = 0xda00_0000_0000_0000 | PT;
    const COORD: u64 = 8;
    const HANDLE: u64 = 8;
    let target = TldsTarget::Texture1DLodZero;
    let ins = TLDS | COORD << 8 | 255 << 28 | HANDLE << 36 | (target.0 as u64) << 53;
    let decoded = InstTlds(ins);
    assert_eq!(decoded.target(), target);
    assert_eq!(decoded.src_a(), COORD as i32);
    assert_eq!(decoded.tid_b(), HANDLE as i32);
    let mut header = [0u32; 20];
    header[0] = 5 << 10;
    header[18] = 1;
    let mut code: Vec<u8> = header.into_iter().flat_map(u32::to_le_bytes).collect();
    code.extend(
        [0, MOV32I | COORD | 4 << 20, ins, EXIT]
            .into_iter()
            .flat_map(u64::to_le_bytes),
    );
    let job = parse_jobs(r#"[{"key":"authored_tlds_buffer","stage":"ps","tex":{"0":"buffer"}}]"#)
        .unwrap()
        .remove(0);
    let result = translate(&job, &code, &Context::default()).unwrap();
    let lib = result.libraries.iter().find(|l| l.suffix == "_b").unwrap();
    let msl = &lib.msl;
    assert!(msl.contains("texture_buffer<float>"), "{msl}");
    assert!(msl.contains(".read(uint(4))"), "{msl}");
    let mac = RawFrontEnd::new(false);
    let ios = RawFrontEnd::new(true);
    for (front, admits) in [(&mac, mac.id().ends_with("+macosx26")), (&ios, false)] {
        let result = front.compile(msl.as_bytes());
        if admits {
            assert!(result.unwrap().metallib.starts_with(b"MTLB"));
        } else {
            let Err(FrontEndError::Reject(reason)) = result else {
                panic!("expected texel buffer target rejection");
            };
            assert_eq!(reason, "unsupported:texture_buffer:target");
        }
    }
}
