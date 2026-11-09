//! The map between host addresses and GPU addresses of the guest memory.
//!
//! The guest memory is one buffer. A host address `h` in `[host_base, host_base + len)` has the GPU address
//! `gpu_base + (h - host_base)`. There is no translation table. The map holds numbers only, so it needs no GPU.

use crate::error::{MetalError, Result};

/// The first address of the window that the guest GPU addresses will use.
///
/// The runtime gives the game virtual GPU addresses in `[GUEST_VA_BASE, GUEST_VA_MAX)`, and each memory pool keeps a
/// no-copy buffer at a real address that Metal chose. The real addresses seen so far are near `0x15_0000_0000`, so
/// the window lies above them. Nothing in Metal may be handed an address inside the window: see [`is_guest_va`].
///
/// The window lies below `1 << 40`. The game's GPU address space is 40 bits wide, and the address hint of the game
/// memory only works for addresses below that. The window is 256 GiB long, which holds the 12.6 GiB guest range with
/// room to spare.
pub const GUEST_VA_BASE: u64 = 0xC0_0000_0000;

/// The end of the window, exclusive. It is `1 << 40`.
pub const GUEST_VA_MAX: u64 = 0x100_0000_0000;

/// True when `addr` is inside the window `[GUEST_VA_BASE, GUEST_VA_MAX)` of virtual guest addresses.
///
/// Such an address is not a GPU address that Metal knows. Metal and the shaders must get real addresses only.
pub const fn is_guest_va(addr: u64) -> bool {
    addr >= GUEST_VA_BASE && addr < GUEST_VA_MAX
}

/// Checks that the range `[addr, addr + len)` of a new Metal allocation does not touch the window. Every allocation
/// that hands out a `gpuAddress` calls it once, when it is made.
pub(crate) fn check_outside_window(what: &'static str, addr: u64, len: u64) -> Result<()> {
    let end = addr.saturating_add(len);
    if addr < GUEST_VA_MAX && end > GUEST_VA_BASE {
        return Err(MetalError::Create {
            what,
            reason: format!(
                "Metal gave the GPU address range {addr:#x}..{end:#x}, which touches the window of the virtual guest addresses {GUEST_VA_BASE:#x}..{GUEST_VA_MAX:#x}; the window must move"
            ),
        });
    }
    Ok(())
}

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

    #[test]
    fn the_window_has_two_edges() {
        assert!(is_guest_va(GUEST_VA_BASE));
        assert!(is_guest_va(GUEST_VA_MAX - 1));
        assert!(!is_guest_va(GUEST_VA_BASE - 1));
        assert!(!is_guest_va(GUEST_VA_MAX));
        assert!(!is_guest_va(0));
        assert!(!is_guest_va(u64::MAX));
        assert_eq!(GUEST_VA_MAX, 1 << 40);
    }

    #[test]
    fn an_allocation_may_not_touch_the_window() {
        assert!(check_outside_window("t", 0x15_0000_0000, 1 << 30).is_ok());
        assert!(check_outside_window("t", GUEST_VA_BASE - 16, 16).is_ok());
        assert!(check_outside_window("t", GUEST_VA_MAX, 16).is_ok());
        assert!(check_outside_window("t", GUEST_VA_BASE - 16, 17).is_err());
        assert!(check_outside_window("t", GUEST_VA_BASE, 1).is_err());
        assert!(check_outside_window("t", GUEST_VA_MAX - 1, 1).is_err());
        assert!(check_outside_window("t", 0x10_0000_0000, 1 << 40).is_err());
    }
}
