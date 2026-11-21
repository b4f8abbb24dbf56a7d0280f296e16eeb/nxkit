//! The shader domain: the translator jobs, the chain version, the guard tables, the header and the target conventions
//! of the shader step, and the cut of a pixel shader's side library (design-v2 sb-domain).
//!
//! No I/O. Everything here is output-relevant and hashed into the chain version V. The MSL itself comes from `nxb-xlate`.

pub mod chain;

pub mod guards;
pub mod job;
pub(crate) mod mesh;

pub mod pch;
pub mod side;
pub mod target;
