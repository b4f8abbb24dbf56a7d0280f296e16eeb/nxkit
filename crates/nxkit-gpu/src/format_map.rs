//! The NVN to Metal format tables. They are `nxkit-pipestate`'s (`nvn`), shared with the build, so a pipeline that the
//! build resolves from a seed has the formats of the draw.

pub(crate) use nxkit_pipestate::nvn::{
    Mapped, is_integer, sample_swizzle, texture_format, vertex_format,
};
