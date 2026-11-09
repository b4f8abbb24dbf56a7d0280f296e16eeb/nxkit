//! The guest memory as the CPU sees it: the address map with the virtual GPU base, and the host pointer.
//!
//! A [`GuestSpace`] is the guest-visible half of the guest memory. A guest GPU address is virtual: it lies in the
//! window `[GUEST_VA_BASE, GUEST_VA_MAX)` and no Metal object knows it. The space can turn a host address into such an
//! address, and it can copy bytes in and out. It holds no Metal buffer and gives no way to reach one, and it knows no
//! real GPU address. Whoever needs a buffer or a real address resolves the guest address through the pools instead.

use std::ptr::NonNull;
use std::sync::Arc;

use crate::address::{AddressMap, GUEST_VA_BASE, GUEST_VA_MAX};
use crate::error::{MetalError, Result};
use crate::guest::GuestMemory;

/// Checks that `[offset, offset + len)` lies in a region of `region_len` bytes. Returns the offset as an index.
fn check(region_len: u64, offset: u64, len: usize) -> Result<usize> {
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

/// CPU-only access to the guest memory, with the virtual GPU addresses of the guest.
///
/// [`GuestSpace::address_map`] maps a host address to its guest GPU address (`GUEST_VA_BASE` plus the offset). That is
/// not the address of any Metal buffer. The space has the CPU methods of [`GuestMemory`] with the same names and
/// signatures, and nothing else: code that needs a buffer or a real address does not compile with it.
///
/// The space is a cheap copy. It keeps the memory valid either through an `Arc<GuestMemory>` ([`GuestSpace::over`]) or
/// through the promise of its maker ([`GuestSpace::from_raw`]).
#[derive(Clone)]
pub struct GuestSpace {
    map: AddressMap,
    base: NonNull<u8>,
    /// Keeps the memory alive for a space made by `over`. A space made by `from_raw` has no owner.
    _owner: Option<Arc<GuestMemory>>,
}

// SAFETY: the pointer refers to memory that is valid for the life of the space (kept by the owner, or promised by the
// maker of `from_raw`). Concurrent reads and writes of the same bytes are the caller's to order, as for any shared
// memory.
unsafe impl Send for GuestSpace {}
// SAFETY: see `Send`. All methods take `&self` and only copy bytes in and out of the region.
unsafe impl Sync for GuestSpace {}

impl GuestSpace {
    /// The space over the memory of `mem`. It keeps `mem` alive. The length must fit the window of the guest GPU
    /// addresses.
    pub fn over(mem: &Arc<GuestMemory>) -> Result<Self> {
        let map = virtual_map(mem.host_base(), mem.len())?;
        Ok(Self {
            map,
            base: mem.base_ptr(),
            _owner: Some(Arc::clone(mem)),
        })
    }

    /// A space over host memory that nobody here owns: `len` bytes at the host address `host_base`. It needs no Metal
    /// buffer. The length must fit the window of the guest GPU addresses.
    ///
    /// # Safety
    ///
    /// `[host_base, host_base + len)` must be valid for reads and writes for as long as the space and every copy of it
    /// lives. The memory must not be unmapped or moved.
    pub unsafe fn from_raw(host_base: u64, len: u64) -> Result<Self> {
        let map = virtual_map(host_base, len)?;
        let base = NonNull::new(host_base as *mut u8)
            .ok_or(MetalError::Invalid("the host base of a space is null"))?;
        Ok(Self {
            map,
            base,
            _owner: None,
        })
    }

    /// The map between host addresses and guest GPU addresses. Its `gpu_base` is `GUEST_VA_BASE`.
    pub fn address_map(&self) -> AddressMap {
        self.map
    }

    /// The length in bytes.
    pub fn len(&self) -> u64 {
        self.map.len()
    }

    /// Always false.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The host address of the first byte.
    pub fn host_base(&self) -> u64 {
        self.map.host_base()
    }

    /// Copies bytes out of the guest memory. The offset is from the start of the region.
    pub fn read(&self, offset: u64, out: &mut [u8]) -> Result<()> {
        // SAFETY: `base` is valid for `len` bytes for the life of the space (see `Send`).
        unsafe { copy_out(self.base, self.map.len(), offset, out) }
    }

    /// Copies bytes into the guest memory. The offset is from the start of the region.
    pub fn write(&self, offset: u64, data: &[u8]) -> Result<()> {
        // SAFETY: `base` is valid for `len` bytes for the life of the space (see `Send`).
        unsafe { copy_in(self.base, self.map.len(), offset, data) }
    }
}

/// The map of a region of `len` bytes at `host_base` whose guest GPU addresses start at `GUEST_VA_BASE`. Fails when
/// the region does not fit the window.
fn virtual_map(host_base: u64, len: u64) -> Result<AddressMap> {
    if len > GUEST_VA_MAX - GUEST_VA_BASE {
        return Err(MetalError::Invalid(
            "the guest memory does not fit the window of the guest GPU addresses",
        ));
    }
    AddressMap::new(host_base, GUEST_VA_BASE, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEN: usize = 0x4000;

    /// A region that the test owns. The space made from it must not outlive it.
    fn region() -> Vec<u8> {
        vec![0; LEN]
    }

    fn space(mem: &mut [u8]) -> GuestSpace {
        // SAFETY: `mem` outlives the space in every test, and nothing moves or frees it.
        unsafe { GuestSpace::from_raw(mem.as_mut_ptr() as u64, mem.len() as u64) }.unwrap()
    }

    #[test]
    fn the_base_is_virtual() {
        let mut mem = region();
        let s = space(&mut mem);
        let map = s.address_map();
        assert_eq!(map.gpu_base(), GUEST_VA_BASE);
        assert_eq!(map.host_base(), mem.as_ptr() as u64);
        assert_eq!(s.host_base(), mem.as_ptr() as u64);
        assert_eq!(s.len(), LEN as u64);
        assert!(!s.is_empty());
        let host = s.host_base() + 0x123;
        assert_eq!(map.gpu_address(host, 4), Ok(GUEST_VA_BASE + 0x123));
        assert_eq!(map.host_address(GUEST_VA_BASE + 0x123, 4), Ok(host));
        assert!(crate::is_guest_va(map.gpu_address(host, 4).unwrap()));
    }

    #[test]
    fn read_and_write_round_trip() {
        let mut mem = region();
        let s = space(&mut mem);
        s.write(0x10, &[1, 2, 3, 4]).unwrap();
        let mut out = [0u8; 4];
        s.read(0x10, &mut out).unwrap();
        assert_eq!(out, [1, 2, 3, 4]);
        // The last byte is inside, the byte after it is not.
        s.write(LEN as u64 - 1, &[9]).unwrap();
        let mut last = [0u8; 1];
        s.read(LEN as u64 - 1, &mut last).unwrap();
        assert_eq!(last, [9]);
        drop(s);
        assert_eq!(mem[0x10..0x14], [1, 2, 3, 4]);
        assert_eq!(mem[LEN - 1], 9);
    }

    #[test]
    fn a_copy_outside_the_region_fails() {
        let mut mem = region();
        let s = space(&mut mem);
        let mut out = [0u8; 8];
        assert!(s.read(LEN as u64 - 7, &mut out).is_err());
        assert!(s.read(LEN as u64 + 1, &mut out).is_err());
        assert!(s.read(u64::MAX, &mut out).is_err());
        assert!(s.write(LEN as u64 - 7, &out).is_err());
        assert!(s.write(u64::MAX - 3, &out).is_err());
        // An empty copy at the end is fine.
        assert!(s.read(LEN as u64, &mut []).is_ok());
    }

    #[test]
    fn a_length_outside_the_window_fails() {
        let window = GUEST_VA_MAX - GUEST_VA_BASE;
        // The check comes before the pointer is used, so no memory is needed.
        // SAFETY: the call fails, so the space is never made and the memory is never read.
        let too_long = unsafe { GuestSpace::from_raw(0x1_0000, window + 1) };
        assert!(too_long.is_err());
        // SAFETY: as above.
        assert!(unsafe { GuestSpace::from_raw(0x1_0000, 0) }.is_err());
        // SAFETY: as above.
        assert!(unsafe { GuestSpace::from_raw(0, 0x1000) }.is_err());
        assert!(virtual_map(0x1_0000, window).is_ok());
    }

    #[test]
    fn a_copy_of_the_space_sees_the_same_bytes() {
        let mut mem = region();
        let a = space(&mut mem);
        let b = a.clone();
        a.write(0, &[7]).unwrap();
        let mut out = [0u8; 1];
        b.read(0, &mut out).unwrap();
        assert_eq!(out, [7]);
    }
}
