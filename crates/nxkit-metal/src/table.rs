//! The argument table and the keep-alive list that the encoder uses.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTL4ArgumentTable, MTL4ArgumentTableDescriptor, MTLDevice, MTLTexture};

use crate::device::Context;
use crate::error::{MetalError, Result, create};
use crate::texture::Texture;

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
    bound: Rc<RefCell<Vec<Texture>>>,
}

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
            bound: Rc::new(RefCell::new(Vec::new())),
        })
    }

    /// Binds a GPU address to a buffer slot. The address is the one a shader reads through a pointer.
    pub fn set_buffer(&self, index: u32, gpu_addr: u64) -> Result<()> {
        if index >= self.max_buffers {
            return Err(MetalError::Invalid("buffer slot above the table size"));
        }
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
        self.bound.borrow_mut().push(texture.clone());
        Ok(())
    }
}
