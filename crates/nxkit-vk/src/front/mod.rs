//! The front end: [`Vk`], the one object that the ABI layer calls for every Vulkan function.
//!
//! One `Vk` exists per process. `nxkit-rt` installs it like the NVN context. It holds:
//!
//! - the state of every object behind one lock (the front lock). A call takes it for its own bookkeeping and gives it
//!   back before it calls the backend, so a slow backend call (a texture, a pipeline, a submit) does not hold up the
//!   other guest threads. The lock is not re-entrant, and the backend never calls back into `Vk`.
//! - the queue lock, taken by a submit around the numbering and the hand-over to the backend, so the backend gets the
//!   submits in serial order while the front lock is free. Lock order: queue, then front.
//! - the backend ([`Backend`]) and the slot allocator of dispatchable handles ([`SlotAlloc`]).
//!
//! A method takes values that the ABI layer read with the decoders of [`crate::info`] and gives handles, values or a
//! [`VkResult`]. A destroy of a null, unknown or already destroyed handle does nothing.

mod descriptors;
mod pipelines;
mod queue;
mod record;
mod record_more;
mod resources;
mod state;
mod swapchain;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::backend::Backend;
use crate::device::DeviceCaps;
use crate::handle::{Kind, SlotAlloc};
use crate::info::{DeviceInfo, InstanceInfo};
use crate::result::{Log, VkError, VkResult};

pub use queue::QueryResults;
pub(crate) use state::State;

/// The Vulkan front end.
pub struct Vk {
    backend: Arc<dyn Backend>,
    slots: Arc<dyn SlotAlloc>,
    caps: DeviceCaps,
    state: Mutex<State>,
    queue: Mutex<QueueClock>,
    log: Log,
}

/// The numbering of submits.
#[derive(Debug, Default)]
struct QueueClock {
    /// The serial of the last submit.
    last: u64,
}

impl std::fmt::Debug for Vk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vk")
            .field("caps", &self.caps)
            .finish_non_exhaustive()
    }
}

impl Vk {
    /// A front end over a backend and the slot allocator of the ABI layer.
    pub fn new(backend: Arc<dyn Backend>, slots: Arc<dyn SlotAlloc>) -> Self {
        let caps = backend.caps();
        Self {
            backend,
            slots,
            caps,
            state: Mutex::new(State::new()),
            queue: Mutex::new(QueueClock::default()),
            log: Log::default(),
        }
    }

    /// The error for a handle that names no live object of the right kind (logged once per kind).
    pub(crate) fn bad(&self, what: &'static str, h: u64) -> VkError {
        self.log
            .once(what, format_args!("[RED] {what}: unknown handle {h:#x}"));
        VkError::Unknown
    }

    /// The log of the front end.
    pub fn log(&self) -> &Log {
        &self.log
    }

    /// `vkGetInstanceProcAddr` and `vkGetDeviceProcAddr`: looks `name` up with `find` in the ABI layer's table,
    /// with the retry without `KHR` (see [`crate::procaddr`]).
    pub fn resolve_proc<T>(&self, name: &str, find: impl Fn(&str) -> Option<T>) -> Option<T> {
        crate::procaddr::resolve(&self.log, name, find)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The device caps that the backend gave.
    pub fn caps(&self) -> DeviceCaps {
        self.caps
    }

    /// The backend.
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    fn slot(&self, kind: Kind) -> VkResult<u64> {
        self.slots.alloc(kind).ok_or(VkError::OutOfHostMemory)
    }

    /// `vkCreateInstance`: the instance and its one physical device. A second create gives the same instance.
    pub fn create_instance(&self, info: &InstanceInfo) -> VkResult<u64> {
        eprintln!(
            "[vk] CreateInstance: application {}, engine {}, API {}.{}; extensions {}; {} layers",
            info.application,
            info.engine,
            info.api_version >> 22 & 0x7F,
            info.api_version >> 12 & 0x3FF,
            info.extensions.join(" "),
            info.layers
        );
        let mut s = self.lock();
        if let Some(i) = s.instance {
            return Ok(i);
        }
        let i = self.slot(Kind::Instance)?;
        let p = self.slot(Kind::PhysicalDevice)?;
        s.instance = Some(i);
        s.physical = Some(p);
        Ok(i)
    }

    /// `vkDestroyInstance`: the instance and its physical device stay, so a game that makes a new one gets them back.
    pub fn destroy_instance(&self, _instance: u64) {}

    /// The physical devices: the one device, or none before an instance.
    pub fn physical_devices(&self) -> Vec<u64> {
        self.lock().physical.into_iter().collect()
    }

    /// `vkCreateDevice`: the device and its queue. A second create gives the same device.
    pub fn create_device(&self, info: &DeviceInfo) -> VkResult<u64> {
        eprintln!(
            "[vk] CreateDevice: {} queues; extensions {}",
            info.queues,
            info.extensions.join(" ")
        );
        let unknown: Vec<&String> = info
            .extensions
            .iter()
            .filter(|e| !crate::device::DEVICE_EXTENSIONS.contains(&e.as_str()))
            .collect();
        if !unknown.is_empty() {
            // As the reference layer: the device is made anyway, the functions of such an extension are no-ops or
            // unsupported.
            self.log.once(
                "device extensions",
                format_args!(
                    "[RED] the game enables device extensions that are not listed: {unknown:?}"
                ),
            );
        }
        let mut s = self.lock();
        if let Some(d) = s.device {
            return Ok(d);
        }
        let d = self.slot(Kind::Device)?;
        let q = self.slot(Kind::Queue)?;
        s.device = Some(d);
        s.queue = Some(q);
        Ok(d)
    }

    /// `vkDestroyDevice`: the device stays (the game may make it again).
    pub fn destroy_device(&self, _device: u64) {}

    /// `vkGetDeviceQueue`: every queue of every family is the one serial queue.
    pub fn queue(&self) -> u64 {
        self.lock().queue.unwrap_or(0)
    }

    /// `vkCreateViSurfaceNN`: a surface on a vi layer window.
    pub fn create_surface(&self, window: u64) -> u64 {
        eprintln!("[vk] CreateViSurfaceNN window {window:#x}");
        self.lock().surfaces.insert(window)
    }

    /// `vkDestroySurfaceKHR`.
    pub fn destroy_surface(&self, surface: u64) {
        self.lock().surfaces.remove(surface);
    }

    /// `vkGetPhysicalDeviceSurfaceSupportKHR`: family 0 presents.
    pub fn surface_support(&self, family: u32) -> bool {
        family == 0
    }

    /// `vkCreateDebugReportCallbackEXT`, `vkCreateDebugUtilsMessengerEXT`: a token that is never called (the layer
    /// reports nothing).
    pub fn create_debug_token(&self) -> u64 {
        self.lock().tokens.insert(())
    }

    /// `vkDestroyDebugReportCallbackEXT`, `vkDestroyDebugUtilsMessengerEXT`.
    pub fn destroy_debug_token(&self, token: u64) {
        self.lock().tokens.remove(token);
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_flow;
