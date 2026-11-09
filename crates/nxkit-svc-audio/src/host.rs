//! The seams that the host sets, and the calls into it.
//!
//! The SDK take-overs of the engine need things that only the host has: the address of an SDK function by its mangled name,
//! the binding of a guest `SystemEvent` to a kernel event, guest threads and a way to stop the game. The host fills a
//! [`Host`] once and passes it to [`set_host`] before the game starts.

use std::sync::OnceLock;

/// What the host gives to the audio take-overs. Every field is required.
#[derive(Clone, Copy)]
pub struct Host {
    /// The address of the SDK's own function with this mangled name (the original, not the take-over), or 0.
    ///
    pub guest_sym: fn(&str) -> usize,
    /// Binds the guest `SystemEvent` at this address to a new kernel event. The second argument is true when the guest
    /// object owns the handle.
    pub bind_system_event: fn(u64, bool),
    /// Signals the guest `SystemEvent` at this address.
    pub signal_system_event: fn(u64),
    /// Copies the readable handle of a system event for the guest to own.
    pub dup_readable_handle: fn(u32) -> u32,
    /// Starts a guest thread that runs `entry(arg)` with a priority and a stack size and returns its thread object.
    ///
    pub spawn_guest_thread: fn(extern "C" fn(u64), u64, i32, usize) -> u64,
    /// Waits until the guest thread ends.
    pub join_guest_thread: fn(u64),
    /// Stops the game with this message. It does not return.
    pub fatal: fn(&str),
}

static HOST: OnceLock<Host> = OnceLock::new();

/// Sets the host seams. Returns false if they were set before.
pub fn set_host(h: Host) -> bool {
    HOST.set(h).is_ok()
}

fn host() -> Option<&'static Host> {
    HOST.get()
}

/// Stops the game with `msg`. Without a host it logs and aborts the process.
pub(crate) fn fatal(msg: &str) -> ! {
    eprintln!("nxkit-audio: warning: {msg}");
    if let Some(h) = host() {
        (h.fatal)(msg);
    }
    std::process::abort();
}

/// The address of the SDK function `name`. Stops the game when the host has no such function.
pub(crate) fn sym(name: &str) -> usize {
    let Some(h) = host() else {
        fatal(&format!(
            "the host seams are not set, cannot find the SDK symbol {name}"
        ));
    };
    let a = (h.guest_sym)(name);
    if a == 0 {
        fatal(&format!("audren: missing SDK symbol {name}"));
    }
    a
}

/// Like [`sym`], but a missing symbol is not an error.
pub(crate) fn sym_opt(name: &str) -> usize {
    host().map_or(0, |h| (h.guest_sym)(name))
}

/// Binds the guest event at `ev`.
pub(crate) fn bind_event(ev: u64, managed: bool) {
    match host() {
        Some(h) => (h.bind_system_event)(ev, managed),
        None => fatal("the host seams are not set, cannot bind a system event"),
    }
}

/// Signals the guest event at `ev`.
pub(crate) fn signal_event(ev: u64) {
    if let Some(h) = host() {
        (h.signal_system_event)(ev);
    }
}

/// Copies a readable handle for the guest.
pub(crate) fn dup_readable(handle: u32) -> u32 {
    host().map_or(0, |h| (h.dup_readable_handle)(handle))
}

/// Starts a guest thread. Returns its thread object, or 0 without a host.
pub(crate) fn spawn_thread(entry: extern "C" fn(u64), arg: u64, prio: i32, stack: usize) -> u64 {
    host().map_or(0, |h| (h.spawn_guest_thread)(entry, arg, prio, stack))
}

/// Waits for a guest thread.
pub(crate) fn join_thread(t: u64) {
    if let Some(h) = host() {
        (h.join_guest_thread)(t);
    }
}
