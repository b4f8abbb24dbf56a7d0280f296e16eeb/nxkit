//! The NVN front end of the nxkit host.

pub(crate) mod actions;
pub mod analyse;
pub mod descriptors;
pub mod error;
pub mod format;
pub mod frame;
pub mod layout;
pub mod passes;
pub mod recorder;
pub mod resource;
pub mod space;
pub mod swizzle;
pub mod tic;

#[cfg(test)]
mod analyse_tests;
