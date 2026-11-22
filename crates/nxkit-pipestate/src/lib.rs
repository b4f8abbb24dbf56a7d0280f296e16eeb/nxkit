//! The state of a pipeline as plain data, shared by the host (`nxkit-metal`, `nxkit-gpu`) and the build
//! (`nxb-archive`, `nxb-build`).
//!
//! - [`format`](mod@format) and [`state`]: pixel formats, blend factors and operations, topology classes and vertex layouts.
//!   `nxkit-metal` maps them to Metal. Nothing here touches Metal.
//! - [`nvn`]: the NVN values of a draw and how they map to that state. Every NVN to Metal decision of a pipeline is
//!   in this module.
//! - [`program`]: the facts of a translated shader that its pipelines depend on, read from its `<key>_b.json`.
//! - [`draw`]: the descriptor rules of a draw (entry functions, colour attachments).
//! - [`list`]: the text of a pipeline list, which the host records and reads, and the build archives and ships.
//! - [`seeds`]: the pipeline seeds of a title (the state of its pipelines in NVN terms) and their resolution into list
//!   entries.
//!
//! The values are part of the key scheme of the host's pipeline cache: the keys hash them, so the order of variants
//! and fields stays as it is.

pub mod draw;
pub mod format;
pub mod list;
pub mod nvn;
pub mod program;
pub mod seeds;
pub mod state;

pub use format::{BcKind, Format, FormatClass, SampleType};
pub use state::{
    BlendFactor, BlendOp, BlendState, ColorTarget, StepFunction, TopologyClass, VertexAttribute,
    VertexDescriptor, VertexFormat, VertexLayout,
};
