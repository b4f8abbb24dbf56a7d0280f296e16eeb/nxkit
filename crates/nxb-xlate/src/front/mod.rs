//! The front end pipeline: from the machine code of a job to the structured program (W28).
//!
//! `core` and `geometry` run one translation, `pipeline` runs the passes, `emit_shader` emits the IR of a shader and
//! `msl_entry` is the entry of the MSL back end with the retry loop of `crate::driver`. The state of a translation is
//! `crate::translation`.

#[cfg(test)]
mod calls_test;
pub mod core;
pub mod emit_shader;
#[cfg(test)]
pub mod emit_test_util;
#[cfg(test)]
mod emit_tests;
pub mod geometry;
#[cfg(test)]
mod geometry_tests;
pub mod msl_entry;
pub mod pipeline;
