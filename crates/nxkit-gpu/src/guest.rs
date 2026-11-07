//! The guest memory as the front end reads it.

use std::sync::Arc;

use nxkit_metal::GuestMemory;
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
