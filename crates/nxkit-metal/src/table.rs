//! The argument table and the keep-alive list that the encoder uses.

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTL4ArgumentTable, MTL4ArgumentTableDescriptor, MTLDevice, MTLTexture};

use crate::address::{GUEST_VA_BASE, GUEST_VA_MAX, is_guest_va};
use crate::device::Context;
use crate::error::{MetalError, Result, create};
use crate::texture::Texture;

/// How many addresses from the window of virtual guest addresses reached Metal.
static GUEST_VA_HITS: AtomicU64 = AtomicU64::new(0);

/// The hits that print a line. Later hits are counted only, so a draw loop cannot flood the log.
const GUEST_VA_LOGGED: u64 = 16;

/// The tripwire: refuses an address in the window of virtual guest addresses. Such an address was not turned into a
/// real one, and Metal would fault on it. Address 0 and every real address pass. A hit is counted, the first hits
/// print the address, and the call returns an error, so the caller skips the command.
pub(crate) fn check_real(addr: u64) -> Result<()> {
    if !is_guest_va(addr) {
        return Ok(());
    }
    let n = GUEST_VA_HITS.fetch_add(1, Ordering::Relaxed);
    if n < GUEST_VA_LOGGED {
        eprintln!(
            "nxkit-metal: a guest address reached Metal: {addr:#x} is in the window {GUEST_VA_BASE:#x}..{GUEST_VA_MAX:#x} (hit {})",
            n + 1
        );
    }
    Err(MetalError::Invalid("a guest address reached Metal"))
}

/// A value kept alive for the GPU.
pub(crate) struct Keep(#[allow(dead_code)] Arc<dyn Any>);

impl Keep {
    pub(crate) fn new(v: impl Any) -> Self {
        Keep(Arc::new(v))
    }
}

// SAFETY: a `Keep` is never read, it only holds a reference count that is dropped on another
// thread. Metal objects are thread-safe reference-counted objects, so dropping them anywhere is fine.
unsafe impl Send for Keep {}
// SAFETY: see `Send`. Nothing can be done with a shared `Keep`.
unsafe impl Sync for Keep {}

/// A table that binds buffer addresses and textures to the slots of a shader.
#[derive(Clone)]
pub struct ArgumentTable {
    pub(crate) raw: Retained<ProtocolObject<dyn MTL4ArgumentTable>>,
    max_buffers: u32,
    max_textures: u32,
    bound: Arc<Mutex<Vec<Texture>>>,
}

// SAFETY: the table is a Metal object that one thread fills while it encodes. The `bound` list has a lock. A table
// is moved to the thread that encodes a pass and is not filled from two threads at once.
unsafe impl Send for ArgumentTable {}
// SAFETY: see `Send`. Shared access only reads the slot counts.
unsafe impl Sync for ArgumentTable {}

impl ArgumentTable {
    /// Makes a table with `max_buffers` address slots and `max_textures` texture slots.
    pub fn new(ctx: &Context, max_buffers: u32, max_textures: u32) -> Result<Self> {
        let d = MTL4ArgumentTableDescriptor::new();
        d.setMaxBufferBindCount(max_buffers as usize);
        d.setMaxTextureBindCount(max_textures as usize);
        let raw = ctx
            .device()
            .newArgumentTableWithDescriptor_error(&d)
            .map_err(|e| create("argument table", e.localizedDescription()))?;
        Ok(Self {
            raw,
            max_buffers,
            max_textures,
            bound: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Binds a GPU address to a buffer slot. The address is the one a shader reads through a pointer.
    pub fn set_buffer(&self, index: u32, gpu_addr: u64) -> Result<()> {
        if index >= self.max_buffers {
            return Err(MetalError::Invalid("buffer slot above the table size"));
        }
        check_real(gpu_addr)?;
        // SAFETY: the index is below the table's buffer count (checked above). The address is data: a wrong one
        // makes the GPU fault on use, which is not a memory-safety matter for the host.
        unsafe { self.raw.setAddress_atIndex(gpu_addr, index as usize) };
        Ok(())
    }

    /// Binds a texture to a texture slot. The table keeps the texture alive.
    pub fn set_texture(&self, index: u32, texture: &Texture) -> Result<()> {
        if index >= self.max_textures {
            return Err(MetalError::Invalid("texture slot above the table size"));
        }
        // SAFETY: the index is below the table's texture count (checked above) and the resource id comes from a
        // live texture, which `bound` keeps alive.
        unsafe {
            self.raw
                .setTexture_atIndex(texture.raw().gpuResourceID(), index as usize)
        };
        self.bound
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(texture.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_real_refuses_the_window_only() {
        assert!(check_real(0).is_ok());
        assert!(check_real(0x15_0000_0000).is_ok());
        assert!(check_real(GUEST_VA_BASE - 1).is_ok());
        assert!(check_real(GUEST_VA_MAX).is_ok());
        assert_eq!(
            check_real(GUEST_VA_BASE),
            Err(MetalError::Invalid("a guest address reached Metal"))
        );
        assert!(check_real(GUEST_VA_MAX - 1).is_err());
    }
}
