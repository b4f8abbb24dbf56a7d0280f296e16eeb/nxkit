//! Calls into the SDK's own functions, the argument of a host-made guest thread, and the host's random bytes.

/// The entry of an SDK function that the host found by mangled name (`guest_sym`).
///
/// A value exists only for an address that the host's lookup returned, and the lookup promises the signature that the
/// name stands for. Each call method below names the signature it assumes.
#[derive(Clone, Copy, Debug)]
pub struct GuestFn(usize);

impl GuestFn {
    /// A function at `addr`, which is not zero and comes from the host's lookup.
    pub(crate) fn new(addr: usize) -> GuestFn {
        GuestFn(addr)
    }

    /// Calls `u64 f(u64)`.
    pub fn call1(self, a: u64) -> u64 {
        // SAFETY: `self.0` is the entry of an SDK function with this signature (the lookup's contract). Switch and
        // Apple AAPCS64 agree on 64-bit integer arguments and results.
        let f: extern "C" fn(u64) -> u64 = unsafe { std::mem::transmute(self.0) };
        f(a)
    }

    /// Calls `u64 f(u64, u64)`.
    pub fn call2(self, a: u64, b: u64) -> u64 {
        // SAFETY: as in `call1`.
        let f: extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(self.0) };
        f(a, b)
    }

    /// Calls `u64 f(u64, u64, u64)`.
    pub fn call3(self, a: u64, b: u64, c: u64) -> u64 {
        // SAFETY: as in `call1`.
        let f: extern "C" fn(u64, u64, u64) -> u64 = unsafe { std::mem::transmute(self.0) };
        f(a, b, c)
    }

    /// Calls `u32 f(u64, u64, u64, u64, u64, i32)`: `nn::os::CreateThread`.
    pub fn call_create_thread(
        self,
        thread: u64,
        entry: u64,
        arg: u64,
        stack: u64,
        stack_size: u64,
        priority: i32,
    ) -> u32 {
        // SAFETY: as in `call1`. A `u32` result is read from w0 and the `i32` goes in w5.
        let f: extern "C" fn(u64, u64, u64, u64, u64, i32) -> u32 =
            unsafe { std::mem::transmute(self.0) };
        f(thread, entry, arg, stack, stack_size, priority)
    }

    /// Calls `u32 f(u64, i32, i32)`: `nn::os::CreateSystemEvent(event, clear mode, inter-process)`. A `bool`
    /// argument of Switch code is one `i32` here, because Switch code does not extend it.
    pub fn call_create_system_event(self, event: u64, clear_mode: i32, inter_process: i32) -> u32 {
        // SAFETY: as in `call1`.
        let f: extern "C" fn(u64, i32, i32) -> u32 = unsafe { std::mem::transmute(self.0) };
        f(event, clear_mode, inter_process)
    }

    /// Calls `u32 f(u64)`: `nn::os::GetWritableHandleOfSystemEvent(event)`.
    pub fn call_u32_1(self, a: u64) -> u32 {
        // SAFETY: as in `call1`.
        let f: extern "C" fn(u64) -> u32 = unsafe { std::mem::transmute(self.0) };
        f(a)
    }
}

/// Makes the argument of an SDK thread entry from a closure. [`run_spawned`] runs it once. A caller whose thread
/// never starts gives it back with [`drop_spawn_arg`].
pub(crate) fn spawn_arg(f: Box<dyn FnOnce() + Send>) -> u64 {
    Box::into_raw(Box::new(f)) as usize as u64
}

/// The entry function of a host-made guest thread: `void entry(void *arg)` with `arg` from [`spawn_arg`].
pub(crate) extern "C" fn run_spawned(arg: u64) {
    // SAFETY: `arg` comes from `spawn_arg` and the SDK passes it to this entry once, so the box is still whole and
    // nothing else owns it.
    let f = unsafe { Box::from_raw(arg as usize as *mut Box<dyn FnOnce() + Send>) };
    f();
}

/// Frees an argument from [`spawn_arg`] that no thread took.
pub(crate) fn drop_spawn_arg(arg: u64) {
    // SAFETY: `arg` comes from `spawn_arg` and `run_spawned` did not take it, so the caller is its only owner.
    drop(unsafe { Box::from_raw(arg as usize as *mut Box<dyn FnOnce() + Send>) });
}

/// Fills `buf` with random bytes of the system (`arc4random_buf` on Apple platforms).
#[cfg(target_vendor = "apple")]
pub fn random_fill(buf: &mut [u8]) {
    use std::ffi::c_void;
    // SAFETY: the declaration is `<stdlib.h>`'s `arc4random_buf`.
    unsafe extern "C" {
        fn arc4random_buf(buf: *mut c_void, len: usize);
    }
    // SAFETY: the pointer and the length are those of `buf`, which is writable for its length.
    unsafe { arc4random_buf(buf.as_mut_ptr().cast::<c_void>(), buf.len()) }
}

/// Fills `buf` with bytes of the standard library's randomly keyed hasher on other platforms.
#[cfg(not(target_vendor = "apple"))]
pub fn random_fill(buf: &mut [u8]) {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    let state = RandomState::new();
    for (i, chunk) in buf.chunks_mut(8).enumerate() {
        let word = state.hash_one(i as u64).to_le_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
}
