//! The results of the Vulkan front end: the success codes and the error codes of `VkResult`, with their real values.
//!
//! Game data is not trusted, so each failure is a value and none is a panic. A call that the front end does not
//! support returns the code that Vulkan names for it ([`VkError::FeatureNotPresent`], [`VkError::FormatNotSupported`]
//! and so on) and logs the reason once ([`Log::unsupported`]).

use std::collections::HashSet;
use std::fmt;
use std::sync::{Mutex, PoisonError};

use nxkit_abi::guest::Fault;

/// A success code of `VkResult`. A call that returns no code returns [`Status::Success`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    /// `VK_SUCCESS`.
    Success,
    /// `VK_NOT_READY`: a fence, a query or an event is not done yet.
    NotReady,
    /// `VK_TIMEOUT`: a wait ran out of time.
    Timeout,
    /// `VK_EVENT_SET`.
    EventSet,
    /// `VK_EVENT_RESET`.
    EventReset,
    /// `VK_INCOMPLETE`: an array that the game gave was too small for all entries.
    Incomplete,
    /// `VK_SUBOPTIMAL_KHR`.
    Suboptimal,
}

impl Status {
    /// The value of the code in `VkResult`.
    pub fn code(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::NotReady => 1,
            Self::Timeout => 2,
            Self::EventSet => 3,
            Self::EventReset => 4,
            Self::Incomplete => 5,
            Self::Suboptimal => 1_000_001_003,
        }
    }
}

/// An error code of `VkResult`, or a guest memory fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VkError {
    /// `VK_ERROR_OUT_OF_HOST_MEMORY`.
    OutOfHostMemory,
    /// `VK_ERROR_OUT_OF_DEVICE_MEMORY`.
    OutOfDeviceMemory,
    /// `VK_ERROR_INITIALIZATION_FAILED`.
    InitializationFailed,
    /// `VK_ERROR_DEVICE_LOST`.
    DeviceLost,
    /// `VK_ERROR_MEMORY_MAP_FAILED`.
    MemoryMapFailed,
    /// `VK_ERROR_LAYER_NOT_PRESENT`.
    LayerNotPresent,
    /// `VK_ERROR_EXTENSION_NOT_PRESENT`.
    ExtensionNotPresent,
    /// `VK_ERROR_FEATURE_NOT_PRESENT`.
    FeatureNotPresent,
    /// `VK_ERROR_INCOMPATIBLE_DRIVER`.
    IncompatibleDriver,
    /// `VK_ERROR_TOO_MANY_OBJECTS`.
    TooManyObjects,
    /// `VK_ERROR_FORMAT_NOT_SUPPORTED`.
    FormatNotSupported,
    /// `VK_ERROR_FRAGMENTED_POOL`.
    FragmentedPool,
    /// `VK_ERROR_UNKNOWN`.
    Unknown,
    /// `VK_ERROR_OUT_OF_POOL_MEMORY`.
    OutOfPoolMemory,
    /// `VK_ERROR_SURFACE_LOST_KHR`.
    SurfaceLost,
    /// `VK_ERROR_OUT_OF_DATE_KHR`.
    OutOfDate,
    /// A struct or an array of the game lies outside guest memory. It is no Vulkan code: the ABI layer ends the game
    /// with the name of the function (R15). [`VkError::code`] gives `VK_ERROR_UNKNOWN` for it.
    Fault {
        /// The first address of the access.
        addr: u64,
        /// The length of the access.
        len: u64,
    },
}

impl VkError {
    /// The value of the code in `VkResult`.
    pub fn code(self) -> i32 {
        match self {
            Self::OutOfHostMemory => -1,
            Self::OutOfDeviceMemory => -2,
            Self::InitializationFailed => -3,
            Self::DeviceLost => -4,
            Self::MemoryMapFailed => -5,
            Self::LayerNotPresent => -6,
            Self::ExtensionNotPresent => -7,
            Self::FeatureNotPresent => -8,
            Self::IncompatibleDriver => -9,
            Self::TooManyObjects => -10,
            Self::FormatNotSupported => -11,
            Self::FragmentedPool => -12,
            Self::Unknown | Self::Fault { .. } => -13,
            Self::OutOfPoolMemory => -1_000_069_000,
            Self::SurfaceLost => -1_000_000_000,
            Self::OutOfDate => -1_000_001_004,
        }
    }

    /// Whether the error is a guest memory fault, which the ABI layer treats as fatal.
    pub fn is_fault(self) -> bool {
        matches!(self, Self::Fault { .. })
    }
}

impl From<Fault> for VkError {
    fn from(f: Fault) -> Self {
        Self::Fault {
            addr: f.addr,
            len: f.len,
        }
    }
}

impl fmt::Display for VkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fault { addr, len } => {
                write!(
                    f,
                    "guest access of {len} bytes at {addr:#x} is outside guest memory"
                )
            }
            other => write!(f, "{other:?} ({})", other.code()),
        }
    }
}

impl std::error::Error for VkError {}

/// The result type of the crate.
pub type VkResult<T> = std::result::Result<T, VkError>;

/// Folds a result without a value into the `VkResult` value that the game gets.
pub fn code_of(r: VkResult<Status>) -> i32 {
    match r {
        Ok(s) => s.code(),
        Err(e) => e.code(),
    }
}

/// The log of the front end: each key is logged once. It lives in the front end's own object, not in a static, so
/// two front ends (tests) do not share it.
#[derive(Debug, Default)]
pub struct Log {
    seen: Mutex<HashSet<String>>,
}

impl Log {
    /// Logs a line the first time `key` is seen and never again. `key` names the call site.
    pub fn once(&self, key: &str, line: impl fmt::Display) {
        let first = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_string());
        if first {
            eprintln!("[vk] {line}");
        }
    }

    /// Logs once that a feature is missing and gives the error for it.
    pub fn unsupported(&self, key: &str, err: VkError, line: impl fmt::Display) -> VkError {
        self.once(key, format_args!("[RED] not supported: {line}"));
        err
    }

    /// Whether `key` was logged.
    pub fn seen(&self, key: &str) -> bool {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_the_vulkan_values() {
        assert_eq!(Status::Incomplete.code(), 5);
        assert_eq!(VkError::FeatureNotPresent.code(), -8);
        assert_eq!(VkError::OutOfPoolMemory.code(), -1_000_069_000);
        assert_eq!(VkError::from(Fault { addr: 1, len: 2 }).code(), -13);
        assert!(VkError::Fault { addr: 0, len: 1 }.is_fault());
        assert_eq!(code_of(Ok(Status::Timeout)), 2);
        assert_eq!(code_of(Err(VkError::OutOfDate)), -1_000_001_004);
    }

    #[test]
    fn the_log_says_each_key_once() {
        let log = Log::default();
        assert!(!log.seen("a"));
        assert_eq!(
            log.unsupported("a", VkError::FeatureNotPresent, "x"),
            VkError::FeatureNotPresent
        );
        assert!(log.seen("a"));
    }
}
