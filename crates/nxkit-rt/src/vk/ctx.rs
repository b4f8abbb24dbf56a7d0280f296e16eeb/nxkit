//! The process-wide Vulkan context of the C ABI.
//!
//! One [`Ctx`] exists per process. The host installs it with [`install`] before the game starts, with the backend
//! (`nxkit-vk-gpu` implements [`Backend`]) and the [`Settings`] of the game. A function of the C ABI gets it with
//! [`ctx`]. A call before `install` ends the game with the name of the function (R15). The context holds the front end
//! ([`Vk`], with the slot allocator of [`super::slots`]), the settings and the name table of `vkGet*ProcAddr`.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use nxkit_vk::{Backend, Vk};

use super::abi::fatal;
use super::slots::GuestSlots;

/// What the host decides for the Vulkan layer.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The guest clock in nanoseconds: the time of a present in the frame history of `nn::vi`. The default is the
    /// monotonic clock of the host, as for NVN.
    pub clock_ns: fn() -> u64,
    /// The big-stack stubs of the functions that need the big host stack, by the address of the function. The loader
    /// puts a stub into an import slot itself; `vkGet*ProcAddr` gives the stub of a function that has one here. The
    /// host fills it from the big-stack rows of `nxkit_rt::entries_for(Graphics::Vulkan)` on the loader thread.
    pub stubs: HashMap<usize, usize>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            clock_ns: nxkit_hos::sys::monotonic_ns,
            stubs: HashMap::new(),
        }
    }
}

/// Why [`install`] failed.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallError {
    /// A context was installed before.
    AlreadyInstalled,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstallError::AlreadyInstalled => f.write_str("a Vulkan context is installed already"),
        }
    }
}

impl std::error::Error for InstallError {}

/// The Vulkan context. See the module documentation.
pub(crate) struct Ctx {
    /// The front end.
    pub(crate) vk: Vk,
    /// The settings of the host.
    pub(crate) settings: Settings,
    /// Every function of the layer by name, for `vkGet*ProcAddr`.
    pub(crate) by_name: HashMap<&'static str, usize>,
    /// The slot allocator of the front end, kept for tests.
    #[cfg(test)]
    pub(crate) slots: Arc<GuestSlots>,
}

// Process-wide (W26): the Vulkan context root: every `vk*` function is a C function of the guest and finds its state here.
static VK: OnceLock<Ctx> = OnceLock::new();

/// Installs the Vulkan context. The host calls it once, for a game whose `game.json` says `"graphics": "vulkan"`.
pub fn install(backend: Arc<dyn Backend>, settings: Settings) -> Result<(), InstallError> {
    if VK.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }
    let slots = Arc::new(GuestSlots::default());
    let ctx = Ctx {
        vk: Vk::new(backend, slots.clone()),
        settings,
        by_name: super::procs()
            .into_iter()
            .map(|p| (p.name, p.addr))
            .collect(),
        #[cfg(test)]
        slots,
    };
    VK.set(ctx).map_err(|_| InstallError::AlreadyInstalled)
}

/// The installed context. A call before [`install`] ends the game with the name of the function.
pub(crate) fn ctx(name: &str) -> &'static Ctx {
    VK.get()
        .unwrap_or_else(|| fatal(name, "no Vulkan backend is installed"))
}

/// The installed context, None before [`install`].
pub(crate) fn installed() -> Option<&'static Ctx> {
    VK.get()
}

/// The guest clock of the Vulkan layer in nanoseconds. 0 before [`install`].
pub fn clock_ns() -> u64 {
    installed().map_or(0, |c| (c.settings.clock_ns)())
}
