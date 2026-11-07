//! The reservation of guest memory (R18, R3).
//!
//! One mapping holds the modules, the thread-local blocks, the shared memory area, the heap, the stack region with the main stack and the alias region. The range
//! is the guest range of `nxkit_abi::guest`, and the whole of it is the no-copy buffer that the GPU backend wraps.

use std::sync::OnceLock;

use crate::error::{HostError, Result};
use crate::layout::{Layout, PAGE};
use crate::sys::{PROT_NONE, PROT_RW, Reservation};

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

/// Makes the host regions usable and registers the range with `nxkit_abi::guest`.
///
/// `res` is the one reservation, which `place` made. The modules are in it already. The lowest page of the main stack
/// is a guard page.
pub fn init(layout: Layout, res: Reservation) -> Result<&'static GuestMemory> {
    let rw = [layout.tls, layout.shared, layout.heap, layout.stack, layout.alias];
    for r in rw {
        res.protect(r.start, r.len, PROT_RW)?;
    }
    res.protect(layout.main_stack.start, PAGE, PROT_NONE)?;
    nxkit_abi::guest::set_range(res.base(), res.len());
    GUEST
        .set(GuestMemory { layout, res })
        .map_err(|_| HostError::Memory("the guest memory is reserved twice".into()))?;
    get()
}
