//! The GPU address space of the guest.
//!
//! The backend maps guest memory as one buffer without copies, and Metal assigns the GPU address of that buffer.
//! The address that NVN gives the game is this Metal address:
//! `gpu = gpu_base + (host - host_base)`. No translation table exists.
//! The backend supplies the two bases. A [`GpuSpace`] is the small interface for them.

use crate::error::{NvnError, Result};

/// The number of bits of a GPU address that NVN can express.
pub const GPU_ADDRESS_BITS: u32 = 40;

/// A GPU address below 2^40.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GpuAddr(u64);

impl GpuAddr {
    /// An address from its raw value. The value must fit in 40 bits.
    pub const fn new(raw: u64) -> Result<Self> {
        if raw >> GPU_ADDRESS_BITS != 0 {
            return Err(NvnError::TooWide(raw));
        }
        Ok(Self(raw))
    }

    /// The raw value.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The address `by` bytes further. The result must fit in 40 bits.
    pub fn offset(self, by: u64) -> Result<Self> {
        let sum = self.0.checked_add(by).ok_or(NvnError::TooWide(u64::MAX))?;
        Self::new(sum)
    }
}

/// The mapping between host addresses of guest memory and GPU addresses.
pub trait GpuSpace {
    /// The GPU address of `len` bytes at the host address `host`.
    /// The range must lie inside the mapped memory.
    fn gpu_address(&self, host: u64, len: u64) -> Result<GpuAddr>;

    /// The host address of `len` bytes at the GPU address `gpu`.
    /// The range must lie inside the mapped memory.
    fn host_address(&self, gpu: GpuAddr, len: u64) -> Result<u64>;
}

/// One mapped range: the host base, the GPU base that Metal chose and the length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinearSpace {
    host_base: u64,
    gpu_base: GpuAddr,
    len: u64,
}

impl LinearSpace {
    /// A mapping of `len` bytes. The whole GPU range must fit in 40 bits.
    pub fn new(host_base: u64, gpu_base: u64, len: u64) -> Result<Self> {
        let gpu_base = GpuAddr::new(gpu_base)?;
        host_base.checked_add(len).ok_or(NvnError::OutOfRange {
            addr: host_base,
            len,
        })?;
        gpu_base.offset(len)?;
        Ok(Self {
            host_base,
            gpu_base,
            len,
        })
    }

    /// The length of the mapped range.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// True when the mapping holds no bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The offset of `len` bytes at `addr` from `base`, or an error when the range is not inside the mapping.
    fn offset_in(&self, addr: u64, base: u64, len: u64) -> Result<u64> {
        let bad = NvnError::OutOfRange { addr, len };
        let off = addr.checked_sub(base).ok_or_else(|| bad.clone())?;
        let end = off.checked_add(len).ok_or_else(|| bad.clone())?;
        if end > self.len {
            return Err(bad);
        }
        Ok(off)
    }
}

impl GpuSpace for LinearSpace {
    fn gpu_address(&self, host: u64, len: u64) -> Result<GpuAddr> {
        let off = self.offset_in(host, self.host_base, len)?;
        self.gpu_base.offset(off)
    }

    fn host_address(&self, gpu: GpuAddr, len: u64) -> Result<u64> {
        let off = self.offset_in(gpu.get(), self.gpu_base.get(), len)?;
        Ok(self.host_base + off)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space() -> LinearSpace {
        LinearSpace::new(0x8_0000_0000, 0x15_0400_8000, 0x1000_0000).unwrap()
    }

    #[test]
    fn maps_without_a_table() {
        let s = space();
        let g = s.gpu_address(0x8_0000_1000, 0x100).unwrap();
        assert_eq!(g.get(), 0x15_0400_9000);
        assert_eq!(s.host_address(g, 0x100).unwrap(), 0x8_0000_1000);
    }

    #[test]
    fn rejects_ranges_outside_the_mapping() {
        let s = space();
        assert!(s.gpu_address(0x7_ffff_ffff, 1).is_err());
        assert!(s.gpu_address(0x8_1000_0000, 1).is_err());
        assert!(s.gpu_address(0x8_0fff_ffff, 2).is_err());
        assert!(s.gpu_address(u64::MAX, 2).is_err());
        assert!(s.gpu_address(0x8_0fff_ffff, 1).is_ok());
        let past = GpuAddr::new(0x15_1400_8000).unwrap();
        assert!(s.host_address(past, 1).is_err());
    }

    #[test]
    fn rejects_addresses_wider_than_40_bits() {
        assert_eq!(GpuAddr::new(1 << 40), Err(NvnError::TooWide(1 << 40)));
        assert!(GpuAddr::new((1 << 40) - 1).is_ok());
        assert!(LinearSpace::new(0, (1 << 40) - 0x10, 0x20).is_err());
        let top = GpuAddr::new((1 << 40) - 1).unwrap();
        assert!(top.offset(1).is_err());
    }
}
