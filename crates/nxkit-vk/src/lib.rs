//! The Vulkan front end of the nxkit host: the API side of the Vulkan-on-Metal layer for games that render with
//! Vulkan (Hades, `0100535012974000`, on The Forge).
//!
//! This crate is pure Rust and uses only `std` (and `nxkit-abi` for guest memory, `nxb-model` for SHA-256). It has no
//! Metal code and no C ABI. It ports the behaviour of the reference layer (`hle_vk.m`):
//!
//! - [`device`]: the one device that the game sees (a Tegra X1, Vulkan 1.1, one queue family, three memory types on
//!   one 3 GB heap), its features, limits, formats and extensions, as plain data; [`write()`](mod@write) puts it into the game's
//!   structs.
//! - [`format`](mod@format): `VkFormat` as a pixel format, a vertex format, a texel or block size, aspects and conversions of this
//!   crate.
//! - [`handle`]: the handles that the game gets. Dispatchable ones are slots of guest memory that the ABI layer gives
//!   ([`handle::SlotAlloc`]).
//! - [`mem`] and [`info`]: guest memory and the decoders of the structs that the game passes, by field offsets of the
//!   official headers (the `layout` module), testable over a memory of their own.
//! - [`desc`]: the backend-neutral descriptions of objects; [`cmd`]: the recorded commands.
//! - [`backend::Backend`]: the seam to `nxkit-vk-gpu`, and [`recording::Recording`], a backend for tests.
//! - [`front::Vk`]: the front end that the ABI layer calls, with one front lock.
//! - [`procaddr`]: `vkGet*ProcAddr` name lookup.
//! - [`result`]: `VkResult` with the real codes.
//!
//! The ABI layer (`nxkit-rt::vk`) stays thin: an `extern "C" fn vkCreateImage(device, info, alloc, out)` reads the
//! create info with [`info::image`] over [`mem::GuestMemory`], calls [`front::Vk::create_image`] and writes the handle
//! to `out`.

pub mod backend;
pub mod cmd;
pub mod consts;
pub mod desc;
pub mod device;
pub mod format;
pub mod front;
pub mod handle;
pub mod info;
pub(crate) mod layout;
pub mod mem;
pub mod procaddr;
pub mod recording;
pub mod result;
pub mod write;

pub use backend::Backend;
pub use front::Vk;
pub use result::{Status, VkError, VkResult};
