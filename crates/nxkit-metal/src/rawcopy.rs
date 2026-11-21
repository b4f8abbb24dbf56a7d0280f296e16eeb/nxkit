//! Byte copies in and out of a raw region of guest memory, with the bounds check. The guest memory (`guest`) and the
//! CPU view of it (`space`) both use them, so they sit below both.

use std::ptr::NonNull;

use crate::error::{MetalError, Result};

/// Checks that `[offset, offset + len)` lies in a region of `region_len` bytes. Returns the offset as an index.
pub(crate) fn check(region_len: u64, offset: u64, len: usize) -> Result<usize> {
    let len = len as u64;
    if offset <= region_len && len <= region_len - offset {
        Ok(offset as usize)
    } else {
        Err(MetalError::OutOfRange { addr: offset, len })
    }
}

/// Copies `out.len()` bytes from the offset `offset` of the region at `base` into `out`.
///
/// # Safety
///
/// `base` must be valid for reads of `region_len` bytes for the duration of the call.
pub(crate) unsafe fn copy_out(
    base: NonNull<u8>,
    region_len: u64,
    offset: u64,
    out: &mut [u8],
) -> Result<()> {
    let at = check(region_len, offset, out.len())?;
    // SAFETY: `check` proved `[at, at + out.len())` is inside the region, which is valid for reads (the contract of this
    // function). The slice `out` is a separate allocation, so the ranges cannot overlap.
    unsafe { std::ptr::copy_nonoverlapping(base.as_ptr().add(at), out.as_mut_ptr(), out.len()) };
    Ok(())
}

/// Copies `data` to the offset `offset` of the region at `base`.
///
/// # Safety
///
/// `base` must be valid for writes of `region_len` bytes for the duration of the call.
pub(crate) unsafe fn copy_in(
    base: NonNull<u8>,
    region_len: u64,
    offset: u64,
    data: &[u8],
) -> Result<()> {
    let at = check(region_len, offset, data.len())?;
    // SAFETY: `check` proved `[at, at + data.len())` is inside the region, which is valid for writes (the contract of
    // this function). `data` is a separate allocation, so the ranges cannot overlap.
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), base.as_ptr().add(at), data.len()) };
    Ok(())
}
