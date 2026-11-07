//! Host-initiated guest threads (nxnative `dm/host/hle_guestthread.c`).
//!
//! Host background work that calls the game's own allocator or locks (a movie player, an audio worker) needs a full
//! SDK thread context. [`GuestThread::spawn`] calls the SDK's `nn::os::CreateThread` and `StartThread` with a host
//! closure as the entry, so the SDK makes the `ThreadType`, the handle and the TLS slots, and the game's `malloc` and
//! `nn::os` locks work in the closure as for any game thread.
//!
//! It must run on a guest thread, because the SDK creates the thread through the svc gate. It is not exposed to the
//! game. The SDK functions come from [`crate::guest_sym`]. A missing one is an error that names it (nxnative dies).
//!
//! Not ported: `dm_thread_mark_host`. It marks the thread as not one of the game's, so that nxnative's start
//! handoff and core-peer waits skip it. The core rules here (`gate.rs`) keep no such wait.

use crate::guestsym::require;
use crate::kernel::current_thread;
use crate::sys::{GuestFn, drop_spawn_arg, run_spawned, spawn_arg};

const MALLOC: &str = "malloc";
const FREE: &str = "free";
const CREATE: &str = "_ZN2nn2os12CreateThreadEPNS0_10ThreadTypeEPFvPvES3_S3_mi";
const START: &str = "_ZN2nn2os11StartThreadEPNS0_10ThreadTypeE";
const WAIT: &str = "_ZN2nn2os10WaitThreadEPNS0_10ThreadTypeE";
const DESTROY: &str = "_ZN2nn2os13DestroyThreadEPNS0_10ThreadTypeE";

const PAGE: u64 = 0x1000;

/// `nn::os::ThreadType` is 0x1C0 bytes. The block has headroom.
#[repr(C, align(16))]
struct ThreadBlock {
    thread_type: [u8; 0x200],
    stack_alloc: u64,
}

/// A thread that the SDK created for the host. [`GuestThread::join`] waits for it and frees it.
pub struct GuestThread {
    block: Box<ThreadBlock>,
}

impl GuestThread {
    /// Starts `body` on a new SDK thread of `priority` with a stack of `stack_size` bytes (rounded up to 4 KB).
    /// The stack comes from the SDK's `malloc`, because the SDK's `CreateThread` maps it with `MapMemory` and
    /// needs memory that the guest allocator owns.
    pub fn spawn(
        body: impl FnOnce() + Send + 'static,
        priority: i32,
        stack_size: u64,
    ) -> Result<GuestThread, String> {
        if current_thread().is_none() {
            return Err("a guest thread is started from a guest thread only".to_string());
        }
        let malloc = require(MALLOC)?;
        let free = require(FREE)?;
        let create = require(CREATE)?;
        let start = require(START)?;
        let size = stack_size.div_ceil(PAGE) * PAGE;
        let stack_alloc = malloc.call1(size + PAGE - 1);
        if stack_alloc == 0 {
            return Err(format!("the SDK's malloc gave no stack of {size} bytes"));
        }
        let stack = stack_alloc.div_ceil(PAGE) * PAGE;
        let mut block = Box::new(ThreadBlock {
            thread_type: [0; 0x200],
            stack_alloc,
        });
        let thread_type = block.thread_type.as_mut_ptr() as usize as u64;
        let arg = spawn_arg(Box::new(body));
        let entry_fn: extern "C" fn(u64) = run_spawned;
        let entry = entry_fn as usize as u64;
        let rc = create.call_create_thread(thread_type, entry, arg, stack, size, priority);
        if rc != 0 {
            drop_spawn_arg(arg);
            free.call1(stack_alloc);
            return Err(format!("nn::os::CreateThread failed with {rc:#x}"));
        }
        start.call1(thread_type);
        Ok(GuestThread { block })
    }

    /// Waits until the thread ends, then destroys it and frees its stack.
    pub fn join(mut self) -> Result<(), String> {
        let wait: GuestFn = require(WAIT)?;
        let destroy = require(DESTROY)?;
        let free = require(FREE)?;
        let thread_type = self.block.thread_type.as_mut_ptr() as usize as u64;
        wait.call1(thread_type);
        destroy.call1(thread_type);
        free.call1(self.block.stack_alloc);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_thread_cannot_start_a_guest_thread() {
        let err = GuestThread::spawn(|| {}, 16, 0x10000)
            .err()
            .unwrap_or_default();
        assert!(err.contains("from a guest thread"), "{err}");
    }
}
