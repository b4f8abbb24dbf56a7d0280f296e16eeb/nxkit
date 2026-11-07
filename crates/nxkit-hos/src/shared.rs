//! Shared memory, transfer memory and what happens when a handle closes.
//!
//! Shared memory is made by a service through `nxkit_abi::kernel::Kernel::create_shared_memory`. Its pages are in the
//! `shared` area of the memory configuration, which the host maps. The game maps them at a second address with
//! `MapSharedMemory`, and the host shares the pages with `mach_vm_remap`. `UnmapSharedMemory` takes the second view
//! away, and the backing stays. The backing is never reused. A service makes few shared memories and the area has
//! room for them.
//!
//! Transfer memory is memory of the game that a service may use. `CreateTransferMemory` locks the pages, so the game
//! cannot unmap or reprotect them. The host keeps them accessible, because the service reads and writes them through
//! the host address. Closing the handle unlocks them. Neither kind of memory is found in nxnative. Both follow the
//! Horizon rules (switchbrew `svc` page, Atmosphere `svc_results.hpp`, checked on 2026-11-07).

use crate::handle::{KObject, SharedMemory, TransferMemory};
use crate::kernel::Kernel;
use crate::memsvc::page_range;
use crate::result::{KResult, KResultOr};
use crate::space::{Block, attr, perm, state};
use crate::thread::lock;
use std::sync::Arc;

fn valid_view_perm(p: u32) -> bool {
    matches!(p, perm::R | perm::RW)
}

impl Kernel {
    /// The base address and size of the shared memory a handle names.
    pub fn shared_memory(&self, handle: u32) -> KResultOr<(u64, u64)> {
        match self.handles.get(handle) {
            Some(KObject::SharedMemory(s)) => Ok((s.base, s.size)),
            _ => Err(KResult::INVALID_HANDLE),
        }
    }

    /// The address and size of the transfer memory a handle names. The pages are in the game's heap.
    pub fn transfer_memory(&self, handle: u32) -> KResultOr<(u64, u64)> {
        match self.handles.get(handle) {
            Some(KObject::TransferMemory(t)) => Ok((t.addr, t.size)),
            _ => Err(KResult::INVALID_HANDLE),
        }
    }

    /// `MapSharedMemory`: the shared memory of `handle` appears at `addr` in the alias region. The size must be the
    /// size of the object. The address must be a whole host page. The host maps whole host pages, so the view can
    /// reach behind `size` up to the next host page. Those bytes are part of the backing and stay zero until the
    /// service writes them.
    pub(crate) fn map_shared_memory(
        &self,
        handle: u32,
        addr: u64,
        size: u64,
        new: u32,
    ) -> KResultOr<()> {
        let vm = self.vm()?;
        let Some(KObject::SharedMemory(obj)) = self.handles.get(handle) else {
            return Err(KResult::INVALID_HANDLE);
        };
        let (start, end) = page_range(addr, size)?;
        if !valid_view_perm(new) {
            return Err(KResult::INVALID_NEW_MEMORY_PERMISSION);
        }
        if size != obj.size {
            return Err(KResult::INVALID_SIZE);
        }
        if start < vm.layout.alias_base || end > vm.layout.alias_end {
            return Err(KResult::INVALID_MEMORY_REGION);
        }
        let host = &vm.cfg.host;
        let page = host.page_size();
        if !start.is_multiple_of(page) {
            (self.config.log)(&format!(
                "MapSharedMemory at {start:#x} needs the host page alignment {page:#x}, the call is refused"
            ));
            return Err(KResult::INVALID_ADDRESS);
        }
        let host_len = size.next_multiple_of(page);
        let mut sp = lock(&vm.space);
        if !sp.map.all(start, end, |b: &Block| b.state == state::FREE) {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        if !host.remap(start, obj.base, host_len, new) {
            return Err(self.host_failed("MapSharedMemory"));
        }
        // The source address marks the block, so views of different objects never join.
        sp.map.set(start, end, state::SHARED, new, 0, obj.base);
        Ok(())
    }

    /// `UnmapSharedMemory`: the view must be exactly the one `MapSharedMemory` made.
    pub(crate) fn unmap_shared_memory(&self, handle: u32, addr: u64, size: u64) -> KResultOr<()> {
        let vm = self.vm()?;
        let Some(KObject::SharedMemory(obj)) = self.handles.get(handle) else {
            return Err(KResult::INVALID_HANDLE);
        };
        let (start, end) = page_range(addr, size)?;
        if size != obj.size {
            return Err(KResult::INVALID_SIZE);
        }
        let mut sp = lock(&vm.space);
        let b = sp.map.query(start);
        if b.state != state::SHARED || b.start != start || b.end != end || b.src != obj.base {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        let host = &vm.cfg.host;
        if !host.release(start, size.next_multiple_of(host.page_size())) {
            return Err(self.host_failed("UnmapSharedMemory"));
        }
        sp.map.set(start, end, state::FREE, perm::NONE, 0, 0);
        Ok(())
    }

    /// `CreateTransferMemory`: locks normal read and write memory without attributes and returns the handle. The
    /// permission is what the service may do. It is recorded and the host pages stay accessible.
    pub(crate) fn create_transfer_memory(&self, addr: u64, size: u64, new: u32) -> KResultOr<u32> {
        let vm = self.vm()?;
        let (start, end) = page_range(addr, size)?;
        if !matches!(new, perm::NONE | perm::R | perm::RW) {
            return Err(KResult::INVALID_NEW_MEMORY_PERMISSION);
        }
        let mut sp = lock(&vm.space);
        let plain = |b: &Block| b.state == state::NORMAL && b.perm == perm::RW && b.attr == 0;
        if !sp.map.all(start, end, plain) {
            return Err(KResult::INVALID_CURRENT_MEMORY);
        }
        let object = Arc::new(TransferMemory { addr: start, size });
        let handle = self.handles.insert(KObject::TransferMemory(object))?;
        sp.map.rewrite(start, end, |b| Block {
            perm: new,
            attr: b.attr | attr::LOCKED,
            ..*b
        });
        Ok(handle)
    }

    /// Runs when a handle closes: a session closes, and transfer memory is unlocked and read and write again.
    pub(crate) fn on_close(&self, object: KObject) {
        match object {
            KObject::Session(s) => s.close(),
            KObject::TransferMemory(t) => {
                if let Some(vm) = &self.vm {
                    lock(&vm.space)
                        .map
                        .rewrite(t.addr, t.addr + t.size, |b| Block {
                            perm: perm::RW,
                            attr: b.attr & !attr::LOCKED,
                            ..*b
                        });
                }
            }
            _ => {}
        }
    }

    /// Makes shared memory of `size` bytes for a service. Returns the handle and the address of the backing.
    pub(crate) fn make_shared_memory(&self, size: u64) -> KResultOr<(u32, u64)> {
        let vm = self.vm()?;
        if size == 0 {
            return Err(KResult::INVALID_SIZE);
        }
        let (base, _) = vm.shared_alloc(size).ok_or_else(|| {
            (self.config.log)("the shared memory area is used up, the host must make it larger");
            KResult::OUT_OF_MEMORY
        })?;
        let object = Arc::new(SharedMemory {
            base,
            size: size.next_multiple_of(crate::memory::PAGE_SIZE),
        });
        let handle = self.handles.insert(KObject::SharedMemory(object))?;
        Ok((handle, base))
    }
}
