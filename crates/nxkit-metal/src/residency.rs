//! The residency of resources that were made outside this crate.
//!
//! Shaders reach memory through raw addresses, so every buffer, heap and texture they touch must be in the
//! residency set of the queue. The resources of this crate join it by themselves. A resource that another part of
//! the host made, for example a heap or a buffer from a window system, joins it with [`Context::make_resident`].

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::MTLAllocation;

use crate::device::Context;
use crate::error::{MetalError, Result};

/// Keeps one allocation in the residency set. Dropping it removes the allocation again.
///
/// Use one guard per allocation. The set holds an allocation once, so the first guard that drops removes it for
/// all. A change takes effect at the next [`Context::commit_residency`].
pub struct ResidentGuard {
    ctx: Context,
    allocation: Retained<ProtocolObject<dyn MTLAllocation>>,
}

// SAFETY: `MTLAllocation` objects are thread safe reference counted objects. The guard only holds one and calls
// `Context`, which serialises every change of the residency set with a lock.
unsafe impl Send for ResidentGuard {}
// SAFETY: see `Send`. A shared guard allows no calls.
unsafe impl Sync for ResidentGuard {}

impl Drop for ResidentGuard {
    fn drop(&mut self) {
        self.ctx.remove_allocation(&self.allocation);
    }
}

impl Context {
    /// Puts an allocation (a buffer, heap or texture) into the residency set until the guard drops.
    pub fn make_resident(&self, allocation: &ProtocolObject<dyn MTLAllocation>) -> ResidentGuard {
        self.add_allocation(allocation);
        ResidentGuard {
            ctx: self.clone(),
            allocation: allocation.retain(),
        }
    }

    /// Like [`Context::make_resident`], for a caller that holds the Metal object as a raw `id`.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a live Objective-C object that conforms to `MTLAllocation`.
    pub unsafe fn make_resident_raw(&self, ptr: *mut c_void) -> Result<ResidentGuard> {
        let ptr = NonNull::new(ptr).ok_or(MetalError::Invalid("the allocation pointer is null"))?;
        // SAFETY: the caller promises a live object that conforms to `MTLAllocation`. `retain` takes its own
        // reference, so the guard does not depend on the caller's one.
        let allocation =
            unsafe { Retained::retain(ptr.as_ptr().cast::<ProtocolObject<dyn MTLAllocation>>()) }
                .ok_or(MetalError::Invalid("the allocation pointer is null"))?;
        Ok(self.make_resident(&allocation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_metal::{MTLDevice, MTLResourceOptions};

    #[test]
    fn a_foreign_buffer_joins_and_leaves_the_residency_set() {
        let Ok(ctx) = Context::system_default() else {
            eprintln!("skipped: no Metal device");
            return;
        };
        let buffer = ctx
            .device()
            .newBufferWithLength_options(4096, MTLResourceOptions::StorageModeShared)
            .unwrap();
        let before = ctx.resident_count();
        let guard = ctx.make_resident(ProtocolObject::from_ref(&*buffer));
        ctx.commit_residency();
        assert_eq!(ctx.resident_count(), before + 1);
        drop(guard);
        ctx.commit_residency();
        assert_eq!(ctx.resident_count(), before);
        // SAFETY: the pointer is a live `MTLBuffer`, which conforms to `MTLAllocation`.
        let raw =
            unsafe { ctx.make_resident_raw(Retained::as_ptr(&buffer).cast_mut().cast()) }.unwrap();
        ctx.commit_residency();
        assert_eq!(ctx.resident_count(), before + 1);
        drop(raw);
        // SAFETY: a null pointer is checked and refused before it is used.
        assert!(unsafe { ctx.make_resident_raw(std::ptr::null_mut()) }.is_err());
    }
}
