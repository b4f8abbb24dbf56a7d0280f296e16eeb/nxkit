//! GPU timestamps: a Metal 4 counter heap that compute passes write into, read on the CPU once the GPU is done.
//!
//! It is a diagnostic. A precise timestamp waits for the work before it, so the steps it splits run one after the
//! other and the times add up to a little more than the work takes without them.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSRange;
use objc2_metal::{
    MTL4CounterHeap, MTL4CounterHeapDescriptor, MTL4CounterHeapType, MTL4TimestampHeapEntry,
    MTLDevice,
};

use crate::device::Context;
use crate::error::{MetalError, Result};

/// The Metal heap behind a [`TimestampHeap`].
pub(crate) struct RawHeap(pub(crate) Retained<ProtocolObject<dyn MTL4CounterHeap>>);

// SAFETY: a counter heap is a Metal object that may be used from any thread; writes come from the GPU and reads go
// through `resolveCounterRange`, which Metal synchronises.
unsafe impl Send for RawHeap {}
// SAFETY: as above.
unsafe impl Sync for RawHeap {}

/// A heap of `count` GPU timestamps.
#[derive(Clone)]
pub struct TimestampHeap {
    pub(crate) raw: Arc<RawHeap>,
    count: usize,
    /// GPU ticks per second.
    frequency: u64,
}

impl TimestampHeap {
    /// Makes a heap of `count` timestamps.
    pub fn new(ctx: &Context, count: usize) -> Result<Self> {
        let desc = MTL4CounterHeapDescriptor::new();
        desc.setType(MTL4CounterHeapType::Timestamp);
        // SAFETY: a positive count; the heap holds that many entries.
        unsafe { desc.setCount(count.max(1)) };
        let device = ctx.device();
        let raw = device
            .newCounterHeapWithDescriptor_error(&desc)
            .map_err(|_| MetalError::Invalid("the device made no timestamp heap"))?;
        Ok(TimestampHeap {
            raw: Arc::new(RawHeap(raw)),
            count: count.max(1),
            frequency: device.queryTimestampFrequency().max(1),
        })
    }

    /// The number of timestamps.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Always false: a heap holds at least one timestamp.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The timestamps `0..n` in nanoseconds, or `None` when Metal has none to give. Call it only after the GPU
    /// finished the command buffers that wrote them.
    pub fn read_ns(&self, n: usize) -> Option<Vec<u64>> {
        let n = n.min(self.count);
        // SAFETY: the range lies inside the heap.
        let data = unsafe { self.raw.0.resolveCounterRange(NSRange::new(0, n)) }?;
        let bytes = data.to_vec();
        let entry = std::mem::size_of::<MTL4TimestampHeapEntry>();
        let ticks = bytes
            .chunks_exact(entry)
            .map(|c| {
                let mut a = [0u8; 8];
                a.copy_from_slice(&c[..8]);
                u64::from_ne_bytes(a)
            })
            .map(|t| (u128::from(t) * 1_000_000_000 / u128::from(self.frequency)) as u64)
            .collect();
        Some(ticks)
    }
}
