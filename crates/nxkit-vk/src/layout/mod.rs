//! The byte layout of the Vulkan structs that the game passes and that the host fills in.
//!
//! The values are `offsetof` and `sizeof` of the official Vulkan headers (`vulkan_core.h`, header version 363) on a
//! 64-bit target, printed by a C program and copied here. A pointer, a `size_t`, a `VkDeviceSize` and every handle
//! take 8 bytes. Each module mirrors one struct whole, so some fields are not read yet. `SIZE` is the size of the
//! struct. A field named `size` is `SIZE_`.
#![allow(dead_code)]

pub(crate) mod command;
pub(crate) mod input;
pub(crate) mod output;
pub(crate) mod pipeline;

pub(crate) use command::*;
pub(crate) use input::*;
pub(crate) use output::*;
pub(crate) use pipeline::*;
