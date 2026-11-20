//! The shader split (replaces `shader_split.py`): container readers, shader keys, pairing and the package record and
//! TSV writers. Pure: bytes in, bytes out, the walk over the RomFS images lives in the stage (`nxb-extract::split`).
//!
//! The container readers build on the shared primitives of `prim` (magics, a found shader, the checks, the byte search),
//! and the scan of `shaders` dispatches to them. A container that no reader handles is an [`SplitError::Unsupported`]
//! error naming the container, never a silent difference. `docs/pipeline-stages.md` lists the supported families.

pub mod aes256;
pub mod bahs;
pub mod bzip2;
pub mod capcom;
pub mod cra;
pub mod engines;
pub mod glslc;
pub mod inflate;
pub mod jobs;
pub mod lz4;
pub mod mgfx;
pub mod misc;
pub mod monolith;
pub mod narc;
pub mod nso;
pub mod oodle;
pub mod packs;
mod prim;
pub mod raw;
pub(crate) mod rd;
pub mod records;
pub mod retro;
pub mod scanfile;
pub mod shaders;
pub mod sized;
pub mod sjis;
pub mod snowdrop;
pub mod src;
pub mod sumo;
pub mod ue3;
pub mod unity;
pub mod xnb;
pub mod yaz0;
pub mod zlibx;
pub mod zstd;

use std::fmt;

/// Why a file or the split failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitError {
    /// A `red()` of the Python (the same text, without the `[RED] ` prefix).
    Red(String),
    /// Damaged compressed or container data.
    Format(String),
    /// A reader this port does not have yet.
    Unsupported(String),
}

impl fmt::Display for SplitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SplitError::Red(m) | SplitError::Format(m) => write!(f, "{m}"),
            SplitError::Unsupported(m) => write!(f, "not supported yet: {m}"),
        }
    }
}

impl std::error::Error for SplitError {}
