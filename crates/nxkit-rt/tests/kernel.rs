//! The kernel service over the C ABI, with guest memory that is host memory. One test owns the global kernel.

use nxkit_rt::{
    NxkitKernelConfig, Service, nxkit_hle_find, nxkit_init, nxkit_kernel_dump_stats,
    nxkit_kernel_init, nxkit_kernel_thread_tls, nxkit_svc,
};
use std::ptr::null_mut;

fn init(cfg: &NxkitKernelConfig) -> i32 {
    // SAFETY: a valid configuration without rules. The addresses of the test are live host memory.
    unsafe { nxkit_kernel_init(cfg) }
}

fn svc(number: u32, regs: &mut [u64; 8]) -> i32 {
    // SAFETY: `regs` is eight live registers.
    unsafe { nxkit_svc(number, regs.as_mut_ptr()) }
}

#[test]
fn svc_calls_reach_the_kernel_only_when_the_service_is_on() {
    let mut regs = [0u64; 8];
    assert_eq!(svc(0x45, &mut regs), 0, "before init");
    let cfg = NxkitKernelConfig {
        thread_entry: None,
        user: null_mut(),
        core_rules: 0,
        roles: std::ptr::null(),
        role_count: 0,
        main_thread_priority: 44,
        main_thread_core: 0,
        main_thread_stack_size: 0x10_0000,
        process_id: 0,
        process_exit: None,
        memory: std::ptr::null(),
    };
    assert_eq!(init(&cfg), 0);
    assert_eq!(init(&cfg), 1, "a second kernel is refused");
    assert_eq!(svc(0x45, &mut regs), 0, "the service is off");

    nxkit_init(Service::Kernel.bit());
    assert_eq!(svc(0x45, &mut regs), 1);
    let (write, read) = (regs[1], regs[2] as u32);
    assert_eq!(regs[0], 0);
    assert_eq!(
        svc(0x7777, &mut [0; 8]),
        0,
        "a number nxkit does not answer"
    );

    // WaitSynchronization on an unsignalled event times out, after the write end signals it the wait returns 0.
    let handles = [read];
    let mut regs = [0, handles.as_ptr() as u64, 1, 1_000_000, 0, 0, 0, 0];
    assert_eq!(svc(0x18, &mut regs), 1);
    assert_eq!(regs[0] as u32, 0xEA01);
    let mut regs = [write, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(svc(0x11, &mut regs), 1);
    let mut regs = [0, handles.as_ptr() as u64, 1, 1_000_000, 0, 0, 0, 0];
    assert_eq!(svc(0x18, &mut regs), 1);
    assert_eq!((regs[0], regs[1]), (0, 0));

    // A lock word in host memory: ArbitrateUnlock of a free word is a success.
    let word = Box::new(0u32);
    let mut regs = [&*word as *const u32 as u64, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(svc(0x1B, &mut regs), 1);
    assert_eq!(regs[0], 0);

    let mut buf = [0 as std::ffi::c_char; 512];
    // SAFETY: `buf` has 512 writable bytes.
    let need = unsafe { nxkit_kernel_dump_stats(buf.as_mut_ptr(), buf.len()) };
    assert!(need > 0 && need < 512);

    // The SDK take-overs belong to the kernel service.
    let find = |name: &str| {
        let c = std::ffi::CString::new(name).unwrap_or_default();
        // SAFETY: `c` is a NUL-terminated string that lives during the call.
        unsafe { nxkit_hle_find(c.as_ptr()) }
    };
    assert!(!find("getentropy").is_null());
    nxkit_init(0);
    assert!(find("getentropy").is_null(), "the service is off");
    nxkit_init(Service::Kernel.bit());

    // The main thread has no guest memory model here, so it has no thread-local region.
    // SAFETY: both pointers are NULL.
    assert_eq!(
        unsafe { nxkit_kernel_thread_tls(std::ptr::null_mut(), std::ptr::null_mut()) },
        -1
    );
}
