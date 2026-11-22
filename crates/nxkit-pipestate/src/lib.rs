//! The state of a pipeline as plain data, shared by the host (`nxkit-metal`, `nxkit-gpu`) and the build
//! (`nxb-archive`, `nxb-build`).
//!
//! - [`format`] and [`state`]: pixel formats, blend factors and operations, topology classes and vertex layouts.
//!   `nxkit-metal` maps them to Metal. Nothing here touches Metal.
//! - [`nvn`]: the NVN values of a draw and how they map to that state. Every NVN to Metal decision of a pipeline is
//!   in this module.
//!
//! The values are part of the key scheme of the host's pipeline cache: the keys hash them, so the order of variants
//! and fields stays as it is.

pub mod format;
pub mod nvn;
pub mod state;

pub use format::{BcKind, Format, FormatClass, SampleType};
pub use state::{
    BlendFactor, BlendOp, BlendState, ColorTarget, StepFunction, TopologyClass, VertexAttribute,
    VertexDescriptor, VertexFormat, VertexLayout,
};
