//! An authored Maxwell fragment shader using the hardware sample index.
#![cfg(feature = "air-front")]

mod shader_fixture;

use nxb_xlate::{
    job::parse_jobs,
    maxwell::{enums::PixMode, insts::InstPixld},
};
use shader_fixture::check_shader;

#[test]
fn maxwell_pixld_myindex_builds_raw_libraries() {
    const PT: u64 = 7;
    const RZ: u64 = 255;
    const PIXLD: u64 = 0xefe8_0000_0000_0000;
    const EXIT: u64 = 0xe300_0000_0000_000f | PT << 16;
    const FRAGMENT_STAGE: u32 = 5 << 10;
    const RT0_X: u32 = 1;
    // PIXLD.MYINDEX R0, RZ (immediate 0, no predicate output); EXIT.
    let pixld = PIXLD | PT << 16 | RZ << 8 | (PixMode::MyIndex.0 as u64) << 31 | PT << 45;
    let op = InstPixld(pixld);
    assert!(op.pix_mode() == PixMode::MyIndex);
    assert_eq!(op.pred(), PT as i32);
    assert_eq!(
        (op.dest(), op.src_a(), op.imm8(), op.dest_pred()),
        (0, RZ as i32, 0, PT as i32)
    );
    let mut header = [0u32; 20];
    header[0] = FRAGMENT_STAGE;
    header[18] = RT0_X; // Keep R0 live as the first component of RT0.
    let mut code: Vec<u8> = header.into_iter().flat_map(u32::to_le_bytes).collect();
    code.extend([0, pixld, EXIT, 0].into_iter().flat_map(u64::to_le_bytes));
    let job = parse_jobs(r#"[{"key":"authored_sample_id","stage":"ps"}]"#)
        .unwrap()
        .remove(0);
    let msl = check_shader(&job, &code).unwrap();
    assert!(msl.contains("uint gl_SampleID [[sample_id]];"), "{}", msl);
    let value = msl
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_suffix(" = as_type<float>(as_type<int>(in.gl_SampleID));")
        })
        .unwrap();
    assert!(msl.contains(&format!("out.c0.x = {value};")), "{}", msl);
}
