//! A persistent shared buffer that the CPU writes and shaders read through its GPU address.
//!
//! The host uses it for argument buffers that hold resource IDs and buffer addresses, for example the argument
//! buffer of an indirect command buffer, which must keep its content between frames. The buffer joins the
//! residency set. The caller makes sure the GPU does not read a range while the CPU rewrites it.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLBuffer, MTLDevice, MTLResource, MTLResourceOptions};

use crate::address::check_outside_window;
use crate::device::Context;
use crate::error::{MetalError, Result, create};

/// A shared buffer with a fixed size.
pub struct DataBuffer {
    pub(crate) raw: Retained<ProtocolObject<dyn MTLBuffer>>,
    ctx: Context,
    len: u64,
    _tracked: crate::memstats::Tracked,
}

// SAFETY: the buffer is a thread safe Metal object. Writers are bounds checked and the caller keeps two writers off
// the same range.
unsafe impl Send for DataBuffer {}
// SAFETY: see `Send`.
unsafe impl Sync for DataBuffer {}

impl DataBuffer {
    /// Makes a zeroed buffer of `len` bytes. It joins the residency set.
    pub fn new(ctx: &Context, len: u64) -> Result<Self> {
        Self::make(ctx, len, true)
    }

    /// Makes a buffer of `len` bytes that the CPU does not fill with zeros first. Use it when everything that is read
    /// is written before: a staging buffer that a kernel fills. It joins the residency set.
    pub fn new_unfilled(ctx: &Context, len: u64) -> Result<Self> {
        Self::make(ctx, len, false)
    }

    fn make(ctx: &Context, len: u64, zero: bool) -> Result<Self> {
        if len == 0 {
            return Err(MetalError::Invalid("a data buffer needs at least one byte"));
        }
        let raw = ctx
            .device()
            .newBufferWithLength_options(len as usize, MTLResourceOptions::StorageModeShared)
            .ok_or_else(|| create("data buffer", "newBufferWithLength returned nil"))?;
        if zero {
            // SAFETY: `contents` points to `len` bytes of shared memory that nobody else uses yet.
            unsafe { std::ptr::write_bytes(raw.contents().cast::<u8>(), 0, len as usize) };
        }
        check_outside_window("data buffer", raw.gpuAddress(), raw.length() as u64)?;
        ctx.add_allocation(ProtocolObject::from_ref(&*raw));
        let tracked = crate::memstats::Tracked::new(
            crate::memstats::Kind::Buffer,
            raw.allocatedSize() as u64,
        );
        Ok(Self {
            raw,
            ctx: ctx.clone(),
            len,
            _tracked: tracked,
        })
    }

    /// The size in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// True when the buffer has no bytes. It never is, the constructor refuses that.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The GPU address of the first byte.
    pub fn gpu_address(&self) -> u64 {
        self.raw.gpuAddress()
    }

    /// Copies `data` to `offset`.
    pub fn write(&self, offset: u64, data: &[u8]) -> Result<()> {
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|e| *e <= self.len)
            .ok_or(MetalError::OutOfRange {
                addr: offset,
                len: data.len() as u64,
            })?;
        debug_assert!(end <= self.len);
        // SAFETY: `[offset, end)` is inside the buffer (checked above) and `data` is a separate allocation.
        unsafe {
            let dst = self.raw.contents().cast::<u8>().add(offset as usize);
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
        }
        Ok(())
    }

    /// Copies `out.len()` bytes from `offset`.
    pub fn read(&self, offset: u64, out: &mut [u8]) -> Result<()> {
        offset
            .checked_add(out.len() as u64)
            .filter(|e| *e <= self.len)
            .ok_or(MetalError::OutOfRange {
                addr: offset,
                len: out.len() as u64,
            })?;
        // SAFETY: the range is inside the buffer (checked above) and `out` is a separate allocation.
        unsafe {
            let src = self.raw.contents().cast::<u8>().add(offset as usize);
            std::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), out.len());
        }
        Ok(())
    }
}

impl Drop for DataBuffer {
    fn drop(&mut self) {
        self.ctx
            .remove_allocation(ProtocolObject::from_ref(&*self.raw));
    }
}
