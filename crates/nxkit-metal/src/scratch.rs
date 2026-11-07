//! A small shared buffer for the constants of one frame (kernel parameters, draw constants).
//!
//! Metal 4 has no `setBytes`. A frame writes its constants here and binds the GPU address.

use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLBuffer, MTLDevice, MTLResourceOptions};

use crate::device::Context;
use crate::error::{MetalError, Result, create};

/// The alignment of every constant block. It suits the `constant` address space of Apple GPUs.
pub const SCRATCH_ALIGN: u64 = 256;

/// A bump allocator over one shared buffer. `reset` makes all of it free again.
pub struct Scratch {
    buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    ctx: Context,
    cursor: Mutex<u64>,
}

// SAFETY: the buffer is a thread safe Metal object and the cursor lock keeps writers on separate ranges.
unsafe impl Send for Scratch {}
// SAFETY: see `Send`.
unsafe impl Sync for Scratch {}

impl Scratch {
    /// Makes a scratch buffer of `size` bytes. It joins the residency set.
    pub fn new(ctx: &Context, size: u64) -> Result<Self> {
        let buffer = ctx
            .device()
            .newBufferWithLength_options(size as usize, MTLResourceOptions::StorageModeShared)
            .ok_or_else(|| create("scratch buffer", "newBufferWithLength returned nil"))?;
        ctx.add_allocation(ProtocolObject::from_ref(&*buffer));
        Ok(Self {
            buffer,
            ctx: ctx.clone(),
            cursor: Mutex::new(0),
        })
    }

    /// Copies `data` into the buffer and returns its GPU address.
    pub fn push(&self, data: &[u8]) -> Result<u64> {
        let mut cur = self
            .cursor
            .lock()
            .map_err(|_| MetalError::Invalid("scratch lock poisoned"))?;
        let start = cur.next_multiple_of(SCRATCH_ALIGN);
        let end = start + data.len() as u64;
        if end > self.buffer.length() as u64 {
            return Err(MetalError::OutOfRange {
                addr: start,
                len: data.len() as u64,
            });
        }
        // SAFETY: `[start, end)` is inside the buffer (checked above) and `contents` points to its shared memory.
        // The cursor lock keeps two callers off the same range, and `data` is a separate allocation.
        unsafe {
            let dst = self.buffer.contents().cast::<u8>().add(start as usize);
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
        }
        *cur = end;
        Ok(self.buffer.gpuAddress() + start)
    }

    /// Makes the whole buffer free. Call it when the GPU is done with the frame that used it.
    pub fn reset(&self) {
        if let Ok(mut cur) = self.cursor.lock() {
            *cur = 0;
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.ctx
            .remove_allocation(ProtocolObject::from_ref(&*self.buffer));
    }
}
