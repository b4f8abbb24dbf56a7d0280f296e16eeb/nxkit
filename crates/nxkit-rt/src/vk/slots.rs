//! The memory of the dispatchable handles (`VkInstance`, `VkPhysicalDevice`, `VkDevice`, `VkQueue`,
//! `VkCommandBuffer`): [`GuestSlots`], the `SlotAlloc` of `nxkit-vk`.
//!
//! A dispatchable handle points to memory whose first word is the loader's magic. The slots live in host memory that
//! the allocator owns: chunks of [`CHUNK`] slots of 16 bytes on the heap, which never move and are never given back
//! while the context lives (the whole process). The game shares the host's address space (R18), so it can read a slot
//! as it reads the static `nn::vi` objects of `gfx::vi`. No code of the game or of the host reads the slots through
//! `nxkit_abi::guest`: the front end keys its objects by the address, and the game's Vulkan loader, the only reader of
//! the magic on the console, is replaced by this layer. The slots need no place in the guest reservation, which
//! belongs to the kernel's allocators, and no static.
//!
//! A freed slot is reused by a later handle, as a driver reuses the memory of a freed object.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use nxkit_vk::handle::{Kind, LOADER_MAGIC, SLOT_BYTES, SlotAlloc};

/// The slots of one chunk.
pub(crate) const CHUNK: usize = 256;

/// The most chunks: 16384 dispatchable objects at once. The Forge makes a few command buffers per frame in flight.
pub(crate) const MAX_CHUNKS: usize = 64;

/// One slot: the loader word and a free word.
#[repr(C, align(16))]
#[derive(Debug, Default)]
struct Slot([AtomicU64; 2]);

const _: () = assert!(std::mem::size_of::<Slot>() as u64 == SLOT_BYTES);

#[derive(Debug, Default)]
struct Chunks {
    chunks: Vec<Box<[Slot]>>,
    /// The slots of the last chunk that were never given out start at this index.
    fresh: usize,
    free: Vec<u64>,
    live: HashSet<u64>,
}

/// The slot allocator of the Vulkan context.
#[derive(Debug, Default)]
pub(crate) struct GuestSlots {
    inner: Mutex<Chunks>,
}

impl GuestSlots {
    /// The slot at `addr`, if it is one of the allocator's.
    fn slot(c: &Chunks, addr: u64) -> Option<&Slot> {
        c.chunks.iter().find_map(|ch| {
            let base = ch.as_ptr() as u64;
            let off = addr.checked_sub(base)?;
            (off % SLOT_BYTES == 0)
                .then(|| ch.get((off / SLOT_BYTES) as usize))
                .flatten()
        })
    }

    /// The first word of a live slot, for tests.
    #[cfg(test)]
    pub(crate) fn word(&self, addr: u64) -> Option<u64> {
        let c = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if !c.live.contains(&addr) {
            return None;
        }
        Self::slot(&c, addr).map(|s| s.0[0].load(Ordering::Relaxed))
    }
}

impl SlotAlloc for GuestSlots {
    fn alloc(&self, _kind: Kind) -> Option<u64> {
        let mut c = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let addr = match c.free.pop() {
            Some(a) => a,
            None => {
                if c.chunks.is_empty() || c.fresh == CHUNK {
                    if c.chunks.len() == MAX_CHUNKS {
                        return None;
                    }
                    let chunk: Box<[Slot]> = (0..CHUNK).map(|_| Slot::default()).collect();
                    c.chunks.push(chunk);
                    c.fresh = 0;
                }
                let i = c.fresh;
                c.fresh += 1;
                let last = c.chunks.last()?;
                last.get(i)? as *const Slot as u64
            }
        };
        let s = Self::slot(&c, addr)?;
        s.0[0].store(LOADER_MAGIC, Ordering::Relaxed);
        s.0[1].store(0, Ordering::Relaxed);
        c.live.insert(addr);
        Some(addr)
    }

    fn free(&self, addr: u64) {
        let mut c = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if c.live.remove(&addr) {
            if let Some(s) = Self::slot(&c, addr) {
                s.0[1].store(0, Ordering::Relaxed);
            }
            c.free.push(addr);
        }
    }
}
