//! The C ABI as the host sees it: init, find, the header and the service bits.
//!
//! One test owns the global mask, so the tests of this file do not race on it.

use std::ffi::CString;

use nxkit_rt::{EXPORTS, Service, nxkit_hle_find, nxkit_init};

fn find(name: &str) -> *const std::ffi::c_void {
    let c = CString::new(name).unwrap_or_default();
    // SAFETY: `c` is a NUL-terminated string that lives during the call.
    unsafe { nxkit_hle_find(c.as_ptr()) }
}

#[test]
fn init_is_repeatable_thread_safe_and_the_last_mask_wins() {
    assert!(find("_ZN2nn2fs5MountEPKc").is_null(), "before init");
    nxkit_init(0);
    assert!(find("_ZN2nn2fs5MountEPKc").is_null(), "empty mask");
    nxkit_init(Service::all_bits());
    nxkit_init(0);
    assert!(
        find("_ZN2nn2fs5MountEPKc").is_null(),
        "second init replaces the first"
    );
    nxkit_init(u64::MAX);
    assert!(find("").is_null(), "no service has functions yet");
    nxkit_init(0);
    init_from_many_threads();
}

fn init_from_many_threads() {
    let handles: Vec<_> = (0..8u64)
        .map(|i| {
            std::thread::spawn(move || {
                for _ in 0..1000 {
                    nxkit_init(i);
                    let _ = find("x");
                }
            })
        })
        .collect();
    for h in handles {
        assert!(h.join().is_ok());
    }
    nxkit_init(0);
}

#[test]
fn the_bits_follow_the_service_order_of_nxtitle() {
    assert_eq!(Service::ALL.len(), nxtitle::Service::ALL.len());
    let header = include_str!("../include/nxkit_rt.h");
    for (i, s) in nxtitle::Service::ALL.iter().enumerate() {
        assert_eq!(Service::ALL[i].bit(), 1 << i);
        let line = format!(
            "#define NXKIT_SERVICE_{:<7} (UINT64_C(1) << {i})",
            s.id().to_uppercase()
        );
        assert!(header.contains(&line), "header line missing: {line}");
    }
}

#[test]
fn the_header_declares_every_export() {
    let header = include_str!("../include/nxkit_rt.h");
    for name in EXPORTS {
        assert!(
            header.contains(&format!("{name}(")),
            "{name} is not in the header"
        );
    }
}
