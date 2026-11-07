//! The error type of the Metal backend.

use std::fmt;

/// What went wrong in the Metal backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetalError {
    /// The system has no Metal device, or the device lacks Metal 4.
    NoDevice(&'static str),
    /// Metal refused to create an object. The text is Metal's own message where it gave one.
    Create {
        /// The object kind, for example `"command queue"`.
        what: &'static str,
        /// The reason.
        reason: String,
    },
    /// A shader library or pipeline did not compile or load.
    Compile(String),
    /// A host address, an offset or a length is outside the guest memory.
    OutOfRange {
        /// The first byte.
        addr: u64,
        /// The length in bytes.
        len: u64,
    },
    /// A base address or a length is not a multiple of the page size.
    Unaligned {
        /// The value.
        value: u64,
        /// The required multiple.
        align: u64,
    },
    /// The arguments of a call do not fit together.
    Invalid(&'static str),
    /// The GPU reported an error for a command buffer.
    Gpu(String),
}

impl fmt::Display for MetalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice(why) => write!(f, "no Metal device: {why}"),
            Self::Create { what, reason } => write!(f, "cannot create {what}: {reason}"),
            Self::Compile(why) => write!(f, "shader compile failed: {why}"),
            Self::OutOfRange { addr, len } => {
                write!(f, "range {addr:#x}+{len:#x} is outside the guest memory")
            }
            Self::Unaligned { value, align } => {
                write!(f, "{value:#x} is not a multiple of {align:#x}")
            }
            Self::Invalid(why) => write!(f, "invalid call: {why}"),
            Self::Gpu(why) => write!(f, "GPU error: {why}"),
        }
    }
}

impl std::error::Error for MetalError {}

/// The result type of this crate.
pub type Result<T> = std::result::Result<T, MetalError>;

pub(crate) fn create(what: &'static str, reason: impl fmt::Display) -> MetalError {
    MetalError::Create {
        what,
        reason: reason.to_string(),
    }
}
