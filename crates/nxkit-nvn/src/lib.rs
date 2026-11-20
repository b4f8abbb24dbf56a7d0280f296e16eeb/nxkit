//! The NVN front end of the nxkit host.
//!
//! This crate is pure Rust. It uses `std`, `foldhash` and the settings snapshot of `nxkit-abi`. It has no Metal code and
//! no FFI. It does three jobs.
//!
//! - It maps guest memory to GPU addresses ([`space`]). The GPU address that NVN gives the game is the Metal address.
//! - It models textures: formats, storage layout, block-linear order, the descriptors that the game writes
//!   ([`mod@format`], [`layout`], [`swizzle`], [`tic`]), and the pools and the resource table ([`descriptors`], [`resource`]).
//! - It records into the command memory of the game ([`cmdbuf`], [`record`], [`mem`]), keeps a summary of each recording
//!   ([`decode`], [`cache`]), holds a whole frame ([`frame`]) and infers the render passes of Metal from it
//!   ([`analyse`], [`passes`]). [`front::FrontEnd`] ties these parts together.
//!
//! [`backend::Backend`] is what the front end needs from the GPU backend.
//!
//! [`entries`] lists the NVN functions of the engine and the ones that this crate handles.
//! Refer to `docs/nxkit-nvn.md` for the design and its limits.

pub(crate) mod actions;
pub mod analyse;
pub mod backend;
pub mod cmdbuf;
pub mod decode;
pub mod descriptors;
pub mod diag;
pub mod entries;
pub mod error;
pub mod format;
pub mod frame;
pub mod front;
pub mod layout;
pub mod marks;
pub mod mem;
pub mod passes;
pub mod passmerge;
pub mod pools;
pub mod prof;
pub mod record;
pub mod resource;
pub mod screen;
pub mod settings;
pub mod space;
pub mod swizzle;
pub mod tic;

// Parts of a larger module, kept at their old paths (W24): a frame carries its GPU counters and render conditions,
// the decoder its recording cache, a record its extensions, the front end the texture lifetimes.
pub use decode::cache;
pub use frame::{counters, rendercond};
pub use front::texlife;
pub use record::ext;
#[cfg(test)]
mod analyse_tests;
#[cfg(test)]
mod front_state_tests;
#[cfg(test)]
mod front_tests;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod texlife_tests;
