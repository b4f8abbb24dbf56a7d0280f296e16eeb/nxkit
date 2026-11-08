//! The Horizon result codes of the fs service and the mapping from a host error.
//!
//! The values are the ones `dm/host/hle_fs.c` returns: module 2, description 1 (path not found), 2 (path already
//! exists), 1002 (target not found) and the generic failure it uses for everything else.

use std::io;

/// Success.
pub(crate) const OK: u32 = 0;
/// A path that does not exist (module 2, description 1).
pub(crate) const PATH_NOT_FOUND: u32 = 0x202;
/// A path that exists already (module 2, description 2).
pub(crate) const PATH_EXISTS: u32 = 0x402;
/// Any other failure. A write to a read-only mount, a bad handle and a host error all give it.
pub(crate) const FAIL: u32 = 0x2A2;
/// A target that is not there, for example add-on content that the player does not own (module 2, description 1002).
pub(crate) const TARGET_NOT_FOUND: u32 = 0x7D402;

/// `Ok` value or a Horizon result code.
pub(crate) type FsResult<T> = Result<T, u32>;

const ENOENT: i32 = 2;
const EEXIST: i32 = 17;
const ENOTDIR: i32 = 20;

/// The result code of a host error, as `err_res` of nxnative gives it: a missing entry or a part of the path that is
/// not a folder is "path not found", an existing entry is "path exists", all else is a failure.
pub(crate) fn from_io(e: &io::Error) -> u32 {
    match e.raw_os_error() {
        Some(ENOENT | ENOTDIR) => PATH_NOT_FOUND,
        Some(EEXIST) => PATH_EXISTS,
        _ if e.kind() == io::ErrorKind::NotFound => PATH_NOT_FOUND,
        _ if e.kind() == io::ErrorKind::AlreadyExists => PATH_EXISTS,
        _ => FAIL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_errors_map_as_in_nxnative() {
        assert_eq!(
            from_io(&io::Error::from_raw_os_error(ENOENT)),
            PATH_NOT_FOUND
        );
        assert_eq!(
            from_io(&io::Error::from_raw_os_error(ENOTDIR)),
            PATH_NOT_FOUND
        );
        assert_eq!(from_io(&io::Error::from_raw_os_error(EEXIST)), PATH_EXISTS);
        assert_eq!(from_io(&io::Error::from_raw_os_error(28)), FAIL);
        assert_eq!(from_io(&io::Error::from(io::ErrorKind::InvalidInput)), FAIL);
    }
}
