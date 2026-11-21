//! The Vulkan backend of the nxkit host (R3, R64): the [`nxkit_vk::Backend`] of the Vulkan front end on
//! `nxkit-metal`, for games that draw with Vulkan (Hades, `0100535012974000`). It ports the behaviour of the old
//! engine's Objective-C layer (`hle_vk.m`), not its code.
//!
//! The modules, in layers (a module uses only modules of its layer or below):
//!
//! | layer | modules | what it holds |
//! |---|---|---|
//! | 0 | `settings`, `log`, `convert`, `queries`, `fixed`, `formats`, `table`, `bind`, `vertex`, `passplan` | the typed settings, the once-log, and pure functions: format and state maps, the shader table, which descriptor goes to which slot, the vertex slots, the load and store actions of render pass segments |
//! | 1 | `objects` | the Metal objects by Vulkan handle |
//! | 2 | `make`, `blit` | making images, views, samplers and pipelines; the present blit |
//! | 3 | `replay` | one submit into one Metal 4 command buffer |
//! | 4 | `gpu` | [`VkGpu`]: the backend, its submits, completions, presents and display ticks |
//!
//! The host (R4) makes the backend with [`VkGpu::new`] from a [`Context`](nxkit_metal::Context) and
//! [`VkGpuSettings`], installs it in the front end, makes a `Pacer` with it as the `TickSink` and hands the pacer back
//! with [`VkGpu::set_pacer`].
//!
//! The crate builds on macOS only. On other systems it is empty, so the workspace still builds there.

#![cfg(target_os = "macos")]

mod bind;
mod blit;
mod convert;
mod fixed;
mod formats;
mod gpu;
mod log;
mod make;
mod objects;
mod passplan;
mod queries;
mod replay;
mod settings;
mod table;
mod vertex;

pub use gpu::{MAX_PENDING, VkGpu};
pub use settings::{ShownHook, VkGpuSettings};
pub use table::ShaderTable;
