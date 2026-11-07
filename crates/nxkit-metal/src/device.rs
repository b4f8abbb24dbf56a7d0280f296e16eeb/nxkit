//! The device, the Metal 4 command queue, the compiler and the residency set.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTL4CommandQueue, MTL4Compiler, MTL4CompilerDescriptor, MTLAllocation,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLResidencySet, MTLResidencySetDescriptor,
};

use crate::error::{MetalError, Result, create};

pub(crate) type RawDevice = Retained<ProtocolObject<dyn MTLDevice>>;
pub(crate) type RawQueue = Retained<ProtocolObject<dyn MTL4CommandQueue>>;
pub(crate) type RawCompiler = Retained<ProtocolObject<dyn MTL4Compiler>>;
pub(crate) type RawResidency = Retained<ProtocolObject<dyn MTLResidencySet>>;

struct Inner {
    device: RawDevice,
    queue: RawQueue,
    compiler: RawCompiler,
    residency: RawResidency,
    residency_dirty: AtomicBool,
    // The residency set is not safe for concurrent changes, and a texture can drop on a Metal thread.
    residency_lock: Mutex<()>,
}

// SAFETY: the device, queue and compiler are documented as thread safe. All changes of the residency set
// take `residency_lock`.
unsafe impl Send for Inner {}
// SAFETY: see `Send`.
unsafe impl Sync for Inner {}

/// The device and everything that belongs to it for the whole run.
///
/// A `Context` is cheap to clone. All clones share the same device, queue and residency set.
#[derive(Clone)]
pub struct Context {
    inner: Arc<Inner>,
}

impl Context {
    /// Opens the system default device and makes the Metal 4 queue, the compiler and the residency set.
    pub fn system_default() -> Result<Self> {
        let device = MTLCreateSystemDefaultDevice().ok_or(MetalError::NoDevice(
            "MTLCreateSystemDefaultDevice returned nil",
        ))?;
        Self::with_device(device)
    }

    pub(crate) fn with_device(device: RawDevice) -> Result<Self> {
        let queue = device.newMTL4CommandQueue().ok_or(MetalError::NoDevice(
            "the device has no Metal 4 command queue",
        ))?;
        let compiler = device
            .newCompilerWithDescriptor_error(&MTL4CompilerDescriptor::new())
            .map_err(|e| create("MTL4Compiler", e.localizedDescription()))?;
        let residency = device
            .newResidencySetWithDescriptor_error(&MTLResidencySetDescriptor::new())
            .map_err(|e| create("residency set", e.localizedDescription()))?;
        queue.addResidencySet(&residency);
        Ok(Self {
            inner: Arc::new(Inner {
                device,
                queue,
                compiler,
                residency,
                residency_lock: Mutex::new(()),
                residency_dirty: AtomicBool::new(false),
            }),
        })
    }

    pub(crate) fn device(&self) -> &RawDevice {
        &self.inner.device
    }

    pub(crate) fn queue(&self) -> &RawQueue {
        &self.inner.queue
    }

    pub(crate) fn compiler(&self) -> &RawCompiler {
        &self.inner.compiler
    }

    /// The name of the GPU.
    pub fn name(&self) -> String {
        self.inner.device.name().to_string()
    }

    /// The largest buffer the device can make, in bytes.
    pub fn max_buffer_length(&self) -> u64 {
        self.inner.device.maxBufferLength() as u64
    }

    /// The recommended working set of the device, in bytes.
    pub fn recommended_working_set(&self) -> u64 {
        self.inner.device.recommendedMaxWorkingSetSize()
    }

    /// Puts an allocation into the residency set. The change takes effect at the next `commit_residency`.
    pub(crate) fn add_allocation(&self, allocation: &ProtocolObject<dyn MTLAllocation>) {
        let _g = self.inner.residency_lock.lock();
        self.inner.residency.addAllocation(allocation);
        self.inner.residency_dirty.store(true, Ordering::Release);
    }

    /// Removes an allocation from the residency set.
    pub(crate) fn remove_allocation(&self, allocation: &ProtocolObject<dyn MTLAllocation>) {
        let _g = self.inner.residency_lock.lock();
        self.inner.residency.removeAllocation(allocation);
        self.inner.residency_dirty.store(true, Ordering::Release);
    }

    /// Applies the pending residency changes. Call it before a frame that uses a new resource.
    pub fn commit_residency(&self) {
        let _g = self.inner.residency_lock.lock();
        if self.inner.residency_dirty.swap(false, Ordering::AcqRel) {
            self.inner.residency.commit();
            self.inner.residency.requestResidency();
        }
    }

    /// The number of allocations in the residency set.
    pub fn resident_count(&self) -> usize {
        self.inner.residency.allocationCount()
    }

    /// Adds the residency set of a `CAMetalLayer` to the queue.
    pub(crate) fn add_layer_residency(&self, set: &ProtocolObject<dyn MTLResidencySet>) {
        self.inner.queue.addResidencySet(set);
    }
}

pub(crate) fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}
