//! The memory model, thread-local slots, `GetInfo` and the main thread, over fake memory.

use nxkit_hos::{
    Config, FakeMemory, GuestMemory, KResult, Kernel, MainThread, MemoryConfig, Thread, ThreadState,
};
use std::sync::Arc;

const BASE: u64 = 0x10_0000;
const SIZE: u64 = 0x80_0000;
const HEAP: u64 = 0x20_0000;
const STACK: u64 = 0x70_0000;
const ALIAS: u64 = 0x80_0000;
const SCRATCH: u64 = BASE + 0x3_0000;
const MIB2: u64 = 0x20_0000;

struct Rig {
    kernel: Arc<Kernel>,
    mem: Arc<FakeMemory>,
    main: Arc<Thread>,
}

#[allow(clippy::expect_used, reason = "a test helper")]
fn rig() -> Rig {
    let mem = Arc::new(FakeMemory::new(BASE, (SIZE / 4) as usize));
    let config = Config {
        memory: Some(MemoryConfig {
            host: mem.clone(),
            base: BASE,
            size: SIZE,
            front_size: 0x10_0000,
            stack_size: 0x10_0000,
            alias_size: 0x10_0000,
            app_pool: 0x40_0000,
            used_memory: 0x10_0000,
            system_resource: 0x20_0000,
            map_limit: false,
            program_id: 0x0100_7EF0_0011_E000,
            tls: (BASE, 0x1_0000),
            shared: (BASE + 0x2_0000, 0x1_0000),
            main_stack: None,
            modules: Vec::new(),
        }),
        main_thread: Some(MainThread {
            priority: 28,
            core: 1,
            stack_size: 0x10_0000,
        }),
        log: |_| {},
        ..Config::default()
    };
    let kernel = Kernel::try_new(config, mem.clone()).expect("the layout fits");
    let main = kernel.attach_main_thread("main").expect("attach");
    Rig { kernel, mem, main }
}

impl Rig {
    fn svc(&self, number: u32, args: &[u64]) -> [u64; 8] {
        let mut regs = [0u64; 8];
        regs[..args.len()].copy_from_slice(args);
        assert!(self.kernel.dispatch_svc(&self.main, number, &mut regs));
        regs
    }

    fn query(&self, addr: u64) -> (u64, u64, u32, u32, u32) {
        let r = self.svc(0x06, &[SCRATCH, 0, addr]);
        assert_eq!(r[0], 0);
        let m = &self.mem;
        (
            m.load64(SCRATCH).unwrap_or(0),
            m.load64(SCRATCH + 8).unwrap_or(0),
            m.load32(SCRATCH + 16).unwrap_or(99),
            m.load32(SCRATCH + 20).unwrap_or(99),
            m.load32(SCRATCH + 24).unwrap_or(99),
        )
    }
}

fn code(r: KResult) -> u64 {
    u64::from(r.raw())
}

#[test]
fn the_main_thread_takes_its_values_from_the_config() {
    let r = rig();
    assert_eq!(r.main.priority(), 28);
    assert_eq!(
        r.kernel.get_thread_core_mask(&r.main, 0xFFFF_8000),
        Ok((1, 0b111))
    );
}

#[test]
fn the_main_thread_does_not_attach_without_the_npdm_values() {
    let mem = Arc::new(FakeMemory::new(BASE, 16));
    let config = Config {
        log: |_| {},
        ..Config::default()
    };
    let kernel = Kernel::new(config, mem);
    assert_eq!(
        kernel.attach_main_thread("main").err(),
        Some(KResult::INVALID_STATE)
    );
}

#[test]
fn a_thread_has_its_own_thread_local_region_with_its_id() {
    let r = rig();
    let tlr = r.main.tlr();
    assert_ne!(tlr, 0);
    assert_eq!(r.main.tls() + 0x2000, tlr);
    assert_eq!(r.mem.load32(tlr + 0x110), Some(r.main.id() as u32));
    assert_eq!(r.mem.load64(tlr + 0x1F8), Some(tlr + 0x200));
}

#[test]
fn an_exited_thread_has_no_thread_local_region() {
    let r = rig();
    let h = r
        .kernel
        .create_thread(nxkit_hos::ThreadSpec {
            name: "worker".into(),
            entry: 0,
            priority: 30,
            core: 0,
            stack_size: 0,
            body: Box::new(|| {}),
        })
        .expect("create");
    r.kernel.start_thread(h).expect("start");
    let t = r.kernel.thread_by_handle(h).expect("thread");
    while t.state() != ThreadState::Exited {
        std::thread::yield_now();
    }
    assert_eq!((t.tls(), t.tlr()), (0, 0));
}

#[test]
fn the_heap_grows_shrinks_and_is_checked() {
    let r = rig();
    assert_eq!(r.svc(0x01, &[0, MIB2 + 1])[0], code(KResult::INVALID_SIZE));
    assert_eq!(r.svc(0x01, &[0, 6 * MIB2])[0], code(KResult::OUT_OF_MEMORY));
    assert_eq!(r.svc(0x01, &[0, 2 * MIB2])[0], code(KResult::LIMIT_REACHED));
    let ok = r.svc(0x01, &[0, MIB2]);
    assert_eq!((ok[0], ok[1]), (0, HEAP));
    assert_eq!(r.query(HEAP), (HEAP, MIB2, 5, 0, 3));
    assert_eq!(r.query(HEAP + MIB2).2, 0);
    assert_eq!(r.svc(0x01, &[0, 0])[0], 0);
    assert_eq!(r.query(HEAP).2, 0);
}

#[test]
fn query_memory_answers_every_address() {
    let r = rig();
    // The free heap region and the free stack and alias regions touch, so they are one block.
    assert_eq!(r.query(STACK + 0x1000), (HEAP, 0x70_0000, 0, 0, 0));
    assert_eq!(r.query(BASE), (BASE, 0x1_0000, 0xC, 0, 3));
    assert_eq!(r.query(0).2, 0x10);
}

#[test]
fn map_memory_makes_a_second_view_and_unmap_ends_it() {
    let r = rig();
    r.svc(0x01, &[0, MIB2]);
    let src = HEAP + 0x1000;
    let dst = STACK;
    assert!(r.mem.store32(src, 0xABCD));
    assert_eq!(r.svc(0x04, &[dst, src, 0x2000])[0], 0);
    assert_eq!(r.mem.load32(dst), Some(0xABCD));
    assert_eq!(r.query(dst), (dst, 0x2000, 0xB, 0, 3));
    assert_eq!(r.query(src), (src, 0x2000, 5, 1, 0));
    assert_eq!(
        r.svc(0x02, &[src, 0x1000, 3])[0],
        code(KResult::INVALID_CURRENT_MEMORY)
    );
    assert_eq!(
        r.svc(0x05, &[dst, src, 0x1000])[0],
        code(KResult::INVALID_CURRENT_MEMORY)
    );
    assert_eq!(r.svc(0x05, &[dst, src, 0x2000])[0], 0);
    assert_eq!(r.query(dst).2, 0);
    assert_eq!(r.query(src), (HEAP, MIB2, 5, 0, 3));
}

#[test]
fn map_memory_refuses_a_destination_outside_the_stack_region() {
    let r = rig();
    r.svc(0x01, &[0, MIB2]);
    assert_eq!(
        r.svc(0x04, &[ALIAS, HEAP, 0x1000])[0],
        code(KResult::INVALID_MEMORY_REGION)
    );
    assert_eq!(
        r.svc(0x04, &[STACK + 1, HEAP, 0x1000])[0],
        code(KResult::INVALID_ADDRESS)
    );
}

#[test]
fn physical_memory_maps_only_in_the_alias_region() {
    let r = rig();
    assert_eq!(
        r.svc(0x2C, &[HEAP, 0x1000])[0],
        code(KResult::INVALID_MEMORY_REGION)
    );
    assert_eq!(r.svc(0x2C, &[ALIAS, 0x4000])[0], 0);
    assert_eq!(r.query(ALIAS), (ALIAS, 0x4000, 5, 0, 3));
    assert_eq!(r.svc(0x2D, &[ALIAS, 0x4000])[0], 0);
    assert_eq!(r.query(ALIAS).2, 0);
}

#[test]
fn transfer_memory_locks_the_pages_until_the_handle_closes() {
    let r = rig();
    r.svc(0x01, &[0, MIB2]);
    let t = r.svc(0x15, &[0, HEAP, 0x2000, 3]);
    assert_eq!(t[0], 0);
    let handle = t[1] as u32;
    assert_eq!(r.query(HEAP), (HEAP, 0x2000, 5, 1, 3));
    assert_eq!(
        r.svc(0x01, &[0, 0])[0],
        code(KResult::INVALID_CURRENT_MEMORY)
    );
    assert_eq!(r.kernel.transfer_memory(handle), Ok((HEAP, 0x2000)));
    assert_eq!(r.svc(0x16, &[u64::from(handle)])[0], 0);
    assert_eq!(r.query(HEAP), (HEAP, MIB2, 5, 0, 3));
}

#[test]
fn a_service_makes_shared_memory_and_the_game_maps_it() {
    let r = rig();
    let (handle, addr) =
        nxkit_abi::kernel::Kernel::create_shared_memory(&*r.kernel, 0x1000).expect("shared memory");
    assert!((BASE + 0x2_0000..BASE + 0x3_0000).contains(&addr));
    assert!(r.mem.store32(addr, 7));
    let at = ALIAS + 0x4000;
    assert_eq!(r.svc(0x13, &[u64::from(handle), at, 0x1000, 3])[0], 0);
    assert_eq!(r.mem.load32(at), Some(7));
    assert_eq!(r.query(at), (at, 0x1000, 6, 0, 3));
    assert_eq!(
        r.svc(0x13, &[u64::from(handle), at, 0x2000, 3])[0],
        code(KResult::INVALID_SIZE)
    );
    assert_eq!(r.svc(0x14, &[u64::from(handle), at, 0x1000])[0], 0);
    assert_eq!(r.query(at).2, 0);
}

#[test]
fn get_info_answers_from_the_memory_model() {
    let r = rig();
    let info = |id: u64, sub: u64| r.svc(0x29, &[0, id, 0xFFFF_8001, sub]);
    assert_eq!(info(0, 0)[1], 0b111);
    assert_eq!((info(4, 0)[1], info(5, 0)[1]), (HEAP, 0x50_0000));
    assert_eq!((info(2, 0)[1], info(3, 0)[1]), (ALIAS, 0x10_0000));
    assert_eq!((info(14, 0)[1], info(15, 0)[1]), (STACK, 0x10_0000));
    assert_eq!((info(12, 0)[1], info(13, 0)[1]), (BASE, SIZE));
    assert_eq!((info(6, 0)[1], info(7, 0)[1]), (0x40_0000, 0x10_0000));
    assert_eq!((info(16, 0)[1], info(17, 0)[1]), (0x20_0000, 0x40_0000));
    assert_eq!(info(18, 0)[1], 0x0100_7EF0_0011_E000);
    assert_eq!(info(23, 0)[1], 1);
    assert_eq!(info(24, 0)[1], 95);
    assert_eq!(info(33, 0)[1], 0x1000);
    assert_eq!(info(11, 4)[0], code(KResult::INVALID_COMBINATION));
    assert_eq!(info(10, 0)[0], code(KResult::INVALID_ENUM_VALUE));
    assert_eq!(info(500, 0)[0], code(KResult::INVALID_ENUM_VALUE));
}

#[test]
fn an_unknown_svc_is_not_answered() {
    let r = rig();
    let mut regs = [0u64; 8];
    assert!(!r.kernel.dispatch_svc(&r.main, 0x7777, &mut regs));
}

#[test]
fn a_kernel_without_memory_leaves_the_memory_svcs_to_the_host() {
    let mem = Arc::new(FakeMemory::new(BASE, 16));
    let kernel = Kernel::new(Config::default(), mem);
    let main = kernel.attach_current_thread("main", 30, 0).expect("attach");
    let mut regs = [0u64; 8];
    assert!(!kernel.dispatch_svc(&main, 0x01, &mut regs));
    assert!(!kernel.dispatch_svc(&main, 0x21, &mut regs));
    assert_eq!(main.tlr(), 0);
}

#[test]
fn connect_to_named_port_checks_its_name() {
    let r = rig();
    assert!(r.mem.store32(SCRATCH, u32::from_le_bytes(*b"sm:\0")));
    assert_eq!(r.svc(0x1F, &[0, SCRATCH])[0], code(KResult::NOT_FOUND));
    for i in 0..3 {
        assert!(r.mem.store32(SCRATCH + 4 * i, u32::from_le_bytes(*b"abcd")));
    }
    assert_eq!(r.svc(0x1F, &[0, SCRATCH])[0], code(KResult::OUT_OF_RANGE));
    assert_eq!(r.svc(0x1F, &[0, 0])[0], code(KResult::INVALID_POINTER));
}

#[test]
fn a_bad_layout_is_an_error_that_names_it() {
    let mem = Arc::new(FakeMemory::new(BASE, 16));
    let config = Config {
        memory: Some(MemoryConfig {
            host: mem.clone(),
            base: BASE,
            size: SIZE,
            front_size: 0x10_0000,
            stack_size: 0x10_0000,
            alias_size: 0x10_0000,
            app_pool: 0,
            used_memory: 0,
            system_resource: 0,
            map_limit: false,
            program_id: 0,
            tls: (BASE + 0x10_0000, 0x1_0000),
            shared: (BASE, 0x1_0000),
            main_stack: None,
            modules: Vec::new(),
        }),
        ..Config::default()
    };
    let err = Kernel::try_new(config, mem).err().unwrap_or_default();
    assert!(err.contains("tls area"), "{err}");
}

#[test]
fn a_read_end_can_be_duplicated_and_both_ends_see_the_signal() {
    let r = rig();
    let (write, read) = r.kernel.create_event().expect("event");
    let dup = r.kernel.duplicate_readable_handle(read).expect("duplicate");
    assert_ne!(dup, read);
    assert_eq!(r.kernel.is_signaled(dup), Ok(false));
    r.kernel.signal_event(write).expect("signal");
    assert_eq!(r.kernel.is_signaled(read), Ok(true));
    assert_eq!(r.kernel.is_signaled(dup), Ok(true));
    assert_eq!(
        r.kernel.duplicate_readable_handle(write).err(),
        Some(KResult::INVALID_HANDLE)
    );
}

#[test]
fn a_system_event_is_not_bound_without_the_sdk_lookup() {
    let r = rig();
    let err = r.kernel.bind_system_event(0x1000, false).unwrap_err();
    assert!(err.contains("lookup is not set"), "{err}");
    assert_eq!(r.kernel.system_event_binding(0x1000), 0);
    assert!(!r.kernel.signal_bound_system_event(0x1000));
    assert_eq!(r.kernel.clear_system_event(0x1000), Ok(()));
}
