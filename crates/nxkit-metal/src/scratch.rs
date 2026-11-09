//! A small shared buffer for the constants of one frame (kernel parameters, draw constants).
//!
//! Metal 4 has no `setBytes`. A frame writes its constants here and binds the GPU address.

use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLBuffer, MTLDevice, MTLResource, MTLResourceOptions};

use crate::device::Context;
use crate::error::{MetalError, Result, create};

/// The alignment of every constant block. It suits the `constant` address space of Apple GPUs.
pub const SCRATCH_ALIGN: u64 = 256;

/// The size of the block a thread takes from the shared cursor. Small writes then come from the block of the
/// thread and do not touch the lock.
const LEASE_BYTES: u64 = 256 * 1024;

/// Gives each [`Scratch`] its own number, so a block of a dead scratch buffer is never taken for a live one.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A block of one scratch buffer that one thread owns until the next `reset`.
struct Lease {
    id: u64,
    epoch: u64,
    ptr: *mut u8,
    address: u64,
    cursor: u64,
    len: u64,
}

thread_local! {
    /// The block this thread writes small constants into.
    static LEASE: RefCell<Option<Lease>> = const { RefCell::new(None) };
}

struct State {
    buffers: Vec<Retained<ProtocolObject<dyn MTLBuffer>>>,
    /// The bytes of `buffers` in the memory counters, one entry per buffer.
    tracked: Vec<crate::memstats::Tracked>,
    index: usize,
    cursor: u64,
}

fn track(buffer: &ProtocolObject<dyn MTLBuffer>) -> crate::memstats::Tracked {
    crate::memstats::Tracked::new(crate::memstats::Kind::Buffer, buffer.allocatedSize() as u64)
}

/// The size of the buffer that follows one of `last` bytes when `need` bytes do not fit: twice `last`, at most
/// `max_chunk` (never below `last`), and at least `need` rounded to the alignment.
pub(crate) fn next_chunk(last: u64, need: u64, max_chunk: u64) -> u64 {
    last.saturating_mul(2)
        .min(max_chunk.max(last))
        .max(need.next_multiple_of(SCRATCH_ALIGN))
}

/// A bump allocator over shared buffers. It grows without moving live allocations, and reuses its buffers after
/// `reset`. The caller commits residency after encoding and before submitting work that uses new buffers.
pub struct Scratch {
    ctx: Context,
    /// The cap of the size of a new buffer, and the size of the first one that follows the first buffer's doubling.
    max_chunk: u64,
    id: u64,
    /// Counts the resets. A block from an earlier epoch is stale.
    epoch: AtomicU64,
    state: Mutex<State>,
}

// SAFETY: the buffer is a thread safe Metal object and the cursor lock keeps writers on separate ranges.
unsafe impl Send for Scratch {}
// SAFETY: see `Send`.
unsafe impl Sync for Scratch {}

impl Scratch {
    /// Makes a scratch buffer of `size` bytes. It joins the residency set. When it fills up, it adds buffers of the
    /// same size.
    pub fn new(ctx: &Context, size: u64) -> Result<Self> {
        Self::growing(ctx, size, size)
    }

    /// Makes a scratch buffer that starts with `first` bytes. When it fills up, each new buffer is twice as large as
    /// the one before, up to `max_chunk` (see [`next_chunk`]).
    pub fn growing(ctx: &Context, first: u64, max_chunk: u64) -> Result<Self> {
        let size = first;
        if size == 0 {
            return Err(MetalError::Invalid(
                "a scratch buffer needs at least one byte",
            ));
        }
        let buffer = ctx
            .device()
            .newBufferWithLength_options(size as usize, MTLResourceOptions::StorageModeShared)
            .ok_or_else(|| create("scratch buffer", "newBufferWithLength returned nil"))?;
        ctx.add_allocation(ProtocolObject::from_ref(&*buffer));
        Ok(Self {
            ctx: ctx.clone(),
            max_chunk: max_chunk.max(size),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            epoch: AtomicU64::new(0),
            state: Mutex::new(State {
                tracked: vec![track(&buffer)],
                buffers: vec![buffer],
                index: 0,
                cursor: 0,
            }),
        })
    }

    /// Copies `data` into the buffer and returns its GPU address.
    ///
    /// A small write goes into a block that the calling thread leased from the shared cursor, so threads that encode
    /// in parallel take the lock once per block and not once per write.
    pub fn push(&self, data: &[u8]) -> Result<u64> {
        let len = data.len() as u64;
        if len > LEASE_BYTES / 8 {
            return self.push_shared(data);
        }
        let epoch = self.epoch.load(Ordering::Acquire);
        let local = LEASE.with(|cell| {
            let mut cell = cell.borrow_mut();
            let lease = cell.as_mut()?;
            if lease.id != self.id || lease.epoch != epoch {
                return None;
            }
            let start = lease.cursor.next_multiple_of(SCRATCH_ALIGN);
            if start + len > lease.len {
                return None;
            }
            // SAFETY: `[start, start + len)` is inside the block of this thread, which `alloc` took from the
            // buffer under the lock. No other thread owns that range, and `data` is a separate allocation.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    lease.ptr.add(start as usize),
                    data.len(),
                )
            };
            lease.cursor = start + len;
            Some(lease.address + start)
        });
        if let Some(address) = local {
            return Ok(address);
        }
        let (ptr, address, epoch) = self.alloc(LEASE_BYTES)?;
        // SAFETY: the block is `LEASE_BYTES` long, `len` fits (checked above) and `data` is a separate allocation.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()) };
        LEASE.with(|cell| {
            *cell.borrow_mut() = Some(Lease {
                id: self.id,
                epoch,
                ptr,
                address,
                cursor: len,
                len: LEASE_BYTES,
            });
        });
        Ok(address)
    }

    /// Copies `data` straight from the shared cursor. Large writes take this path.
    fn push_shared(&self, data: &[u8]) -> Result<u64> {
        let len = data.len() as u64;
        let (ptr, address, _) = self.alloc(len)?;
        // SAFETY: `alloc` returned `len` bytes that no other caller owns, and `data` is a separate allocation.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()) };
        Ok(address)
    }

    /// Takes `len` bytes off the shared cursor. Returns the CPU pointer, the GPU address and the epoch of the block.
    fn alloc(&self, len: u64) -> Result<(*mut u8, u64, u64)> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| MetalError::Invalid("scratch lock poisoned"))?;
        let mut start = st.cursor.next_multiple_of(SCRATCH_ALIGN);
        while start + len > st.buffers[st.index].length() as u64 {
            let next = st.index + 1;
            if next == st.buffers.len() {
                let size = next_chunk(st.buffers[st.index].length() as u64, len, self.max_chunk);
                let buffer = self
                    .ctx
                    .device()
                    .newBufferWithLength_options(
                        size as usize,
                        MTLResourceOptions::StorageModeShared,
                    )
                    .ok_or_else(|| create("scratch buffer", "newBufferWithLength returned nil"))?;
                self.ctx.add_allocation(ProtocolObject::from_ref(&*buffer));
                st.tracked.push(track(&buffer));
                st.buffers.push(buffer);
            }
            st.index = next;
            st.cursor = 0;
            start = 0;
        }
        let buffer = &st.buffers[st.index];
        let ptr = buffer.contents().cast::<u8>();
        let address = buffer.gpuAddress() + start;
        st.cursor = start + len;
        let epoch = self.epoch.load(Ordering::Acquire);
        // SAFETY: `[start, start + len)` is inside the buffer (checked above) and `contents` points to its shared
        // memory. The cursor lock keeps callers off the same range.
        Ok((unsafe { ptr.add(start as usize) }, address, epoch))
    }

    /// The buffer that holds `[address, address + len)` and the offset of `address` in it. None when the range is not
    /// inside one buffer of this scratch. A blit reads scratch bytes through it.
    pub(crate) fn locate(
        &self,
        address: u64,
        len: u64,
    ) -> Option<(Retained<ProtocolObject<dyn MTLBuffer>>, u64)> {
        let st = self.state.lock().ok()?;
        st.buffers.iter().find_map(|b| {
            let offset = address.checked_sub(b.gpuAddress())?;
            (offset.checked_add(len)? <= b.length() as u64).then(|| (b.clone(), offset))
        })
    }

    /// Makes the whole buffer free. Call it when the GPU is done with the frame that used it.
    pub fn reset(&self) {
        if let Ok(mut st) = self.state.lock() {
            st.index = 0;
            st.cursor = 0;
            self.epoch.fetch_add(1, Ordering::AcqRel);
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let st = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for buffer in &st.buffers {
            self.ctx
                .remove_allocation(ProtocolObject::from_ref(&**buffer));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn chunks_double_up_to_the_cap() {
        assert_eq!(next_chunk(8 * MIB, 1, 64 * MIB), 16 * MIB);
        assert_eq!(next_chunk(16 * MIB, 1, 64 * MIB), 32 * MIB);
        assert_eq!(next_chunk(32 * MIB, 1, 64 * MIB), 64 * MIB);
        assert_eq!(next_chunk(64 * MIB, 1, 64 * MIB), 64 * MIB);
    }

    #[test]
    fn a_large_request_gets_its_own_chunk() {
        assert_eq!(
            next_chunk(8 * MIB, 100 * MIB + 1, 64 * MIB),
            100 * MIB + SCRATCH_ALIGN
        );
    }

    #[test]
    fn a_fixed_scratch_keeps_its_size() {
        assert_eq!(next_chunk(4 * MIB, 1, 4 * MIB), 4 * MIB);
    }
}
