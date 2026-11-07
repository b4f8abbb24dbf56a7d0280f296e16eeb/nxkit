//! Horizon result codes. Module 1 is the kernel, and the code is `module | description << 9`. The descriptions
//! below match `svc_results.hpp` of Atmosphere (checked on 2026-11-07).

use std::fmt;

/// A Horizon result code as a guest sees it in `w0`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct KResult(pub u32);

impl KResult {
    /// The call worked.
    pub const SUCCESS: KResult = KResult(0);
    /// A pointer or table index is out of the allowed range (kernel description 103).
    pub const OUT_OF_RESOURCE: KResult = KResult(0xCE01);
    /// The process has no free handle (105).
    pub const OUT_OF_HANDLES: KResult = KResult(0xD201);
    /// The address is not aligned or not allowed (102).
    pub const INVALID_ADDRESS: KResult = KResult(0xCC01);
    /// The kernel could not read or write the guest address (106).
    pub const INVALID_CURRENT_MEMORY: KResult = KResult(0xD401);
    /// The priority is not 0 to 63 (112).
    pub const INVALID_PRIORITY: KResult = KResult(0xE001);
    /// The core is not one of the process's cores (113).
    pub const INVALID_CORE_ID: KResult = KResult(0xE201);
    /// The handle is not valid or has the wrong type (114).
    pub const INVALID_HANDLE: KResult = KResult(0xE401);
    /// The combination of arguments does not work together (116).
    pub const INVALID_COMBINATION: KResult = KResult(0xE801);
    /// The wait ran out of time (117).
    pub const TIMED_OUT: KResult = KResult(0xEA01);
    /// `CancelSynchronization` ended the wait (118).
    pub const CANCELLED: KResult = KResult(0xEC01);
    /// The count is too large (119).
    pub const OUT_OF_RANGE: KResult = KResult(0xEE01);
    /// The enumeration value is unknown (120).
    pub const INVALID_ENUM_VALUE: KResult = KResult(0xF001);
    /// The object is in the wrong state for the call (125).
    pub const INVALID_STATE: KResult = KResult(0xFA01);
    /// The call is not implemented (33).
    pub const NOT_IMPLEMENTED: KResult = KResult(0x4201);
    /// A size is zero, not aligned or too large (101).
    pub const INVALID_SIZE: KResult = KResult(0xCA01);
    /// The kernel has no memory for the request (104).
    pub const OUT_OF_MEMORY: KResult = KResult(0xD001);
    /// The address range is not in the region the call needs (110).
    pub const INVALID_MEMORY_REGION: KResult = KResult(0xDC01);
    /// A pointer is null or not aligned (115).
    pub const INVALID_POINTER: KResult = KResult(0xE601);
    /// The name is not known (121).
    pub const NOT_FOUND: KResult = KResult(0xF201);
    /// The session is closed (123).
    pub const SESSION_CLOSED: KResult = KResult(0xF601);
    /// The call is not supported (127).
    pub const NOT_SUPPORTED: KResult = KResult(0xFE01);
    /// The permission asked for is not allowed for the call (108).
    pub const INVALID_NEW_MEMORY_PERMISSION: KResult = KResult(0xD801);
    /// The process is at the limit of a resource (132).
    pub const LIMIT_REACHED: KResult = KResult(0x10801);

    /// The raw code.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Whether the code is zero.
    pub const fn is_success(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Debug for KResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KResult({:#x})", self.0)
    }
}

/// The result of a service call.
pub type KResultOr<T> = Result<T, KResult>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_module_one_with_the_description_shifted() {
        for (r, desc) in [
            (KResult::TIMED_OUT, 117),
            (KResult::CANCELLED, 118),
            (KResult::INVALID_HANDLE, 114),
            (KResult::INVALID_STATE, 125),
            (KResult::INVALID_ADDRESS, 102),
            (KResult::INVALID_CURRENT_MEMORY, 106),
            (KResult::NOT_IMPLEMENTED, 33),
            (KResult::INVALID_SIZE, 101),
            (KResult::OUT_OF_MEMORY, 104),
            (KResult::INVALID_MEMORY_REGION, 110),
            (KResult::INVALID_POINTER, 115),
            (KResult::NOT_FOUND, 121),
            (KResult::SESSION_CLOSED, 123),
            (KResult::NOT_SUPPORTED, 127),
            (KResult::LIMIT_REACHED, 132),
            (KResult::INVALID_NEW_MEMORY_PERMISSION, 108),
        ] {
            assert_eq!(r.raw(), 1 | desc << 9);
        }
    }
}
