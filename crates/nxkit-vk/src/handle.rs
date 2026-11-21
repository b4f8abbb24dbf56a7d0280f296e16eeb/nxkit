//! The handles that the game gets, and the registries that map them to objects.
//!
//! A non-dispatchable handle (`VkImage`, `VkBuffer`, ...) is a 64-bit value that the front end makes up: the kind of the
//! object in the top byte and a serial number below it ([`Kind::handle`]). Serials are never reused, so a handle that
//! the game destroyed never names a newer object, and a call with a destroyed or a wrong-kind handle finds nothing.
//!
//! A dispatchable handle (`VkInstance`, `VkPhysicalDevice`, `VkDevice`, `VkQueue`, `VkCommandBuffer`) is a pointer to
//! memory whose first word is the slot of the loader (`VK_LOADER_DATA`). The ABI layer owns that memory: the front end
//! asks a [`SlotAlloc`] for a slot and frees it when the object goes. The slot address is the handle.
//!
//! Destroying a handle twice or destroying the null handle does nothing, as the reference layer allows.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::PoisonError;

/// The value that the loader interface puts in the first word of a dispatchable object (`ICD_LOADER_MAGIC`).
pub const LOADER_MAGIC: u64 = 0x01CD_C0DE;

/// The bytes of one dispatchable slot. The first word is the loader slot, the rest is free.
pub const SLOT_BYTES: u64 = 16;

/// The kind of an object, in the top byte of its handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Kind {
    /// `VkInstance` (dispatchable).
    Instance = 1,
    /// `VkPhysicalDevice` (dispatchable).
    PhysicalDevice,
    /// `VkDevice` (dispatchable).
    Device,
    /// `VkQueue` (dispatchable).
    Queue,
    /// `VkCommandBuffer` (dispatchable).
    CommandBuffer,
    /// `VkSurfaceKHR`.
    Surface,
    /// `VkSwapchainKHR`.
    Swapchain,
    /// `VkDeviceMemory`.
    Memory,
    /// `VkBuffer`.
    Buffer,
    /// `VkBufferView`.
    BufferView,
    /// `VkImage`.
    Image,
    /// `VkImageView`.
    ImageView,
    /// `VkSampler`.
    Sampler,
    /// `VkDescriptorSetLayout`.
    SetLayout,
    /// `VkDescriptorPool`.
    DescriptorPool,
    /// `VkDescriptorSet`.
    DescriptorSet,
    /// `VkDescriptorUpdateTemplate`.
    UpdateTemplate,
    /// `VkPipelineLayout`.
    PipelineLayout,
    /// `VkRenderPass`.
    RenderPass,
    /// `VkFramebuffer`.
    Framebuffer,
    /// `VkShaderModule`.
    ShaderModule,
    /// `VkPipelineCache`.
    PipelineCache,
    /// `VkPipeline`.
    Pipeline,
    /// `VkCommandPool`.
    CommandPool,
    /// `VkFence`.
    Fence,
    /// `VkSemaphore`.
    Semaphore,
    /// `VkEvent`.
    Event,
    /// `VkQueryPool`.
    QueryPool,
    /// `VkDebugReportCallbackEXT` and `VkDebugUtilsMessengerEXT`: tokens that are never called.
    DebugCallback,
}

impl Kind {
    /// The handle of serial `serial` of this kind.
    pub fn handle(self, serial: u64) -> u64 {
        (self as u64) << 56 | (serial & 0x00FF_FFFF_FFFF_FFFF)
    }

    /// Whether a handle value names an object of this kind. Dispatchable kinds are guest addresses and have no tag.
    pub fn tags(self, handle: u64) -> bool {
        handle >> 56 == self as u64
    }
}

/// The objects of one kind, keyed by handle.
#[derive(Debug)]
pub struct Registry<T> {
    kind: Kind,
    next: u64,
    map: HashMap<u64, T>,
}

impl<T> Registry<T> {
    /// An empty registry for objects of `kind`.
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            next: 1,
            map: HashMap::new(),
        }
    }

    /// Adds an object under a new handle and gives the handle.
    pub fn insert(&mut self, value: T) -> u64 {
        let h = self.kind.handle(self.next);
        self.next += 1;
        self.map.insert(h, value);
        h
    }

    /// Adds an object under a handle that the caller chose (a dispatchable slot address).
    pub fn insert_at(&mut self, handle: u64, value: T) {
        self.map.insert(handle, value);
    }

    /// The object of a handle. The null handle and a destroyed one give `None`.
    pub fn get(&self, handle: u64) -> Option<&T> {
        self.map.get(&handle)
    }

    /// The object of a handle, to change.
    pub fn get_mut(&mut self, handle: u64) -> Option<&mut T> {
        self.map.get_mut(&handle)
    }

    /// Removes an object. A second remove or the null handle gives `None` and changes nothing.
    pub fn remove(&mut self, handle: u64) -> Option<T> {
        self.map.remove(&handle)
    }

    /// The number of live objects.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether no object lives.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The live objects.
    pub fn iter(&self) -> impl Iterator<Item = (&u64, &T)> {
        self.map.iter()
    }
}

/// Where dispatchable objects live: memory that the game can read, given by the ABI layer.
///
/// [`SlotAlloc::alloc`] gives [`SLOT_BYTES`] bytes of guest memory, with [`LOADER_MAGIC`] in the first word and zero
/// in the rest. The address is the handle of the object. [`SlotAlloc::free`] gives it back.
pub trait SlotAlloc: Send + Sync {
    /// A new slot, or `None` when guest memory is full.
    fn alloc(&self, kind: Kind) -> Option<u64>;

    /// Gives a slot back. A slot that was not given out is ignored.
    fn free(&self, addr: u64);
}

/// A [`SlotAlloc`] for tests: addresses from `base` upwards in steps of [`SLOT_BYTES`], with no memory behind them.
#[derive(Debug)]
pub struct CountingSlots {
    base: u64,
    state: Mutex<(u64, Vec<u64>)>,
}

impl CountingSlots {
    /// Slots from `base`.
    pub fn new(base: u64) -> Self {
        Self {
            base,
            state: Mutex::new((0, Vec::new())),
        }
    }

    /// The slots that are given out now.
    pub fn live(&self) -> Vec<u64> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .clone()
    }
}

impl SlotAlloc for CountingSlots {
    fn alloc(&self, _kind: Kind) -> Option<u64> {
        let mut g = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let a = self.base + g.0 * SLOT_BYTES;
        g.0 += 1;
        g.1.push(a);
        Some(a)
    }

    fn free(&self, addr: u64) {
        let mut g = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        g.1.retain(|&a| a != addr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_carry_their_kind_and_are_not_reused() {
        let mut r = Registry::new(Kind::Image);
        let a = r.insert(1);
        let b = r.insert(2);
        assert_ne!(a, b);
        assert!(Kind::Image.tags(a));
        assert!(!Kind::Buffer.tags(a));
        assert_eq!(r.remove(a), Some(1));
        assert_eq!(r.remove(a), None);
        assert_eq!(r.remove(0), None);
        let c = r.insert(3);
        assert_ne!(c, a);
        assert_eq!(r.get(b), Some(&2));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn counting_slots_track_what_is_live() {
        let s = CountingSlots::new(0x100);
        let a = s.alloc(Kind::Queue);
        let b = s.alloc(Kind::CommandBuffer);
        assert_eq!((a, b), (Some(0x100), Some(0x110)));
        s.free(0x100);
        s.free(0x999);
        assert_eq!(s.live(), vec![0x110]);
    }
}
