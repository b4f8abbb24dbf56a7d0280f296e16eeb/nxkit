//! Module data can back a stack alias without losing its original memory state.
use super::*;

const MODULE: u64 = BASE + 0x4_0000;
const DATA: u64 = MODULE + 0x2000;
const BSS: u64 = DATA + 0x1000;

fn module_rig() -> Rig {
    let mem = Arc::new(FakeMemory::new(BASE, (SIZE / 4) as usize));
    module_build(mem.clone(), mem)
}

fn module_build(host: Arc<dyn HostSpace>, mem: Arc<FakeMemory>) -> Rig {
    build_modules(
        host,
        mem.clone(),
        mem,
        vec![nxkit_hos::Module {
            base: MODULE,
            text: 0x1000,
            rodata: 0x1000,
            data: 0x1000,
            bss: 0x8000,
        }],
    )
}

#[test]
fn bss_stack_mapping_restores_code_data_and_allows_reuse() {
    let r = module_rig();
    assert_eq!(r.query(BSS).2, 4);
    assert!(r.mem.store32(BSS, 0x1234));
    for _ in 0..2 {
        assert_eq!(r.svc(0x04, &[STACK, BSS, 0x8000])[0], 0);
        assert_eq!(r.mem.load32(STACK), Some(0x1234));
        assert_eq!(r.query(BSS), (BSS, 0x8000, 4, 1, 0));
        assert_eq!(r.query(STACK), (STACK, 0x8000, 0xB, 0, 3));
        assert_eq!(
            r.svc(0x04, &[STACK + 0x8000, BSS, 0x8000])[0],
            code(KResult::INVALID_CURRENT_MEMORY)
        );
        assert_eq!(r.svc(0x05, &[STACK, BSS, 0x8000])[0], 0);
        assert_eq!(r.query(BSS), (DATA, 0x9000, 4, 0, 3));
        assert_eq!(r.query(STACK).2, 0);
    }
}

#[test]
fn executable_and_read_only_sources_remain_rejected() {
    let r = module_rig();
    for src in [MODULE, MODULE + 0x1000, BASE, BASE + 0x2_0000] {
        assert_eq!(
            r.svc(0x04, &[STACK, src, 0x1000])[0],
            code(KResult::INVALID_CURRENT_MEMORY)
        );
    }
    assert_eq!(r.svc(0x02, &[BSS, 0x8000, 1])[0], 0);
    assert_eq!(
        r.svc(0x04, &[STACK, BSS, 0x8000])[0],
        code(KResult::INVALID_CURRENT_MEMORY)
    );
    assert_eq!(r.query(STACK).2, 0);
}

#[test]
fn attributed_code_data_cannot_back_a_stack_alias() {
    for attribute in [8, 0x10] {
        let r = module_rig();
        assert_eq!(r.svc(0x03, &[BSS, 0x8000, attribute, attribute])[0], 0);
        assert_eq!(
            r.svc(0x04, &[STACK, BSS, 0x8000])[0],
            code(KResult::INVALID_CURRENT_MEMORY)
        );
        assert_eq!(r.query(BSS), (BSS, 0x8000, 4, attribute as u32, 3));
        assert_eq!(r.query(STACK).2, 0);
    }
}

#[test]
fn copied_code_data_stack_writes_back_without_changing_neighbors() {
    let _pins = exclusive_pins();
    let mem = Arc::new(FakeMemory::new(BASE, (SIZE / 4) as usize));
    let rec = Arc::new(Rec {
        mem: mem.clone(),
        calls: Mutex::new(Vec::new()),
    });
    let r = module_build(rec.clone(), mem);
    pinned::pin(BSS, BSS + 0x8000);
    assert!(r.mem.store32(BSS - 4, 0x55));
    assert_eq!(r.svc(0x04, &[STACK, BSS, 0x8000])[0], 0);
    assert!(r.mem.store32(STACK, 0x77));
    assert_eq!(r.svc(0x05, &[STACK, BSS, 0x8000])[0], 0);
    assert_eq!(r.mem.load32(BSS), Some(0x77));
    assert_eq!(r.mem.load32(BSS - 4), Some(0x55));
    assert_eq!(r.query(BSS), (DATA, 0x9000, 4, 0, 3));
    assert_eq!(
        rec.calls(),
        vec![
            Call::Copy(STACK, BSS, 0x8000),
            Call::Copy(BSS, STACK, 0x8000),
            Call::Give(STACK, STACK + 0x8000)
        ]
    );
    assert!(pinned::unpin(BSS, BSS + 0x8000));
}

#[test]
fn mixed_code_data_and_normal_source_is_rejected() {
    let mem = Arc::new(FakeMemory::new(BASE, (SIZE / 4) as usize));
    let r = build_modules(
        mem.clone(),
        mem.clone(),
        mem,
        vec![nxkit_hos::Module {
            base: HEAP - 0x1000,
            text: 0,
            rodata: 0,
            data: 0x1000,
            bss: 0,
        }],
    );
    assert_eq!(r.svc(0x01, &[0, MIB2])[0], 0);
    assert_eq!(
        r.svc(0x04, &[STACK, HEAP - 0x1000, 0x2000])[0],
        code(KResult::INVALID_CURRENT_MEMORY)
    );
    assert_eq!(r.query(STACK).2, 0);
}
