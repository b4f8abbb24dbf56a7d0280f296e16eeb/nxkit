//! Guest memory as the Vulkan front end reads and writes it.
//!
//! The game passes its structs by address. A guest address is a host address (R18). [`Mem`] is the one way into that
//! memory: [`GuestMemory`] goes through `nxkit_abi::guest` (with its range check), [`TestMemory`] is a buffer of its
//! own for tests, at an address that a test picks. The decoders of [`crate::info`] and the writers of
//! [`crate::write`] take any [`Mem`], so they run in tests without a guest.

use std::sync::{Mutex, PoisonError};

use nxkit_abi::guest;

use crate::result::{VkError, VkResult};

/// The most entries that the front end reads from one array of the game. A count above it is garbage, and the call
/// fails with `VK_ERROR_OUT_OF_HOST_MEMORY` instead of allocating gigabytes.
pub const MAX_ARRAY: u32 = 1 << 16;

/// The most links that the front end follows in one `pNext` chain.
pub const MAX_CHAIN: usize = 64;

/// Guest memory: plain copies in and out.
pub trait Mem {
    /// Copies the bytes at `addr` into `out`.
    fn read(&self, addr: u64, out: &mut [u8]) -> VkResult<()>;

    /// Copies `data` to `addr`.
    fn write(&self, addr: u64, data: &[u8]) -> VkResult<()>;

    /// Reads a `u8`.
    fn u8(&self, addr: u64) -> VkResult<u8> {
        let mut b = [0u8; 1];
        self.read(addr, &mut b)?;
        Ok(b[0])
    }

    /// Reads a little-endian `u32`.
    fn u32(&self, addr: u64) -> VkResult<u32> {
        let mut b = [0u8; 4];
        self.read(addr, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Reads a little-endian `i32`.
    fn i32(&self, addr: u64) -> VkResult<i32> {
        Ok(self.u32(addr)? as i32)
    }

    /// Reads a little-endian `f32`.
    fn f32(&self, addr: u64) -> VkResult<f32> {
        Ok(f32::from_bits(self.u32(addr)?))
    }

    /// Reads a `VkBool32`.
    fn bool32(&self, addr: u64) -> VkResult<bool> {
        Ok(self.u32(addr)? != 0)
    }

    /// Reads a little-endian `u64` (also a pointer, a `size_t` or a handle).
    fn u64(&self, addr: u64) -> VkResult<u64> {
        let mut b = [0u8; 8];
        self.read(addr, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    /// Writes a little-endian `u32`.
    fn put_u32(&self, addr: u64, v: u32) -> VkResult<()> {
        self.write(addr, &v.to_le_bytes())
    }

    /// Writes a little-endian `u64`.
    fn put_u64(&self, addr: u64, v: u64) -> VkResult<()> {
        self.write(addr, &v.to_le_bytes())
    }

    /// Reads `len` bytes.
    fn bytes(&self, addr: u64, len: u64) -> VkResult<Vec<u8>> {
        if len > u64::from(MAX_ARRAY) * 1024 {
            return Err(VkError::OutOfHostMemory);
        }
        let mut v = vec![0u8; len as usize];
        self.read(addr, &mut v)?;
        Ok(v)
    }

    /// Reads a NUL-terminated string of at most `max` bytes, without the NUL. A null pointer reads as an empty string.
    fn cstr(&self, addr: u64, max: usize) -> VkResult<String> {
        let mut out = Vec::new();
        if addr != 0 {
            for i in 0..max as u64 {
                let b = self.u8(addr + i)?;
                if b == 0 {
                    break;
                }
                out.push(b);
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Reads `n` little-endian `u32` at `addr`.
    fn u32s(&self, addr: u64, n: u32) -> VkResult<Vec<u32>> {
        let raw = self.bytes(addr, u64::from(check_count(n)?) * 4)?;
        Ok(raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect())
    }

    /// Reads `n` little-endian `u64` at `addr` (pointers or handles).
    fn u64s(&self, addr: u64, n: u32) -> VkResult<Vec<u64>> {
        let raw = self.bytes(addr, u64::from(check_count(n)?) * 8)?;
        Ok(raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_le_bytes(*c))
            .collect())
    }

    /// Reads `n` C strings through an array of `n` pointers (`ppEnabledExtensionNames`).
    fn cstrs(&self, addr: u64, n: u32) -> VkResult<Vec<String>> {
        if n == 0 || addr == 0 {
            return Ok(Vec::new());
        }
        self.u64s(addr, n)?
            .into_iter()
            .map(|p| self.cstr(p, 256))
            .collect()
    }

    /// The `(sType, address)` pairs of a `pNext` chain that starts at `first` (a `pNext` value).
    fn chain(&self, first: u64) -> VkResult<Vec<(u32, u64)>> {
        let mut out = Vec::new();
        let mut at = first;
        while at != 0 && out.len() < MAX_CHAIN {
            out.push((self.u32(at)?, at));
            at = self.u64(at + 8)?;
        }
        Ok(out)
    }
}

/// Checks the count of an array of the game against [`MAX_ARRAY`].
pub fn check_count(n: u32) -> VkResult<u32> {
    if n > MAX_ARRAY {
        return Err(VkError::OutOfHostMemory);
    }
    Ok(n)
}

/// Guest memory of the running game, through `nxkit_abi::guest`.
#[derive(Clone, Copy, Debug, Default)]
pub struct GuestMemory;

impl Mem for GuestMemory {
    fn read(&self, addr: u64, out: &mut [u8]) -> VkResult<()> {
        Ok(guest::read(addr, out)?)
    }

    fn write(&self, addr: u64, data: &[u8]) -> VkResult<()> {
        Ok(guest::write(addr, data)?)
    }
}

/// A memory of its own for tests: `len` zero bytes at the address `base`, and a bump allocator to place structs in it.
#[derive(Debug)]
pub struct TestMemory {
    base: u64,
    inner: Mutex<(Vec<u8>, u64)>,
}

impl TestMemory {
    /// `len` zero bytes at `base`.
    pub fn new(base: u64, len: usize) -> Self {
        Self {
            base,
            inner: Mutex::new((vec![0; len], 0)),
        }
    }

    /// Reserves `len` zero bytes aligned to 16 and gives their address. Returns 0 when the memory is full.
    pub fn alloc(&self, len: u64) -> u64 {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let at = g.1.next_multiple_of(16);
        if at + len > g.0.len() as u64 {
            return 0;
        }
        g.1 = at + len;
        self.base + at
    }

    /// Reserves room for `data` and copies it there.
    pub fn put(&self, data: &[u8]) -> u64 {
        let at = self.alloc(data.len() as u64);
        if at != 0 && self.write(at, data).is_err() {
            return 0;
        }
        at
    }

    fn span(&self, addr: u64, len: usize, size: usize) -> VkResult<std::ops::Range<usize>> {
        let fault = VkError::Fault {
            addr,
            len: len as u64,
        };
        let off = addr.checked_sub(self.base).ok_or(fault)? as usize;
        let end = off.checked_add(len).ok_or(fault)?;
        if end > size {
            return Err(fault);
        }
        Ok(off..end)
    }
}

impl Mem for TestMemory {
    fn read(&self, addr: u64, out: &mut [u8]) -> VkResult<()> {
        let g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let r = self.span(addr, out.len(), g.0.len())?;
        out.copy_from_slice(&g.0[r]);
        Ok(())
    }

    fn write(&self, addr: u64, data: &[u8]) -> VkResult<()> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let r = self.span(addr, data.len(), g.0.len())?;
        g.0[r].copy_from_slice(data);
        Ok(())
    }
}

/// A builder of one guest struct for tests: zero bytes of a size, with fields set at offsets.
#[derive(Clone, Debug, Default)]
pub struct StructBytes(pub Vec<u8>);

impl StructBytes {
    /// `size` zero bytes.
    pub fn new(size: u64) -> Self {
        Self(vec![0; size as usize])
    }

    fn set(mut self, off: u64, b: &[u8]) -> Self {
        let o = off as usize;
        if o + b.len() > self.0.len() {
            self.0.resize(o + b.len(), 0);
        }
        self.0[o..o + b.len()].copy_from_slice(b);
        self
    }

    /// Sets a `u32` (also an enum, a flags value or a `VkBool32`).
    pub fn u32(self, off: u64, v: u32) -> Self {
        self.set(off, &v.to_le_bytes())
    }

    /// Sets an `f32`.
    pub fn f32(self, off: u64, v: f32) -> Self {
        self.set(off, &v.to_le_bytes())
    }

    /// Sets a `u64` (also a pointer or a handle).
    pub fn u64(self, off: u64, v: u64) -> Self {
        self.set(off, &v.to_le_bytes())
    }

    /// Sets raw bytes.
    pub fn raw(self, off: u64, b: &[u8]) -> Self {
        self.set(off, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_reads_what_it_wrote_and_refuses_outside() {
        let m = TestMemory::new(0x1000, 256);
        let a = m.put(&StructBytes::new(16).u32(0, 7).u64(8, 0x1010).0);
        assert_eq!(a, 0x1000);
        assert_eq!(m.u32(a), Ok(7));
        assert_eq!(m.u64(a + 8), Ok(0x1010));
        assert!(m.u32(0xFFF).is_err());
        assert!(m.u64(0x1000 + 252).is_err());
        let s = m.put(b"VK_KHR_swapchain\0");
        let arr = m.put(&s.to_le_bytes());
        assert_eq!(m.cstrs(arr, 1), Ok(vec!["VK_KHR_swapchain".to_string()]));
        assert_eq!(m.cstr(0, 8), Ok(String::new()));
        assert_eq!(m.u32s(a, MAX_ARRAY + 1), Err(VkError::OutOfHostMemory));
    }

    #[test]
    fn a_chain_is_followed_to_its_end() {
        let m = TestMemory::new(0x2000, 256);
        let second = m.put(&StructBytes::new(16).u32(0, 22).0);
        let first = m.put(&StructBytes::new(16).u32(0, 11).u64(8, second).0);
        assert_eq!(m.chain(first), Ok(vec![(11, first), (22, second)]));
        assert_eq!(m.chain(0), Ok(vec![]));
    }
}
