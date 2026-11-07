//! The map between host addresses and GPU addresses of the guest memory.
//!
//! The guest memory is one buffer. A host address `h` in `[host_base, host_base + len)` has the GPU address
//! `gpu_base + (h - host_base)`. There is no translation table. The map holds numbers only, so it needs no GPU.

use crate::error::{MetalError, Result};

/// The map of one guest memory region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressMap {
    host_base: u64,
    gpu_base: u64,
    len: u64,
}

impl AddressMap {
    /// Makes the map of a region that starts at `host_base` on the host and at `gpu_base` on the GPU.
    pub fn new(host_base: u64, gpu_base: u64, len: u64) -> Result<Self> {
        if len == 0 || host_base.checked_add(len).is_none() || gpu_base.checked_add(len).is_none() {
            return Err(MetalError::Invalid("the region is empty or wraps around"));
        }
        Ok(Self {
            host_base,
            gpu_base,
            len,
        })
    }

    /// The host address of the first byte.
    pub fn host_base(&self) -> u64 {
        self.host_base
    }

    /// The GPU address of the first byte.
    pub fn gpu_base(&self) -> u64 {
        self.gpu_base
    }

    /// The length in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Always false. A map has at least one byte.
    pub fn is_empty(&self) -> bool {
        false
    }

    fn range_ok(&self, start: u64, len: u64) -> bool {
        start >= self.host_base
            && start - self.host_base <= self.len
            && len <= self.len - (start - self.host_base)
    }

    /// True when `[host_addr, host_addr + len)` lies inside the region.
    pub fn contains(&self, host_addr: u64, len: u64) -> bool {
        self.range_ok(host_addr, len)
    }

    /// The offset of a host address from the start of the region.
    pub fn offset_of_host(&self, host_addr: u64, len: u64) -> Result<u64> {
        if self.range_ok(host_addr, len) {
            Ok(host_addr - self.host_base)
        } else {
            Err(MetalError::OutOfRange {
                addr: host_addr,
                len,
            })
        }
    }

    /// The GPU address that a shader uses for a host address. `len` is the size the shader will touch.
    pub fn gpu_address(&self, host_addr: u64, len: u64) -> Result<u64> {
        Ok(self.gpu_base + self.offset_of_host(host_addr, len)?)
    }

    /// The GPU address of a byte offset into the region.
    pub fn gpu_address_of_offset(&self, offset: u64, len: u64) -> Result<u64> {
        self.gpu_address(self.host_base.saturating_add(offset), len)
    }

    /// The host address of a GPU address inside the region.
    pub fn host_address(&self, gpu_addr: u64, len: u64) -> Result<u64> {
        let ok = gpu_addr >= self.gpu_base
            && gpu_addr - self.gpu_base <= self.len
            && len <= self.len - (gpu_addr - self.gpu_base);
        if ok {
            Ok(self.host_base + (gpu_addr - self.gpu_base))
        } else {
            Err(MetalError::OutOfRange {
                addr: gpu_addr,
                len,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> AddressMap {
        AddressMap::new(0x1_0000_0000, 0x9000_0000_0000, 0x4000_0000)
            .unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn maps_both_ways() {
        let m = map();
        assert_eq!(m.gpu_address(0x1_0000_1234, 4), Ok(0x9000_0000_1234));
        assert_eq!(m.host_address(0x9000_0000_1234, 4), Ok(0x1_0000_1234));
        assert_eq!(m.gpu_address_of_offset(0x40, 1), Ok(0x9000_0000_0040));
    }

    #[test]
    fn edges() {
        let m = map();
        let end = m.host_base() + m.len();
        assert!(m.contains(end - 1, 1));
        assert!(m.contains(end, 0));
        assert!(!m.contains(end, 1));
        assert!(!m.contains(end - 4, 5));
        assert!(!m.contains(m.host_base() - 1, 1));
        assert!(m.gpu_address(u64::MAX, 2).is_err());
        assert!(m.host_address(m.gpu_base() + m.len(), 1).is_err());
    }

    #[test]
    fn rejects_empty_and_wrapping() {
        assert!(AddressMap::new(0, 0, 0).is_err());
        assert!(AddressMap::new(u64::MAX, 0, 2).is_err());
    }
}
