//! Access to guest memory.
//!
//! The recorder writes its records into the game's own command memory, and the decoder reads them back.
//! This crate has no `unsafe` code, so it cannot hold a raw pointer. The host implements [`GuestMem`] over the real mapping.
//! [`VecMem`] is an implementation over a `Vec` for tests and tools.

use crate::error::{NvnError, Result};

/// Reads and writes bytes of guest memory by host address.
pub trait GuestMem {
    /// Copies `out.len()` bytes from `host` into `out`.
    fn read(&self, host: u64, out: &mut [u8]) -> Result<()>;

    /// Copies `data` to `host`.
    fn write(&mut self, host: u64, data: &[u8]) -> Result<()>;
}

/// A block of memory at a fixed host address.
#[derive(Clone, Debug)]
pub struct VecMem {
    base: u64,
    bytes: Vec<u8>,
}

impl VecMem {
    /// `len` zero bytes that start at `base`.
    pub fn new(base: u64, len: usize) -> Self {
        Self {
            base,
            bytes: vec![0; len],
        }
    }

    fn range(&self, host: u64, len: usize) -> Result<std::ops::Range<usize>> {
        let out = NvnError::OutOfRange {
            addr: host,
            len: len as u64,
        };
        let start = host.checked_sub(self.base).ok_or(out.clone())?;
        let end = start.checked_add(len as u64).ok_or(out.clone())?;
        if end > self.bytes.len() as u64 {
            return Err(out);
        }
        Ok(start as usize..end as usize)
    }
}

impl GuestMem for VecMem {
    fn read(&self, host: u64, out: &mut [u8]) -> Result<()> {
        let r = self.range(host, out.len())?;
        out.copy_from_slice(&self.bytes[r]);
        Ok(())
    }

    fn write(&mut self, host: u64, data: &[u8]) -> Result<()> {
        let r = self.range(host, data.len())?;
        self.bytes[r].copy_from_slice(data);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_was_written_and_rejects_outside() {
        let mut m = VecMem::new(0x1000, 16);
        m.write(0x1008, &[1, 2, 3]).unwrap();
        let mut b = [0u8; 3];
        m.read(0x1008, &mut b).unwrap();
        assert_eq!(b, [1, 2, 3]);
        assert!(m.write(0x100e, &[0; 4]).is_err());
        assert!(m.read(0xfff, &mut b).is_err());
        assert!(m.read(u64::MAX, &mut b).is_err());
    }
}
