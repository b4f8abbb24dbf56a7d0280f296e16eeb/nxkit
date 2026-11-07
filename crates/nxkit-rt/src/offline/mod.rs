//! The `offline` service: what the host answers for networking and peripherals the game cannot use here (local wireless,
//! Bluetooth LE, sockets, ssl, nifm, telemetry, friends, profanity filter, play statistics, IR sensor, album). It ports
//! the entries of `dm/host/hle_offline.c` and `dm/host/hle_net.c` that need no state of the host.
//!
//! Every function answers as the host does: `Result` functions give 0 (success) or `0x2A2` (failure), socket calls give
//! -1, an output the game passes is written when it is not NULL. A function that ignores all its arguments is one shared
//! function for the whole group, because Switch code passes arguments in registers and the callee need not read them.
//!
//! What stays in nxnative, because it needs the host: the events that are bound to a host system event
//! (`ldn::AttachStateChangeEvent`, `bluetooth::Acquire*Event`), the error and web applets and the album calls that log
//! through the host, `poll`, `select` and the event descriptors, the socket calls that set the host's `errno` of the
//! thread (`sk_fail`, `GetLastError`, `GetHostEnt*`, `SetLastErrno`, `Close`), the variadic `Fcntl` and the naked
//! assembly `PlayReport::Send`.

mod table;

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicI32, Ordering};

pub(crate) use table::entries;

/// `RES_OK` of the host: success.
pub(crate) const RES_OK: u32 = 0;
/// `RES_FAIL` of the host: the one failure that cut features return.
pub(crate) const RES_FAIL: u32 = 0x2A2;
/// FreeBSD `ENETDOWN`, which Horizon sockets use.
pub(crate) const ENETDOWN: i32 = 50;
/// `EAI_FAIL`.
pub(crate) const EAI_FAIL: i32 = 4;
/// `IrCameraStatus` 2: unconnected. The game takes 0 as available.
pub(crate) const IRS_UNCONNECTED: i32 = 2;
/// What `nn::socket::GAIStrError` points to.
pub(crate) const NETWORK_UNAVAILABLE: &[u8] = b"network unavailable\0";

// The functions take a pointer from the guest and write through it only where the host's function does, which is
// where the game gave a buffer of that type. Each `unsafe` block says which.

extern "C" fn ok() -> u32 {
    RES_OK
}

extern "C" fn fail() -> u32 {
    RES_FAIL
}

extern "C" fn nothing() {}

extern "C" fn neg1_i32() -> i32 {
    -1
}

extern "C" fn neg1_i64() -> i64 {
    -1
}

extern "C" fn zero() -> u32 {
    0
}

extern "C" fn enetdown() -> i32 {
    ENETDOWN
}

extern "C" fn irs_unconnected() -> i32 {
    IRS_UNCONNECTED
}

/// `GetIrCameraHandle(const uint32_t *npad)`: the low byte of the npad id.
unsafe extern "C" fn irs_handle(npad: *const u32) -> u32 {
    // SAFETY: the game passes a readable npad id, which the host's function reads without a check.
    unsafe { *npad & 0xff }
}

/// `(a, n, ...)`: `*n = 0`, failure. Scan results of local wireless and the IR sensor's state lists.
unsafe extern "C" fn fail_count_second(_a: *mut c_void, n: *mut i32) -> u32 {
    if !n.is_null() {
        // SAFETY: `n` is the game's count and not NULL.
        unsafe { *n = 0 };
    }
    RES_FAIL
}

/// `(self, id, ...)`: `*id = 0`, failure. The ssl context's certificate imports.
unsafe extern "C" fn fail_id_second(_self: *mut c_void, id: *mut u64) -> u32 {
    if !id.is_null() {
        // SAFETY: `id` is the game's id and not NULL.
        unsafe { *id = 0 };
    }
    RES_FAIL
}

/// `(a, ...)`: `*a = 0` as 32 bits, failure. An IP address, the size of a certificate buffer.
unsafe extern "C" fn fail_zero_u32_first(a: *mut u32) -> u32 {
    if !a.is_null() {
        // SAFETY: `a` is the game's output and not NULL.
        unsafe { *a = 0 };
    }
    RES_FAIL
}

/// `(count, ...)`: `*count = 0`, failure. The friend list.
unsafe extern "C" fn fail_zero_i32_first(count: *mut i32) -> u32 {
    if !count.is_null() {
        // SAFETY: `count` is the game's output and not NULL.
        unsafe { *count = 0 };
    }
    RES_FAIL
}

/// `Connection::Peek(self, buf, int *out, n)`: `*out = 0`, failure.
unsafe extern "C" fn fail_zero_i32_third(
    _self: *mut c_void,
    _buf: *mut c_char,
    out: *mut i32,
) -> u32 {
    if !out.is_null() {
        // SAFETY: `out` is the game's output and not NULL.
        unsafe { *out = 0 };
    }
    RES_FAIL
}

/// `(self, uint32_t *r)`: `*r = RES_FAIL`, failure. The last error of an ssl connection.
unsafe extern "C" fn fail_result_second(_self: *mut c_void, r: *mut u32) -> u32 {
    if !r.is_null() {
        // SAFETY: `r` is the game's output and not NULL.
        unsafe { *r = RES_FAIL };
    }
    RES_FAIL
}

/// `(void **out, ...)`: `*out = NULL`, failure. The built-in certificates.
unsafe extern "C" fn fail_null_first(out: *mut *mut c_void) -> u32 {
    if !out.is_null() {
        // SAFETY: `out` is the game's output and not NULL.
        unsafe { *out = std::ptr::null_mut() };
    }
    RES_FAIL
}

/// `ldn::GetIpv4Address(uint32_t *address, uint32_t *mask)`: both 0, failure.
unsafe extern "C" fn fail_zero_pair(a: *mut u32, m: *mut u32) -> u32 {
    if !a.is_null() {
        // SAFETY: `a` is the game's output and not NULL.
        unsafe { *a = 0 };
    }
    if !m.is_null() {
        // SAFETY: `m` is the game's output and not NULL.
        unsafe { *m = 0 };
    }
    RES_FAIL
}

/// `nifm::GetCurrentIpConfigInfo` (both overloads): five addresses, each 0 when given, failure.
unsafe extern "C" fn fail_zero_five(
    a: *mut u32,
    b: *mut u32,
    c: *mut u32,
    d: *mut u32,
    e: *mut u32,
) -> u32 {
    for p in [a, b, c, d, e] {
        if !p.is_null() {
            // SAFETY: `p` is one of the game's outputs and not NULL.
            unsafe { *p = 0 };
        }
    }
    RES_FAIL
}

/// `nifm::GetCurrentIpConfig(v4, int *n4, v6, int *n6, ...)`: both counts 0, failure.
unsafe extern "C" fn fail_zero_counts(
    _v4: *mut c_void,
    n4: *mut i32,
    _v6: *mut c_void,
    n6: *mut i32,
) -> u32 {
    for n in [n4, n6] {
        if !n.is_null() {
            // SAFETY: `n` is one of the game's counts and not NULL.
            unsafe { *n = 0 };
        }
    }
    RES_FAIL
}

/// `nsd::GetEnvironmentIdentifier(char *out)`: 8 bytes, `lp1` and zeros. The host's function does not check `out`.
unsafe extern "C" fn nsd_environment(out: *mut u8) {
    // SAFETY: the game passes an 8-byte identifier, which the host's function writes without a check.
    unsafe {
        out.write_bytes(0, 8);
        out.copy_from_nonoverlapping(b"lp1".as_ptr(), 3);
    }
}

/// `friends::CheckFriendListAvailability(bool *out, ...)`: false, success. The host's function does not check `out`.
unsafe extern "C" fn friends_unavailable(out: *mut u8) -> u32 {
    // SAFETY: the game passes a bool, which the host's function writes without a check.
    unsafe { *out = 0 };
    RES_OK
}

/// `friends::AsyncContext::HasDone(self, bool *done)`: done, success. The host's function does not check `done`.
unsafe extern "C" fn async_done(_self: *mut c_void, done: *mut u8) -> u32 {
    // SAFETY: the game passes a bool, which the host's function writes without a check.
    unsafe { *done = 1 };
    RES_OK
}

/// `ngc::...(bool *out, text)`: false when given, success.
unsafe extern "C" fn ok_zero_u8_first(out: *mut u8) -> u32 {
    if !out.is_null() {
        // SAFETY: `out` is the game's bool and not NULL.
        unsafe { *out = 0 };
    }
    RES_OK
}

/// `ngc::ProfanityFilter::CheckProfanityWords(self, uint32_t *out, mask, words, n)`: `n` zeros, success.
unsafe extern "C" fn ngc_words(
    _self: *mut c_void,
    out: *mut u32,
    _mask: u32,
    _words: *const *const u16,
    n: u64,
) -> u32 {
    if !out.is_null() {
        for i in 0..n as usize {
            // SAFETY: `out` has `n` entries, the game's result array.
            unsafe { *out.add(i) = 0 };
        }
    }
    RES_OK
}

/// `pdm::QueryApplicationPlayStatistics(uint64_t *out, uid, const uint64_t *ids, int n)`: per id (id, 0, 0), success.
unsafe extern "C" fn pdm_statistics(
    out: *mut u64,
    _uid: *const c_void,
    ids: *const u64,
    n: i32,
) -> u32 {
    if !out.is_null() && !ids.is_null() {
        for i in 0..n.max(0) as usize {
            // SAFETY: `ids` has `n` entries and `out` has three words for each, the game's arrays.
            unsafe {
                *out.add(3 * i) = *ids.add(i);
                *out.add(3 * i + 1) = 0;
                *out.add(3 * i + 2) = 0;
            }
        }
    }
    RES_OK
}

/// `album::GetSharePostedLastUrl(char *url, size_t n, ...)`: an empty string when there is room, failure.
unsafe extern "C" fn album_url(url: *mut c_char, n: usize) -> u32 {
    if !url.is_null() && n != 0 {
        // SAFETY: `url` holds `n` characters, at least one.
        unsafe { *url = 0 };
    }
    RES_FAIL
}

/// `(host, service, hints, void **result, ...)`: no result, `EAI_FAIL`. Both name lookups of C++ and C.
unsafe extern "C" fn gai_fail(
    _host: *const c_char,
    _service: *const c_char,
    _hints: *const c_void,
    result: *mut *mut c_void,
) -> i32 {
    if !result.is_null() {
        // SAFETY: `result` is the game's output and not NULL.
        unsafe { *result = std::ptr::null_mut() };
    }
    EAI_FAIL
}

extern "C" fn gai_strerror(_e: i32) -> *const c_char {
    NETWORK_UNAVAILABLE.as_ptr().cast()
}

/// The pending network request of nifm: queries that still answer "on hold" (0: none, or settled).
static NIFM_HOLD: AtomicI32 = AtomicI32::new(0);

/// The request is on hold for the first query after the submission and settled at the next.
extern "C" fn nifm_submit() {
    NIFM_HOLD.store(1, Ordering::SeqCst);
}

extern "C" fn nifm_cancel() {
    NIFM_HOLD.store(0, Ordering::SeqCst);
}

extern "C" fn nifm_on_hold() -> i32 {
    let hold = NIFM_HOLD.try_update(Ordering::SeqCst, Ordering::SeqCst, |h| {
        (h > 0).then_some(h - 1)
    });
    i32::from(hold.is_ok_and(|h| h > 0))
}

#[cfg(test)]
mod tests;
