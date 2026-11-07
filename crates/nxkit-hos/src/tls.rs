//! The thread-local slot of a guest thread.
//!
//! nxnative gives every guest thread one block: an 8 KiB TLS block, a 0x200 byte thread-local region and a 0x400 byte
//! thread object (`dm/host/thread.c`). The register `tpidrro_el0` points to the region. Its first 0x100 bytes are the
//! IPC buffer. The SDK reads the id of the thread at offset 0x110 and the address of the thread object at 0x1F8.
//! Here one slot of 0x4000 bytes holds all three, so a slot is one 16 KiB host page. The slots come from the `tls`
//! area of [`MemoryConfig`](crate::vm::MemoryConfig). The host reads a thread's addresses with [`Thread::tls`] and
//! [`Thread::tlr`] when it enters guest code.

use crate::kernel::Kernel;
use crate::result::{KResult, KResultOr};
use crate::thread::Thread;
use crate::vm::{THREAD_OBJECT_OFFSET, TLR_OFFSET, TLR_THREAD_ID, TLR_THREAD_OBJECT, TLS_SLOT};
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl Kernel {
    /// Gives the calling thread a zeroed slot with its id and thread object address in the region. A kernel without
    /// a memory model has no slots, and the addresses of the thread stay zero.
    pub(crate) fn tls_attach(&self, t: &Arc<Thread>) -> KResultOr<()> {
        let Some(vm) = &self.vm else {
            return Ok(());
        };
        let slot = vm.tls_acquire().ok_or_else(|| {
            (self.config.log)(
                "no thread-local slot is free, the tls area is too small for the thread count",
            );
            KResult::OUT_OF_RESOURCE
        })?;
        let tlr = slot + TLR_OFFSET;
        let ready = vm.cfg.host.zero(slot, TLS_SLOT)
            && self.mem.store32(tlr + TLR_THREAD_ID, t.id as u32)
            && self
                .mem
                .store64(tlr + TLR_THREAD_OBJECT, slot + THREAD_OBJECT_OFFSET);
        if !ready {
            vm.tls_release(slot);
            (self.config.log)(
                "the thread-local slot cannot be written, the tls area is not mapped",
            );
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        t.tls_slot.store(slot, Ordering::Release);
        Ok(())
    }

    /// Takes the slot of an exiting thread back. A second call does nothing.
    pub(crate) fn tls_release(&self, t: &Thread) {
        let slot = t.tls_slot.swap(0, Ordering::AcqRel);
        if slot != 0
            && let Some(vm) = &self.vm
        {
            vm.tls_release(slot);
        }
    }
}
