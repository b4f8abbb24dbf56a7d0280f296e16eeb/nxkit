//! An authored Maxwell vertex shader writing a physically indexed attribute.
#![cfg(feature = "air-front")]

mod shader_fixture;

use nxb_xlate::{job::parse_jobs, maxwell::insts::InstAst};
use shader_fixture::check_shader;

#[test]
fn maxwell_indexed_output_builds_raw_libraries() {
    const PT: u64 = 7 << 16;
    const EXIT: u64 = 0xe300_0000_0000_0000 | PT | 15;
    // ALD R8, a[0x2fc] (vertex ID); MOV R0, 1.0; AST a[R8], R0; EXIT.
    let load_index = 0xefd8_0000_0000_0000 | 0x2fc << 20 | 0xff << 8 | 0xff << 39 | PT | 8;
    let mov = 0x0100_0000_0000_0000 | PT | 15 << 12 | u64::from(1.0f32.to_bits()) << 20;
    let store = 0xeff0_0000_0000_0000 | 8 << 8 | 0xff << 39 | PT;
    assert!(InstAst(store).phys());
    let mut header = [0u32; 20];
    header[0] = 1 << 10;
    let mut code: Vec<u8> = header.into_iter().flat_map(u32::to_le_bytes).collect();
    code.extend(
        [0, load_index, mov, store, 0, EXIT]
            .into_iter()
            .flat_map(u64::to_le_bytes),
    );
    let job = parse_jobs(r#"[{"key":"authored_indexed_output","stage":"vs"}]"#)
        .unwrap()
        .remove(0);
    let msl = check_shader(&job, &code).unwrap();
    assert!(msl.contains("? out.loc0 :"), "{}", msl);
    assert!(msl.contains("out.loc15)["), "{}", msl);
}
