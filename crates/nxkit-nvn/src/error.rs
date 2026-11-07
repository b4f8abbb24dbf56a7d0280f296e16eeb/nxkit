//! The errors of the NVN front end. Game data is not trusted, so each failure is a value and none is a panic.

use std::fmt;

/// A failure of the NVN front end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NvnError {
    /// An address range lies outside the memory that the backend mapped.
    OutOfRange {
        /// The first address of the range.
        addr: u64,
        /// The length of the range in bytes.
        len: u64,
    },
    /// A GPU address does not fit in 40 bits.
    TooWide(u64),
    /// The format number is not an NVN format that the front end knows.
    UnknownFormat(i32),
    /// The texture description is not valid. The text names the field.
    BadTexture(&'static str),
    /// The texture uses a feature that the front end does not handle yet.
    Unsupported(&'static str),
    /// A pool range is empty, too large or overlaps a pool that exists.
    BadPool(&'static str),
    /// A descriptor id that no pool covers, or a slot that no texture or sampler filled.
    UnknownDescriptor(u32),
    /// A texture id that the resource table does not hold, or that was finalised.
    UnknownTexture(u32),
    /// A buffer that is too small for the data it must hold.
    BufferTooSmall {
        /// The bytes that are needed.
        need: u64,
        /// The bytes that the caller gave.
        have: u64,
    },
    /// A value that does not fit the descriptor field that holds it.
    FieldRange(&'static str),
    /// A command that needs a render target slot that the game did not set.
    NoTarget(u8),
    /// The command memory has no room for the next record. The game adds memory and records again.
    OutOfCommandMemory {
        /// The bytes that the record needs, including the room for a jump.
        need: u32,
    },
    /// A command record in guest memory is not valid. The text says why.
    BadRecord(&'static str),
}

impl fmt::Display for NvnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange { addr, len } => {
                write!(
                    f,
                    "range {addr:#x}+{len:#x} is outside the mapped guest memory"
                )
            }
            Self::TooWide(a) => write!(f, "GPU address {a:#x} does not fit in 40 bits"),
            Self::UnknownFormat(n) => write!(f, "unknown NVN format {n:#x}"),
            Self::BadTexture(why) => write!(f, "invalid texture: {why}"),
            Self::Unsupported(what) => write!(f, "not supported yet: {what}"),
            Self::BadPool(why) => write!(f, "invalid descriptor pool: {why}"),
            Self::UnknownDescriptor(id) => write!(f, "descriptor {id} is not registered"),
            Self::UnknownTexture(id) => write!(f, "texture {id} is not in the resource table"),
            Self::BufferTooSmall { need, have } => {
                write!(f, "buffer of {have} bytes is too small, {need} are needed")
            }
            Self::FieldRange(what) => write!(f, "value does not fit the descriptor field {what}"),
            Self::NoTarget(slot) => write!(f, "render target slot {slot} is not set"),
            Self::OutOfCommandMemory { need } => {
                write!(f, "command memory is full, {need} more bytes are needed")
            }
            Self::BadRecord(why) => write!(f, "invalid command record: {why}"),
        }
    }
}

impl std::error::Error for NvnError {}

/// The result type of the crate.
pub type Result<T> = std::result::Result<T, NvnError>;
