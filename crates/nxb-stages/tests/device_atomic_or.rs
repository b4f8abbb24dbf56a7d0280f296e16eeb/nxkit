//! An authored Maxwell shader, translated and compiled without executing the guest code.
#![cfg(feature = "air-front")]

use nxb_tool::FrontEnd;
use nxb_tool::airfront::RawFrontEnd;
use nxb_xlate::{
    accessor::Accessor,
    attr::Stage,
    job::parse_jobs,
    maxwell::{
        decoder::{decode, dump},
        enums::{AtomOp, AtomSize},
        insts::InstAtom,
    },
    msl::{Context, translate},
};

#[test]
fn maxwell_device_atomic_or_builds_raw_libraries() {
    const PT: u64 = 7 << 16;
    let mov = |r, v: u32| 0x0100_0000_0000_0000 | PT | 15 << 12 | r | (u64::from(v) << 20);
    let atom =
        0xed00_0000_0000_0000 | (AtomOp::Or.0 as u64) << 52 | 1 << 48 | PT | 8 << 8 | 10 << 20 | 11;
    let ins = InstAtom(atom);
    assert_eq!(
        (ins.op(), ins.size(), ins.e()),
        (AtomOp::Or, AtomSize::U32, true)
    );
    // Authored toy address. Store the pre-OR value so the result remains live.
    let store = 0xeed8_0000_0000_0000 | 4 << 48 | 1 << 45 | 4 << 20 | PT | 8 << 8 | 11;
    let exit = 0xe300_0000_0000_0000 | PT | 15;
    let words = [
        0,
        mov(8, 0x100),
        mov(9, 0),
        mov(10, 4),
        0,
        atom,
        store,
        exit,
    ];
    let code: Vec<u8> = words.into_iter().flat_map(u64::to_le_bytes).collect();
    let job = parse_jobs(r#"[{"key":"authored_device_or","stage":"cs","local":[1,1,1]}]"#)
        .unwrap()
        .remove(0);
    let (ops, _) = decode(Stage::Compute, &mut Accessor::new(&job), &code, 0).unwrap();
    assert!(dump(&ops).contains("Atom"));
    let result = translate(&job, &code, &Context::default()).unwrap();
    let lib = result
        .libraries
        .iter()
        .find(|lib| lib.suffix == "_b")
        .unwrap();
    assert!(
        lib.msl
            .contains("reinterpret_cast<device atomic_uint*>(&(*reinterpret_cast<device uint*>")
    );
    assert!(lib.msl.contains("atomic_fetch_or_explicit"));
    for ios in [false, true] {
        let raw = RawFrontEnd::new(ios).compile(lib.msl.as_bytes()).unwrap();
        assert!(raw.metallib.starts_with(b"MTLB"));
    }
}
