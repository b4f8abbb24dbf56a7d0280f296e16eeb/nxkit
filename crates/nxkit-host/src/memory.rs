//! The reservation of guest memory (R18, R3).
//!
//! One `mmap` holds the modules, the main stack, the thread-local blocks, the heap, the stack region and the alias region. The range
//! is the guest range of `nxkit_abi::guest`, and the whole of it is the no-copy buffer that the GPU backend wraps.

use std::sync::OnceLock;

use crate::error::{HostError, Result};
use crate::layout::{Layout, PAGE};
use crate::sys::{self, PROT_NONE, PROT_RW, Reservation};

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

/// Reserves the address range, makes the host regions usable and registers the range with `nxkit_abi::guest`.
///
/// The module ranges stay without access until the loader gives them to dyld or maps them itself. The lowest page of
/// the main stack is a guard page.
pub fn init(layout: Layout) -> Result<&'static GuestMemory> {
    let res = sys::reserve(layout.all.start, layout.all.len)?;
    let rw = [layout.main_stack, layout.tls, layout.heap, layout.stack, layout.alias];
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
