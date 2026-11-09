//! The guest memory as the front end reads it.

use std::sync::Arc;

use nxkit_metal::{AddressMap, GuestMemory, Result as MetalResult};
use nxkit_nvn::error::{NvnError, Result};
use nxkit_nvn::mem::GuestMem;

/// Reads and writes the mapped guest memory by host address.
pub(crate) struct GuestView(pub(crate) Arc<GuestMemory>);

impl GuestView {
    fn offset(&self, host: u64, len: usize) -> Result<u64> {
        let out = NvnError::OutOfRange {
            addr: host,
            len: len as u64,
        };
        host.checked_sub(self.0.host_base()).ok_or(out)
    }
}

impl GuestMem for GuestView {
    fn read(&self, host: u64, out: &mut [u8]) -> Result<()> {
        let at = self.offset(host, out.len())?;
        self.0.read(at, out).map_err(|_| NvnError::OutOfRange {
            addr: host,
            len: out.len() as u64,
        })
    }

    fn write(&mut self, host: u64, data: &[u8]) -> Result<()> {
        let at = self.offset(host, data.len())?;
        self.0.write(at, data).map_err(|_| NvnError::OutOfRange {
            addr: host,
            len: data.len() as u64,
        })
    }
}

/// CPU-only access to the guest memory: the address map and the host pointer, and no way to reach the Metal buffer.
/// It has the CPU methods of [`GuestMemory`] with the same names and signatures, so code that only reads and writes
/// guest bytes can take it, and code that needs a buffer address no longer compiles with it.
///
/// This crate forbids `unsafe`, so the space cannot hold a bare host pointer. It keeps the `Arc<GuestMemory>` instead:
/// the memory stays valid for as long as any copy of the space lives, and the caller has nothing to promise. The
/// buffer stays alive with it, which is the cost. When `nxkit-metal` offers a CPU-only mapping without a buffer (the
/// per-pool mode needs one), only the field of this type changes.
#[derive(Clone)]
pub(crate) struct GuestSpace(Arc<GuestMemory>);

#[allow(dead_code)] // the users come with the plumbing package
impl GuestSpace {
    /// The space over the memory of `mem`.
    pub(crate) fn over(mem: &Arc<GuestMemory>) -> Self {
        Self(Arc::clone(mem))
    }

    /// The map between host and GPU addresses.
    pub(crate) fn address_map(&self) -> AddressMap {
        self.0.address_map()
    }

    /// The length in bytes.
    pub(crate) fn len(&self) -> u64 {
        self.0.len()
    }

    /// Always false.
    pub(crate) fn is_empty(&self) -> bool {
        false
    }

    /// The host address of the first byte.
    pub(crate) fn host_base(&self) -> u64 {
        self.0.host_base()
    }

    /// Copies bytes out of the guest memory. The offset is from the start of the region.
    pub(crate) fn read(&self, offset: u64, out: &mut [u8]) -> MetalResult<()> {
        self.0.read(offset, out)
    }

    /// Copies bytes into the guest memory. The offset is from the start of the region.
    pub(crate) fn write(&self, offset: u64, data: &[u8]) -> MetalResult<()> {
        self.0.write(offset, data)
    }
}
