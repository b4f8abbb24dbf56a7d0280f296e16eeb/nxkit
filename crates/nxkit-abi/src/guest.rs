//! Guest memory as a service sees it.
//!
//! The game's code runs at fixed addresses in the host process (R18). A guest address is a host address. The host
//! reserves one range for all guest memory and names it once with [`set_range`] before the game starts. Every access
//! checks the range and copies with plain loads and stores. A guest thread may write the same bytes at the same time,
//! as on the Switch, so a reader gets whatever the memory holds at that moment.
//!
//! Tests point the range at a host buffer of their own.
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};

static BASE: AtomicU64 = AtomicU64::new(0);
static END: AtomicU64 = AtomicU64::new(0);

/// An access outside the guest range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    /// The first address of the access.
    pub addr: u64,
    /// The length of the access.
    pub len: u64,
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "guest access of {} bytes at {:#x} is outside guest memory", self.len, self.addr)
    }
}

impl std::error::Error for Fault {}

/// Names the guest range `[base, base + len)`. The host calls it once after it reserved guest memory. A later call
/// replaces the range, which only tests do.
pub fn set_range(base: u64, len: u64) {
    END.store(base.saturating_add(len), Ordering::Release);
    BASE.store(base, Ordering::Release);
}

/// The guest range as `(base, len)`. `(0, 0)` before [`set_range`].
pub fn range() -> (u64, u64) {
    let base = BASE.load(Ordering::Acquire);
    (base, END.load(Ordering::Acquire).saturating_sub(base))
}

/// Checks that `[addr, addr + len)` lies in the guest range. A zero length is always valid.
pub fn check(addr: u64, len: u64) -> Result<(), Fault> {
    if len == 0 {
        return Ok(());
    }
    let base = BASE.load(Ordering::Acquire);
    let end = END.load(Ordering::Acquire);
    match addr.checked_add(len) {
        Some(last) if addr >= base && last <= end => Ok(()),
        _ => Err(Fault { addr, len }),
    }
}

/// Copies guest bytes at `addr` into `out`.
pub fn read(addr: u64, out: &mut [u8]) -> Result<(), Fault> {
    check(addr, out.len() as u64)?;
    if out.is_empty() {
        return Ok(());
    }
    // SAFETY: the range check above proves that the bytes lie in the reserved guest range, which the host keeps
    // mapped for the life of the process. `out` is a separate host buffer, so the regions do not overlap.
    unsafe { std::ptr::copy_nonoverlapping(addr as *const u8, out.as_mut_ptr(), out.len()) };
    Ok(())
}

/// Copies `data` into guest memory at `addr`.
pub fn write(addr: u64, data: &[u8]) -> Result<(), Fault> {
    check(addr, data.len() as u64)?;
    if data.is_empty() {
        return Ok(());
    }
    // SAFETY: as in `read`. The guest range is mapped writable.
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), addr as *mut u8, data.len()) };
    Ok(())
}

/// Fills `len` guest bytes at `addr` with `byte`.
pub fn fill(addr: u64, len: u64, byte: u8) -> Result<(), Fault> {
    check(addr, len)?;
    if len == 0 {
        return Ok(());
    }
    // SAFETY: as in `write`.
    unsafe { std::ptr::write_bytes(addr as *mut u8, byte, len as usize) };
    Ok(())
}

/// Gives `f` the guest bytes at `addr` without a copy. The game may change them during the call.
pub fn with_slice<R>(addr: u64, len: u64, f: impl FnOnce(&[u8]) -> R) -> Result<R, Fault> {
    check(addr, len)?;
    if len == 0 {
        return Ok(f(&[]));
    }
    // SAFETY: the bytes lie in the mapped guest range. The slice does not outlive the call.
    let s = unsafe { std::slice::from_raw_parts(addr as *const u8, len as usize) };
    Ok(f(s))
}

/// Gives `f` the guest bytes at `addr` to write without a copy, for example to decode a file straight into a buffer
/// of the game.
pub fn with_slice_mut<R>(addr: u64, len: u64, f: impl FnOnce(&mut [u8]) -> R) -> Result<R, Fault> {
    check(addr, len)?;
    if len == 0 {
        return Ok(f(&mut []));
    }
    // SAFETY: the bytes lie in the mapped writable guest range. The slice does not outlive the call. The game owns
    // the buffer it passed and does not touch it during the call, as on the Switch.
    let s = unsafe { std::slice::from_raw_parts_mut(addr as *mut u8, len as usize) };
    Ok(f(s))
}

macro_rules! scalar {
    ($read:ident, $write:ident, $t:ty) => {
        #[doc = concat!("Reads a little-endian `", stringify!($t), "` at `addr`.")]
        pub fn $read(addr: u64) -> Result<$t, Fault> {
            let mut b = [0u8; std::mem::size_of::<$t>()];
            read(addr, &mut b)?;
            Ok(<$t>::from_le_bytes(b))
        }

        #[doc = concat!("Writes a little-endian `", stringify!($t), "` at `addr`.")]
        pub fn $write(addr: u64, v: $t) -> Result<(), Fault> {
            write(addr, &v.to_le_bytes())
        }
    };
}

scalar!(read_u8, write_u8, u8);
scalar!(read_u16, write_u16, u16);
scalar!(read_u32, write_u32, u32);
scalar!(read_u64, write_u64, u64);
scalar!(read_i32, write_i32, i32);
scalar!(read_i64, write_i64, i64);
scalar!(read_f32, write_f32, f32);

/// Reads a NUL-terminated string of at most `max` bytes at `addr`, without the NUL.
pub fn read_cstr(addr: u64, max: usize) -> Result<Vec<u8>, Fault> {
    let mut out = Vec::new();
    for i in 0..max as u64 {
        let b = read_u8(addr + i)?;
        if b == 0 {
            break;
        }
        out.push(b);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accesses_stay_inside_the_range() {
        let mut buf = vec![0u8; 64];
        let base = buf.as_mut_ptr() as u64;
        set_range(base, 64);
        assert!(write_u32(base + 4, 0xdead_beef).is_ok());
        assert_eq!(read_u32(base + 4), Ok(0xdead_beef));
        assert!(read_u64(base + 60).is_err());
        assert!(read_u8(base - 1).is_err());
        assert!(write(base, b"abc\0").is_ok());
        assert_eq!(read_cstr(base, 16), Ok(b"abc".to_vec()));
        set_range(0, 0);
        drop(buf);
    }
}
