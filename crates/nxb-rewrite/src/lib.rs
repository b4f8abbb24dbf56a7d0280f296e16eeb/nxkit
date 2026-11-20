//! Executable module parsing (NSO, NRO) and the AArch64 rewriter with its rules and TSV outputs (replaces `patch_nso.py`).
//!
//! Oracle: the reference script on real modules, byte-identical output (`tests/oracle.rs`, environment variables
//! `NXB_REWRITE_*`).

mod analyse;
mod branch;
mod census;
mod cuts;
mod dynsym;
mod engine;
mod error;
mod finders;
mod finders2;
mod frame;
/// The `nn::jit` image (port of `jit_capture.py`).
pub mod jit;
mod jumps;
mod jumps2;
mod jumps3;
#[cfg(test)]
mod jumps_tests;
mod locks;
mod matchfn;
#[cfg(test)]
mod matchfn_tests;
mod matchrule;
/// NSO and NRO parsing.
pub mod nso;
mod obs;
mod options;
mod pat;
/// The registered patches of the engine.
pub mod patches;
mod region;
mod rewrite;
mod ring;
#[cfg(test)]
mod ring_tests;
/// The registered patches as `rules/*.toml`.
pub mod rules;
mod rw;
mod scan;
mod sites;
mod snowdrop;
mod spin;
mod spin2;
mod spinreg;
mod tailcall;
mod windows;
#[cfg(test)]
mod windows_tests;

pub use census::census;
pub use error::RewriteError;
pub use rewrite::{Options, Output, rewrite};
