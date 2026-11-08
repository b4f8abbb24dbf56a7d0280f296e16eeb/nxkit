//! The reservation of guest memory (R18, R3).
//!
//! One mapping holds the modules, the thread-local blocks, the shared memory area, the heap, the stack region with the main stack and the alias region. The range
//! is the guest range of `nxkit_abi::guest`, and the whole of it is the no-copy buffer that the GPU backend wraps.

use std::sync::OnceLock;

use crate::error::{HostError, Result};
use crate::layout::{Layout, PAGE, Range};
use crate::sys::{PROT_NONE, Reservation};

/// The reservation and its map.
#[derive(Debug)]
pub struct GuestMemory {
    /// Where the regions sit.
    pub layout: Layout,
    /// The mapping.
    pub res: Reservation,
}

static GUEST: OnceLock<GuestMemory> = OnceLock::new();

/// The reservation, after [`init`].
pub fn get() -> Result<&'static GuestMemory> {
    GUEST
        .get()
        .ok_or_else(|| HostError::Memory("the guest memory is not reserved yet".into()))
}

/// Registers the range with `nxkit_abi::guest`.
///
/// `res` is the one reservation, which `place` made. It is read and write everywhere except where the library or the
/// copy made code `r-x`, and the modules are in it already. No page is `PROT_NONE` yet, so the GPU buffer can wrap the
/// range (`GuestMemory::gpu_memory`). [`GuestMemory::seal`] makes the guard page after that.
pub fn init(layout: Layout, res: Reservation) -> Result<&'static GuestMemory> {
    nxkit_abi::guest::set_range(res.base(), res.len());
    GUEST
        .set(GuestMemory { layout, res })
        .map_err(|_| HostError::Memory("the guest memory is reserved twice".into()))?;
    get()
}

impl GuestMemory {
    /// The range that the GPU buffer covers: everything behind the modules, which is the thread-local blocks, the
    /// shared memory area, the heap, the stack region and the alias region. The modules hold file-mapped pages, and a
    /// residency set over them makes them unreadable.
    pub fn gpu_range(&self) -> Range {
        let start = self.layout.tls.start;
        Range { start, len: self.layout.all.end() - start }
    }

    /// Creates the GPU buffer over [`GuestMemory::gpu_range`].
    pub fn gpu_memory(&self, ctx: &nxkit_metal::Context) -> Result<nxkit_metal::GuestMemory> {
        let r = self.gpu_range();
        self.res.as_gpu_memory(ctx, r.start, r.len)
    }

    /// Makes the lowest page of the main stack a guard page. It runs after the GPU buffer exists, because Metal
    /// refuses a range with a `PROT_NONE` page.
    pub fn seal(&self) -> Result<()> {
        self.res.protect(self.layout.main_stack.start, PAGE, PROT_NONE)
    }
}
