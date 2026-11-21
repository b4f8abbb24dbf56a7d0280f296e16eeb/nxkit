//! The Vulkan C ABI of the host (R64): the `extern "C" fn vk*` that a Vulkan game (Hades, `0100535012974000`) imports
//! or asks `vkGet*ProcAddr` for.
//!
//! The Vulkan model is `nxkit-vk` (R2). A function here only unpacks the guest arguments: it reads the game's structs
//! with the decoders of `nxkit_vk::info` over guest memory, calls the front end (`nxkit_vk::Vk`) of the installed
//! context and writes the outputs with `nxkit_vk::write`. The conventions and the fatal errors are in `abi`.
//!
//! The groups, each with its `procs()`: `instance` (instance, physical device, device, surface, `vkGet*ProcAddr`, the
//! debug extensions as no-ops), `resources` (memory, buffers, images, views, samplers), `pipelines` (render passes,
//! shader modules, pipelines, layouts, descriptor sets), `commands` and `transfer` (command pools and buffers, the
//! recorded commands) and `queue` (submits, sync objects, query pools, the swapchain and the present).
//!
//! The functions are rows of the `gpu` service in `nxkit_rt::entries_for(Graphics::Vulkan)` and only there: the host
//! names the graphics API of the game and gets the table of that layer. A function marked `[big]` runs on the big
//! host stack: the ones that reach the backend and Metal (device creation, memory storage, textures, samplers, shader
//! modules, pipelines, query pools, submits, waits, the swapchain and the present) and the descriptor updates, which
//! decode arrays of the game on worker threads whose stacks the host does not know. The host calls [`install`] once
//! with the backend, before the game starts.

pub(crate) mod abi;
mod commands;
pub(crate) mod ctx;
mod instance;
mod pipelines;
mod queue;
mod resources;
mod slots;
mod transfer;

pub use ctx::{InstallError, Settings, clock_ns, install};

use std::collections::HashMap;

use nxkit_abi::export::Export;
use nxkit_vk::result::Log;

/// The longest function name that a game asks for.
const NAME_MAX: usize = 256;

/// One function of the layer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Proc {
    /// The Vulkan name.
    pub(crate) name: &'static str,
    /// The address of the `extern "C"` function.
    pub(crate) addr: usize,
    /// True when it runs on the big host stack.
    pub(crate) big: bool,
}

/// Every function of the groups, in the order of the groups. The table of `entries_for` and the context keep what
/// they need of it, so it is built twice per process.
pub(crate) fn procs() -> Vec<Proc> {
    let mut all = Vec::new();
    all.extend(instance::procs());
    all.extend(resources::procs());
    all.extend(pipelines::procs());
    all.extend(commands::procs());
    all.extend(transfer::procs());
    all.extend(queue::procs());
    all
}

/// The functions of the layer as exports of the `gpu` service: those that need the big host stack (`big`) or the rest.
pub(crate) fn exports(big: bool) -> Vec<Export> {
    procs()
        .into_iter()
        .filter(|p| p.big == big)
        .map(|p| Export::new(p.name, p.addr))
        .collect()
}

/// The address that `vkGet*ProcAddr` gives for `name`: the function of `by_name`, with the retry without `KHR` of
/// `nxkit_vk::procaddr`, as its big-stack stub when `stubs` has one. 0 for a name that no group exports (logged once
/// in `log`).
pub(crate) fn find_proc(
    log: &Log,
    by_name: &HashMap<&'static str, usize>,
    stubs: &HashMap<usize, usize>,
    name: &str,
) -> u64 {
    nxkit_vk::procaddr::resolve(log, name, |n| by_name.get(n).copied())
        .map_or(0, |a| stubs.get(&a).copied().unwrap_or(a) as u64)
}

/// `vkGetInstanceProcAddr` and `vkGetDeviceProcAddr` with the installed context. `fname` names the caller.
pub(crate) fn lookup(fname: &str, name: &str) -> u64 {
    let c = ctx::ctx(fname);
    find_proc(c.vk.log(), &c.by_name, &c.settings.stubs, name)
}

/// Reads the name that a game passes to `vkGet*ProcAddr`.
pub(crate) fn name_at(name: u64) -> nxkit_vk::VkResult<String> {
    let bytes = nxkit_abi::guest::read_cstr(name, NAME_MAX)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_abi;
