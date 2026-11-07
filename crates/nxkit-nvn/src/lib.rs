//! The NVN front end of the nxkit host.
//!
//! This crate is pure Rust and uses only `std`. It has no Metal code and no FFI. It does three jobs.
//!
//! - It maps guest memory to GPU addresses ([`space`]). The GPU address that NVN gives the game is the Metal address.
//! - It models textures: formats, storage layout, block-linear order, the descriptors that the game writes
//!   ([`mod@format`], [`layout`], [`swizzle`], [`tic`]), and the pools and the resource table ([`descriptors`], [`resource`]).
//! - It records a whole frame ([`recorder`], [`frame`]) and infers the render passes of Metal from it
//!   ([`analyse`], [`passes`]). [`front::FrontEnd`] ties these parts together.
//!
//! [`entries`] lists the NVN functions of nxnative and the ones that this crate handles.
//! Refer to `docs/nxkit-nvn.md` for the design and its limits.

pub(crate) mod actions;
pub mod analyse;
pub mod descriptors;
pub mod entries;
pub mod error;
pub mod format;
pub mod frame;
pub mod front;
pub mod layout;
pub mod passes;
pub mod recorder;
pub mod resource;
pub mod space;
pub mod swizzle;
pub mod tic;

#[cfg(test)]
mod analyse_tests;
