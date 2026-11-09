//! SystemEvents of the SDK layer.
//!
//! An SDK function such as `nn::oe::GetNotificationMessageEvent` fills a `nn::os::SystemEventType` of the game. On a
//! console the service hands the read end of a kernel event to the game, and the SDK wraps it. The export does the
//! same in three steps. It calls the SDK function `CreateSystemEvent` of the game through the `guest_sym` seam, which
//! makes the kernel event. It reads the write end with `GetWritableHandleOfSystemEvent`. It keeps the pair (guest
//! address, write end), and signals through `nxkit_abi::kernel::kernel()`. This follows `hle_bind_system_event` and
//! `hle_signal_system_event` in the engine `dm/host/hle_thread.c`.

use crate::seams::guest_sym;
use crate::sys::call_guest;
use crate::util::lock;
use nxkit_abi::kernel::kernel;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const CREATE: &str = "_ZN2nn2os17CreateSystemEventEPNS0_15SystemEventTypeENS0_14EventClearModeEb";
const WRITABLE: &str = "_ZN2nn2os30GetWritableHandleOfSystemEventEPKNS0_15SystemEventTypeE";

static BOUND: Mutex<BTreeMap<u64, u32>> = Mutex::new(BTreeMap::new());

fn handle(ev: u64) -> Option<u32> {
    lock(&BOUND).get(&ev).copied()
}

/// Makes the SDK event at the guest address `ev` a kernel event that the host can signal. An event that is bound
/// already stays as it is. `auto_clear` is the `EventClearMode` of the SDK. Runs guest code, so a guest thread must
/// call it. Gives false when the game has no `CreateSystemEvent`, as for a game that does not link the `os` module.
pub(crate) fn bind(ev: u64, auto_clear: bool) -> bool {
    if ev == 0 {
        return false;
    }
    if handle(ev).is_some() {
        return true;
    }
    let (Some(create), Some(writable)) = (guest_sym(CREATE), guest_sym(WRITABLE)) else {
        return false;
    };
    let result = call_guest(create, [ev, u64::from(auto_clear), 1, 0, 0, 0]) & 0xffff_ffff;
    if result != 0 {
        eprintln!("nxkit-svc-system: CreateSystemEvent failed with {result:#x}");
        return false;
    }
    let wh = (call_guest(writable, [ev, 0, 0, 0, 0, 0]) & 0xffff_ffff) as u32;
    lock(&BOUND).insert(ev, wh);
    true
}

/// Signals the event, and binds it as a manual-clear event on the spot when it is not bound yet.
pub(crate) fn signal(ev: u64) {
    if handle(ev).is_none() && !bind(ev, false) {
        return;
    }
    signal_bound(ev);
}

/// Signals the event when it is bound. Gives false for an event that was never bound or that the game destroyed.
/// A thread of the host can call it.
pub(crate) fn signal_bound(ev: u64) -> bool {
    match (handle(ev), kernel()) {
        (Some(wh), Some(k)) => k.signal_event(wh).is_ok(),
        _ => false,
    }
}

/// Clears the event when it is bound.
pub(crate) fn clear(ev: u64) {
    if let (Some(wh), Some(k)) = (handle(ev), kernel()) {
        let _ = k.clear_event(wh);
    }
}

/// Forgets the events whose write end is `handle`. The kernel calls it, through [`crate::on_handle_closed`], when the
/// game closes a handle, so that the address of a destroyed event can be bound again.
pub(crate) fn unbind_handle(handle: u32) {
    lock(&BOUND).retain(|_, wh| *wh != handle);
}

/// Allocates `size` bytes in the guest address range that stay for the life of the process, and zeroes them. The
/// memory is shared memory of the kernel that the host never maps away. Gives 0 when there is no kernel or the kernel
/// refuses.
pub(crate) fn guest_alloc(size: u64) -> u64 {
    let Some(k) = kernel() else {
        return 0;
    };
    let size = size.max(1).div_ceil(0x1000) * 0x1000;
    match k.create_shared_memory(size) {
        Ok((_, addr)) => {
            if nxkit_abi::guest::fill(addr, size, 0).is_err() {
                return 0;
            }
            addr
        }
        Err(_) => 0,
    }
}

/// A block of guest memory that the game reads and writes and that never moves: the stand-in for the static buffers of
/// Horizon (`static uint8_t g_ev[0x100]`). The first call of [`Block::addr`] allocates it.
pub(crate) struct Block(AtomicU64);

impl Block {
    /// A block that is not allocated yet.
    pub(crate) const fn new() -> Block {
        Block(AtomicU64::new(0))
    }

    /// The guest address of the block of `size` bytes, page aligned and zeroed. The size of the first call counts. Gives
    /// 0 when the allocation failed, and the next call tries again. A call after the first is one atomic load. Two
    /// first calls at once can both allocate. The loser's block stays unused, which is a few pages once.
    pub(crate) fn addr(&self, size: u64) -> u64 {
        let a = self.0.load(Ordering::Acquire);
        if a != 0 {
            return a;
        }
        let fresh = guest_alloc(size);
        if fresh == 0 {
            return 0;
        }
        match self
            .0
            .compare_exchange(0, fresh, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => fresh,
            Err(won) => won,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_without_a_kernel_has_no_address() {
        assert_eq!(Block::new().addr(0x100), 0);
    }

    #[test]
    fn an_unbound_event_is_not_signalled() {
        assert!(!signal_bound(0x1234));
    }

    #[test]
    fn a_null_event_is_not_bound() {
        assert!(!bind(0, false));
    }
}
