//! The memory model, thread-local slots, `GetInfo` and the main thread, over fake memory.

use nxkit_hos::pinned;
use nxkit_hos::{
    Config, FakeMemory, GuestMemory, HostSpace, KResult, Kernel, MainThread, MemoryConfig, Thread,
    ThreadState,
};
use std::sync::{Arc, Mutex, MutexGuard};

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

fn rig() -> Rig {
    let mem = Arc::new(FakeMemory::new(BASE, (SIZE / 4) as usize));
    build(mem.clone(), mem.clone(), mem)
}

#[allow(clippy::expect_used, reason = "a test helper")]
fn build(host: Arc<dyn HostSpace>, guest: Arc<dyn GuestMemory>, mem: Arc<FakeMemory>) -> Rig {
    build_modules(host, guest, mem, Vec::new())
}

#[allow(clippy::expect_used, reason = "a test helper")]
fn build_modules(
    host: Arc<dyn HostSpace>,
    guest: Arc<dyn GuestMemory>,
    mem: Arc<FakeMemory>,
    modules: Vec<nxkit_hos::Module>,
) -> Rig {
    let config = Config {
        memory: Some(MemoryConfig {
            host,
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
            modules,
        }),
        main_thread: Some(MainThread {
            priority: 28,
            core: 1,
            stack_size: 0x10_0000,
        }),
        log: |_| {},
        ..Config::default()
    };
    let kernel = Kernel::try_new(config, guest).expect("the layout fits");
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

/// One call of the kernel on the host's pages, as the recording host saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    /// `remap(dst, src, len)`: the pages are shared.
    Remap(u64, u64, u64),
    /// `copy(dst, src, len)`.
    Copy(u64, u64, u64),
    /// `drop_view(addr, len)`.
    Drop(u64, u64),
    /// A run of `release` that was given back.
    Give(u64, u64),
    /// A run of `release` that a pin keeps: zeroed in place.
    Keep(u64, u64),
}

/// Fake memory that records the calls that move pages. It asks the pinned set like the host memory does, and cuts a
/// `release` at the pinned pages like the host memory does (`pinned::split`). Pages are 4 KiB.
struct Rec {
    mem: Arc<FakeMemory>,
    calls: Mutex<Vec<Call>>,
}

impl Rec {
    fn note(&self, c: Call) {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(c);
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn forget(&self) {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

impl GuestMemory for Rec {
    fn load32(&self, addr: u64) -> Option<u32> {
        self.mem.load32(addr)
    }
    fn load64(&self, addr: u64) -> Option<u64> {
        self.mem.load64(addr)
    }
    fn store32(&self, addr: u64, value: u32) -> bool {
        self.mem.store32(addr, value)
    }
    fn cas32(&self, addr: u64, current: u32, new: u32) -> Option<Result<u32, u32>> {
        self.mem.cas32(addr, current, new)
    }
}

impl HostSpace for Rec {
    fn page_size(&self) -> u64 {
        0x1000
    }
    fn protect(&self, addr: u64, len: u64, perm: u32) -> bool {
        self.mem.protect(addr, len, perm)
    }
    fn release(&self, addr: u64, len: u64) -> bool {
        pinned::split(addr, addr + len, 0x1000)
            .into_iter()
            .all(|(s, e, pinned)| {
                self.note(if pinned {
                    Call::Keep(s, e)
                } else {
                    Call::Give(s, e)
                });
                self.mem.zero(s, e - s)
            })
    }
    fn zero(&self, addr: u64, len: u64) -> bool {
        self.mem.zero(addr, len)
    }
    fn remap(&self, dst: u64, src: u64, len: u64, perm: u32) -> bool {
        self.note(Call::Remap(dst, src, len));
        self.mem.remap(dst, src, len, perm)
    }
    fn copy(&self, dst: u64, src: u64, len: u64) -> bool {
        self.note(Call::Copy(dst, src, len));
        self.mem.copy(dst, src, len)
    }
    fn can_remap(&self, addr: u64, len: u64) -> bool {
        !pinned::overlaps(addr, len)
    }
    fn drop_view(&self, addr: u64, len: u64) -> bool {
        // like the host memory: the pages of the view go, the pages they showed stay. The fake has no second view.
        self.note(Call::Drop(addr, len));
        self.mem.zero(addr, len)
    }
}

/// The pinned set is a static of the process and the tests run in parallel: a test that pins takes this lock, and
/// the set is empty when it starts and when it ends.
static PINS: Mutex<()> = Mutex::new(());

struct Exclusive {
    _lock: MutexGuard<'static, ()>,
}

impl Drop for Exclusive {
    fn drop(&mut self) {
        pinned::clear();
    }
}

fn exclusive_pins() -> Exclusive {
    let g = PINS.lock().unwrap_or_else(|e| e.into_inner());
    pinned::clear();
    Exclusive { _lock: g }
}

fn rig_rec() -> (Rig, Arc<Rec>) {
    let mem = Arc::new(FakeMemory::new(BASE, (SIZE / 4) as usize));
    let rec = Arc::new(Rec {
        mem: mem.clone(),
        calls: Mutex::new(Vec::new()),
    });
    (build(rec.clone(), rec.clone(), mem), rec)
}

#[test]
fn a_mapping_that_shared_its_pages_is_unmapped_by_sharing_although_a_pin_appeared() {
    let _p = exclusive_pins();
    let (r, rec) = rig_rec();
    r.svc(0x01, &[0, MIB2]);
    let (src, dst) = (HEAP + 0x1000, STACK);
    assert!(r.mem.store32(src, 0xABCD));
    assert_eq!(r.svc(0x04, &[dst, src, 0x2000])[0], 0);
    assert_eq!(rec.calls(), vec![Call::Remap(dst, src, 0x2000)]);
    // a buffer over the source appears while the stack view lives
    pinned::pin(src, src + 0x2000);
    rec.forget();
    assert_eq!(r.svc(0x05, &[dst, src, 0x2000])[0], 0);
    // only the view goes: nothing is copied and nothing is zeroed in the source
    assert_eq!(rec.calls(), vec![Call::Drop(dst, 0x2000)]);
    assert_eq!(r.mem.load32(src), Some(0xABCD));
}

#[test]
fn a_mapping_that_copied_its_pages_is_unmapped_by_copying_although_the_pin_went_away() {
    let _p = exclusive_pins();
    let (r, rec) = rig_rec();
    r.svc(0x01, &[0, MIB2]);
    let (src, dst) = (HEAP + 0x1000, STACK);
    pinned::pin(src, src + 0x2000);
    assert!(r.mem.store32(src, 1));
    assert_eq!(r.svc(0x04, &[dst, src, 0x2000])[0], 0);
    assert_eq!(rec.calls(), vec![Call::Copy(dst, src, 0x2000)]);
    // the buffer is gone, and the stack view got a write
    assert!(pinned::unpin(src, src + 0x2000));
    assert!(r.mem.store32(dst, 0x77));
    rec.forget();
    assert_eq!(r.svc(0x05, &[dst, src, 0x2000])[0], 0);
    assert_eq!(
        rec.calls(),
        vec![Call::Copy(src, dst, 0x2000), Call::Give(dst, dst + 0x2000)]
    );
    assert_eq!(r.mem.load32(src), Some(0x77));
}

#[test]
fn map_memory_copies_when_the_source_lies_in_the_set_range_and_shares_when_it_goes() {
    let _p = exclusive_pins();
    let (r, rec) = rig_rec();
    r.svc(0x01, &[0, MIB2]);
    let (src, dst) = (HEAP + 0x1000, STACK);
    pinned::set(HEAP, HEAP + MIB2);
    assert_eq!(r.svc(0x04, &[dst, src, 0x1000])[0], 0);
    pinned::set(0, 0);
    assert_eq!(r.svc(0x05, &[dst, src, 0x1000])[0], 0);
    assert_eq!(r.svc(0x04, &[dst, src, 0x1000])[0], 0);
    assert_eq!(r.svc(0x05, &[dst, src, 0x1000])[0], 0);
    assert_eq!(
        rec.calls(),
        vec![
            Call::Copy(dst, src, 0x1000),
            Call::Copy(src, dst, 0x1000),
            Call::Give(dst, dst + 0x1000),
            Call::Remap(dst, src, 0x1000),
            Call::Drop(dst, 0x1000),
        ]
    );
}

#[test]
fn a_heap_shrink_gives_back_all_but_the_pinned_page() {
    let _p = exclusive_pins();
    let (r, rec) = rig_rec();
    r.svc(0x01, &[0, MIB2]);
    let kept = HEAP + 0x3000;
    pinned::pin(kept, kept + 0x1000);
    assert!(r.mem.store32(kept, 5));
    assert!(r.mem.store32(HEAP, 6));
    rec.forget();
    assert_eq!(r.svc(0x01, &[0, 0])[0], 0);
    assert_eq!(
        rec.calls(),
        vec![
            Call::Give(HEAP, kept),
            Call::Keep(kept, kept + 0x1000),
            Call::Give(kept + 0x1000, HEAP + MIB2),
        ]
    );
    // all of it reads as zero, the kept page included, and the model says the heap is free
    assert_eq!(r.mem.load32(kept), Some(0));
    assert_eq!(r.mem.load32(HEAP), Some(0));
    assert_eq!(r.query(HEAP).2, 0);
    assert_eq!(r.svc(0x01, &[0, MIB2])[0], 0);
    assert_eq!(r.query(HEAP), (HEAP, MIB2, 5, 0, 3));
}

#[test]
fn a_heap_shrink_over_the_set_range_keeps_every_page() {
    let _p = exclusive_pins();
    let (r, rec) = rig_rec();
    r.svc(0x01, &[0, MIB2]);
    pinned::set(HEAP, HEAP + MIB2);
    rec.forget();
    assert_eq!(r.svc(0x01, &[0, 0])[0], 0);
    assert_eq!(rec.calls(), vec![Call::Keep(HEAP, HEAP + MIB2)]);
}

#[test]
fn unmap_physical_memory_gives_back_all_but_the_pinned_page() {
    let _p = exclusive_pins();
    let (r, rec) = rig_rec();
    assert_eq!(r.svc(0x2C, &[ALIAS, 0x4000])[0], 0);
    pinned::pin(ALIAS + 0x1000, ALIAS + 0x2000);
    assert!(r.mem.store32(ALIAS + 0x1000, 9));
    rec.forget();
    assert_eq!(r.svc(0x2D, &[ALIAS, 0x4000])[0], 0);
    assert_eq!(
        rec.calls(),
        vec![
            Call::Give(ALIAS, ALIAS + 0x1000),
            Call::Keep(ALIAS + 0x1000, ALIAS + 0x2000),
            Call::Give(ALIAS + 0x2000, ALIAS + 0x4000),
        ]
    );
    assert_eq!(r.query(ALIAS).2, 0);
    assert_eq!(r.svc(0x2C, &[ALIAS, 0x4000])[0], 0);
    assert_eq!(r.mem.load32(ALIAS + 0x1000), Some(0));
}

#[path = "memory/code_data.rs"]
mod code_data;
