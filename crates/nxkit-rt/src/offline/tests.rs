use super::table::NAMES;
use super::*;
use crate::{Service, find_in};
use std::ffi::CStr;

fn entries_with(f: &str) -> Vec<usize> {
    let all = entries();
    NAMES
        .iter()
        .filter(|(_, id)| *id == f)
        .map(|(n, _)| {
            find_in(&all, Service::Offline.bit(), n.as_bytes())
                .unwrap_or_else(|| panic!("{n} is not in the table"))
        })
        .collect()
}

/// Calls every entry of `f` as a function of type `$ty`.
macro_rules! each {
    ($f:literal, $ty:ty, |$g:ident| $body:expr) => {
        let list = entries_with($f);
        assert!(!list.is_empty(), "{} has no entry", $f);
        for addr in list {
            // SAFETY: the address is of a function of this signature, by the table.
            let $g = unsafe { std::mem::transmute::<usize, $ty>(addr) };
            $body;
        }
    };
}

#[test]
fn names_are_unique_and_belong_to_the_offline_service() {
    let all = entries();
    assert_eq!(all.len(), NAMES.len());
    let mut names: Vec<&str> = all.iter().map(|e| e.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), all.len());
    assert!(all.iter().all(|e| e.service == Service::Offline));
    assert!(
        names.iter().all(|n| n.starts_with("_ZN2nn")
            || n.starts_with("nnssl")
            || n.starts_with("nnsocket"))
    );
    assert_eq!(find_in(&all, Service::Fs.bit(), names[0].as_bytes()), None);
}

#[test]
fn every_function_of_the_table_has_a_test_here() {
    let mut used: Vec<&str> = NAMES.iter().map(|(_, f)| *f).collect();
    used.sort_unstable();
    used.dedup();
    assert_eq!(used, TESTED);
}

/// The functions of the table, sorted. A new function needs a test below and a line here.
const TESTED: &[&str] = &[
    "album_url",
    "async_done",
    "enetdown",
    "fail",
    "fail_count_second",
    "fail_id_second",
    "fail_null_first",
    "fail_result_second",
    "fail_zero_counts",
    "fail_zero_five",
    "fail_zero_i32_first",
    "fail_zero_i32_third",
    "fail_zero_pair",
    "fail_zero_u32_first",
    "friends_unavailable",
    "gai_fail",
    "gai_strerror",
    "irs_handle",
    "irs_unconnected",
    "neg1_i32",
    "neg1_i64",
    "ngc_words",
    "nifm_cancel",
    "nifm_on_hold",
    "nifm_submit",
    "nothing",
    "nsd_environment",
    "ok",
    "ok_zero_u8_first",
    "pdm_statistics",
    "zero",
];

#[test]
fn results_and_plain_returns() {
    each!("ok", extern "C" fn() -> u32, |f| assert_eq!(f(), 0));
    each!("fail", extern "C" fn() -> u32, |f| assert_eq!(f(), 0x2A2));
    each!("zero", extern "C" fn() -> u32, |f| assert_eq!(f(), 0));
    each!("neg1_i32", extern "C" fn() -> i32, |f| assert_eq!(f(), -1));
    each!("neg1_i64", extern "C" fn() -> i64, |f| assert_eq!(f(), -1));
    each!("enetdown", extern "C" fn() -> i32, |f| assert_eq!(f(), 50));
    each!("irs_unconnected", extern "C" fn() -> i32, |f| assert_eq!(
        f(),
        2
    ));
    each!("nothing", extern "C" fn(), |f| f());
}

#[test]
fn the_error_text_of_a_name_lookup() {
    each!("gai_strerror", extern "C" fn(i32) -> *const c_char, |f| {
        // SAFETY: the function returns a static NUL-terminated string.
        let text = unsafe { CStr::from_ptr(f(4)) };
        assert_eq!(text.to_bytes(), b"network unavailable");
    });
}

#[test]
fn a_name_lookup_gives_no_result_and_eai_fail() {
    type F =
        unsafe extern "C" fn(*const c_char, *const c_char, *const c_void, *mut *mut c_void) -> i32;
    each!("gai_fail", F, |f| {
        let mut res = std::ptr::dangling_mut::<c_void>();
        // SAFETY: the pointers are valid or NULL, as the function allows.
        unsafe {
            assert_eq!(
                f(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    &mut res
                ),
                4
            );
            assert!(res.is_null());
            assert_eq!(
                f(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut()
                ),
                4
            );
        }
    });
}

#[test]
fn counts_are_zeroed_when_given() {
    type F = unsafe extern "C" fn(*mut c_void, *mut i32) -> u32;
    each!("fail_count_second", F, |f| {
        let mut n = 7;
        // SAFETY: `n` is a valid count and NULL is allowed.
        unsafe {
            assert_eq!(f(std::ptr::null_mut(), &mut n), 0x2A2);
            assert_eq!(n, 0);
            assert_eq!(f(std::ptr::null_mut(), std::ptr::null_mut()), 0x2A2);
        }
    });
    type F1 = unsafe extern "C" fn(*mut i32) -> u32;
    each!("fail_zero_i32_first", F1, |f| {
        let mut n = 7;
        // SAFETY: as above.
        unsafe {
            assert_eq!(f(&mut n), 0x2A2);
            assert_eq!(n, 0);
            assert_eq!(f(std::ptr::null_mut()), 0x2A2);
        }
    });
    type F3 = unsafe extern "C" fn(*mut c_void, *mut c_char, *mut i32) -> u32;
    each!("fail_zero_i32_third", F3, |f| {
        let mut n = 7;
        // SAFETY: as above.
        unsafe {
            assert_eq!(f(std::ptr::null_mut(), std::ptr::null_mut(), &mut n), 0x2A2);
            assert_eq!(n, 0);
            assert_eq!(
                f(
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                0x2A2
            );
        }
    });
    type F4 = unsafe extern "C" fn(*mut c_void, *mut i32, *mut c_void, *mut i32) -> u32;
    each!("fail_zero_counts", F4, |f| {
        let (mut a, mut b) = (7, 8);
        // SAFETY: as above.
        unsafe {
            assert_eq!(
                f(std::ptr::null_mut(), &mut a, std::ptr::null_mut(), &mut b),
                0x2A2
            );
            assert_eq!((a, b), (0, 0));
            assert_eq!(
                f(
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                0x2A2
            );
        }
    });
}

#[test]
fn addresses_ids_and_results_are_set_when_given() {
    type Id = unsafe extern "C" fn(*mut c_void, *mut u64) -> u32;
    each!("fail_id_second", Id, |f| {
        let mut id = 9u64;
        // SAFETY: `id` is valid and NULL is allowed.
        unsafe {
            assert_eq!(f(std::ptr::null_mut(), &mut id), 0x2A2);
            assert_eq!(id, 0);
            assert_eq!(f(std::ptr::null_mut(), std::ptr::null_mut()), 0x2A2);
        }
    });
    type U = unsafe extern "C" fn(*mut u32) -> u32;
    each!("fail_zero_u32_first", U, |f| {
        let mut a = 9u32;
        // SAFETY: as above.
        unsafe {
            assert_eq!(f(&mut a), 0x2A2);
            assert_eq!(a, 0);
            assert_eq!(f(std::ptr::null_mut()), 0x2A2);
        }
    });
    type R = unsafe extern "C" fn(*mut c_void, *mut u32) -> u32;
    each!("fail_result_second", R, |f| {
        let mut r = 9u32;
        // SAFETY: as above.
        unsafe {
            assert_eq!(f(std::ptr::null_mut(), &mut r), 0x2A2);
            assert_eq!(r, 0x2A2);
            assert_eq!(f(std::ptr::null_mut(), std::ptr::null_mut()), 0x2A2);
        }
    });
    type P = unsafe extern "C" fn(*mut u32, *mut u32) -> u32;
    each!("fail_zero_pair", P, |f| {
        let (mut a, mut m) = (1u32, 2u32);
        // SAFETY: as above.
        unsafe {
            assert_eq!(f(&mut a, &mut m), 0x2A2);
            assert_eq!((a, m), (0, 0));
            assert_eq!(f(std::ptr::null_mut(), &mut m), 0x2A2);
        }
    });
    type Five = unsafe extern "C" fn(*mut u32, *mut u32, *mut u32, *mut u32, *mut u32) -> u32;
    each!("fail_zero_five", Five, |f| {
        let mut v = [1u32, 2, 3, 4, 5];
        let p = v.as_mut_ptr();
        // SAFETY: the five pointers are into `v`, and NULL is allowed.
        unsafe {
            assert_eq!(
                f(p, p.add(1), std::ptr::null_mut(), p.add(3), p.add(4)),
                0x2A2
            );
        }
        assert_eq!(v, [0, 0, 3, 0, 0]);
    });
    type N = unsafe extern "C" fn(*mut *mut c_void) -> u32;
    each!("fail_null_first", N, |f| {
        let mut p = std::ptr::dangling_mut::<c_void>();
        // SAFETY: as above.
        unsafe {
            assert_eq!(f(&mut p), 0x2A2);
            assert!(p.is_null());
            assert_eq!(f(std::ptr::null_mut()), 0x2A2);
        }
    });
}

#[test]
fn the_nsd_environment_is_lp1() {
    each!("nsd_environment", unsafe extern "C" fn(*mut u8), |f| {
        let mut out = [0xFFu8; 9];
        // SAFETY: eight bytes are written, `out` has nine.
        unsafe { f(out.as_mut_ptr()) };
        assert_eq!(out, *b"lp1\0\0\0\0\0\xFF");
    });
}

#[test]
fn friends_answer_that_they_are_unavailable_and_done() {
    each!(
        "friends_unavailable",
        unsafe extern "C" fn(*mut u8) -> u32,
        |f| {
            let mut b = 1u8;
            // SAFETY: `b` is valid.
            assert_eq!(unsafe { f(&mut b) }, 0);
            assert_eq!(b, 0);
        }
    );
    each!(
        "async_done",
        unsafe extern "C" fn(*mut c_void, *mut u8) -> u32,
        |f| {
            let mut b = 0u8;
            // SAFETY: `b` is valid.
            assert_eq!(unsafe { f(std::ptr::null_mut(), &mut b) }, 0);
            assert_eq!(b, 1);
        }
    );
}

#[test]
fn the_profanity_filter_finds_nothing() {
    each!(
        "ok_zero_u8_first",
        unsafe extern "C" fn(*mut u8) -> u32,
        |f| {
            let mut b = 1u8;
            // SAFETY: as above.
            unsafe {
                assert_eq!(f(&mut b), 0);
                assert_eq!(b, 0);
                assert_eq!(f(std::ptr::null_mut()), 0);
            }
        }
    );
    type W = unsafe extern "C" fn(*mut c_void, *mut u32, u32, *const *const u16, u64) -> u32;
    each!("ngc_words", W, |f| {
        let mut out = [7u32; 4];
        // SAFETY: `out` has three entries to write of four, and NULL is allowed.
        unsafe {
            assert_eq!(
                f(
                    std::ptr::null_mut(),
                    out.as_mut_ptr(),
                    0,
                    std::ptr::null(),
                    3
                ),
                0
            );
            assert_eq!(
                f(
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                    3
                ),
                0
            );
        }
        assert_eq!(out, [0, 0, 0, 7]);
    });
}

#[test]
fn play_statistics_are_the_id_and_two_zeros() {
    type Q = unsafe extern "C" fn(*mut u64, *const c_void, *const u64, i32) -> u32;
    each!("pdm_statistics", Q, |f| {
        let ids = [11u64, 22];
        let mut out = [5u64; 7];
        // SAFETY: `ids` has two entries and `out` six words of seven.
        unsafe { assert_eq!(f(out.as_mut_ptr(), std::ptr::null(), ids.as_ptr(), 2), 0) };
        assert_eq!(out, [11, 0, 0, 22, 0, 0, 5]);
        // a negative count writes nothing
        // SAFETY: as above.
        unsafe { assert_eq!(f(out.as_mut_ptr(), std::ptr::null(), ids.as_ptr(), -1), 0) };
        assert_eq!(out[0], 11);
    });
}

#[test]
fn the_ir_camera_is_unconnected_and_the_handle_is_the_low_byte() {
    each!("irs_handle", unsafe extern "C" fn(*const u32) -> u32, |f| {
        let npad = 0x1234_5678u32;
        // SAFETY: `npad` is valid.
        assert_eq!(unsafe { f(&npad) }, 0x78);
    });
}

#[test]
fn the_album_url_is_emptied_when_there_is_room() {
    each!(
        "album_url",
        unsafe extern "C" fn(*mut c_char, usize) -> u32,
        |f| {
            let mut buf = [b'x' as c_char; 3];
            // SAFETY: `buf` has three characters.
            unsafe {
                assert_eq!(f(buf.as_mut_ptr(), 0), 0x2A2);
                assert_eq!(buf[0], b'x' as c_char);
                assert_eq!(f(buf.as_mut_ptr(), 3), 0x2A2);
                assert_eq!(f(std::ptr::null_mut(), 3), 0x2A2);
            }
            assert_eq!(buf[0], 0);
        }
    );
}

#[test]
fn a_network_request_is_on_hold_for_one_query() {
    let (submit, cancel, query) = (
        entries_with("nifm_submit"),
        entries_with("nifm_cancel"),
        entries_with("nifm_on_hold"),
    );
    // SAFETY: the addresses are of functions of these signatures, by the table.
    let (submit, cancel, query) = unsafe {
        (
            std::mem::transmute::<usize, extern "C" fn()>(submit[0]),
            std::mem::transmute::<usize, extern "C" fn()>(cancel[0]),
            std::mem::transmute::<usize, extern "C" fn() -> i32>(query[0]),
        )
    };
    submit();
    assert_eq!([query(), query(), query()], [1, 0, 0]);
    submit();
    cancel();
    assert_eq!(query(), 0);
}
