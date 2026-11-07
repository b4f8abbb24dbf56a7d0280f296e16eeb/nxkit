//! The guest ABI helpers that every NVN group uses.
//!
//! An NVN object (`NVNdevice`, `NVNqueue`, `NVNmemoryPool`, `NVNtextureBuilder` and so on) is memory that the game
//! owns, of the size that real NVN defines. The game does not look inside, but it allocates the bytes and passes the
//! address to every call. The host keeps what it needs in these bytes ([`Obj`]) or, where the object is too small or
//! the data is a Rust type, in a [`Registry`] keyed by the guest address of the object.
//!
//! Argument conventions of a guest-facing function (they come from the rules in `lib.rs`):
//! - An argument of fewer than 32 bits is a `u32`. A boolean (`NVNboolean`) is the low 8 bits of it ([`arg_bool`]).
//! - A pointer is a `u64`. A guest address is a host address (R18).
//! - An enum is an `i32`, a `size_t` is a `u64`, a `ptrdiff_t` is an `i64`.
//! - A function returns a boolean as `u32` 0 or 1 ([`ret_bool`]).
//!
//! Errors. A call never goes on after it failed (R15). The body of a function runs inside [`guard`]. An error ends the
//! game with the name of the function ([`fatal`]). A function that is not implemented does the same through the stub
//! table of `mod.rs`.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use nxkit_abi::guest::{self, Fault};
use nxkit_nvn::backend::BackendError;
use nxkit_nvn::error::NvnError;

/// The sizes in bytes that real NVN reserves for its objects (`nvn.h` of nxnative). The host keeps its state in the
/// first bytes and never reads or writes past the size.
pub(crate) mod size {
    /// `NVNdevice`
    pub(crate) const DEVICE: u64 = 0x3000;
    /// `NVNdeviceBuilder`
    pub(crate) const DEVICE_BUILDER: u64 = 0x40;
    /// `NVNqueue`
    pub(crate) const QUEUE: u64 = 0x2000;
    /// `NVNqueueBuilder`
    pub(crate) const QUEUE_BUILDER: u64 = 0x40;
    /// `NVNwindow`
    pub(crate) const WINDOW: u64 = 0x180;
    /// `NVNwindowBuilder`
    pub(crate) const WINDOW_BUILDER: u64 = 0x40;
    /// `NVNsync`
    pub(crate) const SYNC: u64 = 0x40;
    /// `NVNprogram`
    pub(crate) const PROGRAM: u64 = 0xC0;
    /// `NVNmemoryPool`
    pub(crate) const POOL: u64 = 0x100;
    /// `NVNmemoryPoolBuilder`
    pub(crate) const POOL_BUILDER: u64 = 0x40;
    /// `NVNbuffer`
    pub(crate) const BUFFER: u64 = 0x30;
    /// `NVNbufferBuilder`
    pub(crate) const BUFFER_BUILDER: u64 = 0x40;
    /// `NVNevent`
    pub(crate) const EVENT: u64 = 0x40;
    /// `NVNeventBuilder`
    pub(crate) const EVENT_BUILDER: u64 = 0x20;
    /// `NVNtexture`
    pub(crate) const TEXTURE: u64 = 0xC0;
    /// `NVNtextureBuilder`
    pub(crate) const TEXTURE_BUILDER: u64 = 0x80;
    /// `NVNtexturePool` and `NVNsamplerPool`
    pub(crate) const DESCRIPTOR_POOL: u64 = 0x20;
    /// `NVNsampler`
    pub(crate) const SAMPLER: u64 = 0x30;
    /// `NVNsamplerBuilder`
    pub(crate) const SAMPLER_BUILDER: u64 = 0x60;
    /// `NVNcommandBuffer`
    pub(crate) const COMMAND_BUFFER: u64 = 0xA0;
}

/// The magic of an initialised object: four ASCII bytes, first byte lowest.
pub(crate) const fn magic(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | (b as u32) << 8 | (c as u32) << 16 | (d as u32) << 24
}

/// Why a call failed.
#[derive(Debug)]
pub(crate) enum AbiError {
    /// The game gave an address that is outside guest memory.
    Fault(Fault),
    /// The front end refused.
    Nvn(NvnError),
    /// The backend failed.
    Backend(BackendError),
    /// Anything else, with the reason as text.
    Other(String),
}

impl fmt::Display for AbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AbiError::Fault(e) => write!(f, "{e}"),
            AbiError::Nvn(e) => write!(f, "{e}"),
            AbiError::Backend(e) => write!(f, "{e}"),
            AbiError::Other(s) => f.write_str(s),
        }
    }
}

impl From<Fault> for AbiError {
    fn from(e: Fault) -> Self {
        AbiError::Fault(e)
    }
}

impl From<NvnError> for AbiError {
    fn from(e: NvnError) -> Self {
        AbiError::Nvn(e)
    }
}

impl From<BackendError> for AbiError {
    fn from(e: BackendError) -> Self {
        AbiError::Backend(e)
    }
}

/// The result of the body of a guest-facing function.
pub(crate) type AbiResult<T> = Result<T, AbiError>;

/// An error with a text.
pub(crate) fn other(why: impl Into<String>) -> AbiError {
    AbiError::Other(why.into())
}

/// Ends the game and says which NVN function did it. The process aborts, so a debugger or a crash report shows the stop.
pub(crate) fn fatal(name: &str, why: impl fmt::Display) -> ! {
    eprintln!("[RED] NVN {name}: {why}");
    std::process::abort()
}

/// Runs the body of a guest-facing function. An error ends the game with the name ([`fatal`]).
pub(crate) fn guard<T>(name: &str, body: impl FnOnce() -> AbiResult<T>) -> T {
    match body() {
        Ok(v) => v,
        Err(e) => fatal(name, e),
    }
}

/// Logs a line the first time `key` is seen and never again. `key` names the call site, as `"nvnWindowSetNumActiveTextures"`.
pub(crate) fn log_once(key: &'static str, line: impl fmt::Display) {
    static SEEN: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let first = seen
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key);
    if first {
        eprintln!("[nvn] {line}");
    }
}

/// A boolean argument. Only the low 8 bits count, because Switch code does not extend a `uint8_t`.
pub(crate) fn arg_bool(v: u32) -> bool {
    v & 0xFF != 0
}

/// A boolean result.
pub(crate) fn ret_bool(b: bool) -> u32 {
    u32::from(b)
}

/// An object in guest memory, as its address. Every access checks the guest range. An access outside it is an
/// [`AbiError::Fault`], which a null pointer gives too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Obj(pub(crate) u64);

impl Obj {
    /// The guest address.
    pub(crate) fn addr(self) -> u64 {
        self.0
    }

    /// True for the null pointer.
    pub(crate) fn is_null(self) -> bool {
        self.0 == 0
    }

    /// The field at byte offset `off`, as an object. Use it for a nested struct.
    pub(crate) fn at(self, off: u64) -> Obj {
        Obj(self.0.wrapping_add(off))
    }

    /// Reads a `u32` at `off`.
    pub(crate) fn u32(self, off: u64) -> AbiResult<u32> {
        Ok(guest::read_u32(self.0.wrapping_add(off))?)
    }

    /// Reads an `i32` at `off`.
    pub(crate) fn i32(self, off: u64) -> AbiResult<i32> {
        Ok(guest::read_i32(self.0.wrapping_add(off))?)
    }

    /// Reads a `u64` at `off`. A pointer field is read with it.
    pub(crate) fn u64(self, off: u64) -> AbiResult<u64> {
        Ok(guest::read_u64(self.0.wrapping_add(off))?)
    }

    /// Reads an `i64` at `off`.
    pub(crate) fn i64(self, off: u64) -> AbiResult<i64> {
        Ok(guest::read_i64(self.0.wrapping_add(off))?)
    }

    /// Reads an `f32` at `off`.
    pub(crate) fn f32(self, off: u64) -> AbiResult<f32> {
        Ok(guest::read_f32(self.0.wrapping_add(off))?)
    }

    /// Writes a `u32` at `off`.
    pub(crate) fn set_u32(self, off: u64, v: u32) -> AbiResult<()> {
        Ok(guest::write_u32(self.0.wrapping_add(off), v)?)
    }

    /// Writes an `i32` at `off`.
    pub(crate) fn set_i32(self, off: u64, v: i32) -> AbiResult<()> {
        Ok(guest::write_i32(self.0.wrapping_add(off), v)?)
    }

    /// Writes a `u64` at `off`.
    pub(crate) fn set_u64(self, off: u64, v: u64) -> AbiResult<()> {
        Ok(guest::write_u64(self.0.wrapping_add(off), v)?)
    }

    /// Writes an `i64` at `off`.
    pub(crate) fn set_i64(self, off: u64, v: i64) -> AbiResult<()> {
        Ok(guest::write_i64(self.0.wrapping_add(off), v)?)
    }

    /// Writes an `f32` at `off`.
    pub(crate) fn set_f32(self, off: u64, v: f32) -> AbiResult<()> {
        Ok(guest::write_f32(self.0.wrapping_add(off), v)?)
    }

    /// Reads `out.len()` bytes at `off`.
    pub(crate) fn read(self, off: u64, out: &mut [u8]) -> AbiResult<()> {
        Ok(guest::read(self.0.wrapping_add(off), out)?)
    }

    /// Writes `data` at `off`.
    pub(crate) fn write(self, off: u64, data: &[u8]) -> AbiResult<()> {
        Ok(guest::write(self.0.wrapping_add(off), data)?)
    }

    /// Sets the first `len` bytes to zero. `Initialize` calls it with the object size.
    pub(crate) fn zero(self, len: u64) -> AbiResult<()> {
        Ok(guest::fill(self.0, len, 0)?)
    }
}

/// Maps a guest address to a host-side Rust value. The map is split into 64 shards, each behind its own lock, so
/// two threads that work on different objects rarely meet. A lookup holds one shard lock for the time of the closure.
/// The closure must not call back into the same registry.
#[derive(Debug)]
pub(crate) struct Registry<T> {
    shards: Vec<Mutex<HashMap<u64, T>>>,
}

/// The number of shards of a [`Registry`]. A power of two.
const SHARDS: usize = 64;

impl<T> Registry<T> {
    /// An empty registry.
    pub(crate) fn new() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(),
        }
    }

    fn shard(&self, key: u64) -> MutexGuard<'_, HashMap<u64, T>> {
        // Objects are 16-byte aligned or more, so the low bits carry nothing. A multiplicative hash spreads the rest.
        let h = (key >> 4).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58;
        self.shards[h as usize % SHARDS]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Stores `value` for `key`. Gives the value that it replaced.
    pub(crate) fn insert(&self, key: u64, value: T) -> Option<T> {
        self.shard(key).insert(key, value)
    }

    /// Removes the value of `key` and gives it.
    pub(crate) fn remove(&self, key: u64) -> Option<T> {
        self.shard(key).remove(&key)
    }

    /// Runs `f` on the value of `key`. None when the key is not there.
    pub(crate) fn with<R>(&self, key: u64, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.shard(key).get_mut(&key).map(f)
    }

    /// A copy of the value of `key`.
    pub(crate) fn get(&self, key: u64) -> Option<T>
    where
        T: Clone,
    {
        self.shard(key).get(&key).cloned()
    }

    /// The number of entries. It locks every shard one after the other.
    pub(crate) fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.lock().unwrap_or_else(PoisonError::into_inner).len())
            .sum()
    }
}

/// Builds the entries of a group: `table!["nvnFoo" => nvn_foo, ...]`. Each function is `extern "C"`.
macro_rules! table {
    ($($name:literal => $f:expr),* $(,)?) => {
        vec![$(($name, $f as usize)),*]
    };
}
pub(crate) use table;

/// Guest memory for tests. The guest range is one process-wide value, so every test in the crate takes its memory from
/// the one arena instead of naming a range of its own.
#[cfg(test)]
pub(crate) mod testing {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    use nxkit_abi::guest;

    /// The size of the arena.
    const ARENA: u64 = 16 << 20;

    static BASE: OnceLock<u64> = OnceLock::new();
    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// The address of `len` zeroed bytes in the guest range, aligned to 0x1000. The memory is never freed.
    pub(crate) fn alloc(len: u64) -> u64 {
        let base = *BASE.get_or_init(|| {
            let mem: &'static mut [u8] =
                Box::leak(vec![0u8; (ARENA + 0x1000) as usize].into_boxed_slice());
            let base = (mem.as_mut_ptr() as u64 + 0xFFF) & !0xFFF;
            guest::set_range(base, ARENA);
            base
        });
        let len = (len + 0xFFF) & !0xFFF;
        let at = NEXT.fetch_add(len, Ordering::Relaxed);
        assert!(at + len <= ARENA, "the test arena is full");
        base + at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_magic_is_four_bytes_first_byte_lowest() {
        assert_eq!(magic(b'D', b'E', b'V', 0), 0x0056_4544);
    }

    #[test]
    fn only_the_low_byte_of_a_boolean_counts() {
        assert!(!arg_bool(0xABCD_EF00));
        assert!(arg_bool(0xABCD_EF01));
        assert_eq!(ret_bool(true), 1);
    }

    #[test]
    fn the_registry_keeps_one_value_per_key() {
        let r = Registry::new();
        assert!(r.insert(0x1000, 5u32).is_none());
        assert_eq!(r.insert(0x1000, 6), Some(5));
        assert_eq!(r.get(0x1000), Some(6));
        assert_eq!(r.with(0x1000, |v| std::mem::replace(v, 9)), Some(6));
        assert_eq!(r.remove(0x1000), Some(9));
        assert_eq!(r.get(0x1000), None);
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn the_registry_spreads_aligned_keys_over_shards() {
        let r = Registry::new();
        for i in 0..256u64 {
            r.insert(0x7000_0000 + i * 0x100, i);
        }
        let used = r
            .shards
            .iter()
            .filter(|s| s.lock().map(|m| !m.is_empty()).unwrap_or(false))
            .count();
        assert!(used > SHARDS / 2, "{used} shards used");
        assert_eq!(r.len(), 256);
    }

    #[test]
    fn an_object_at_the_null_address_faults() {
        assert!(Obj(0).u32(0).is_err());
        assert!(Obj(0).is_null());
        assert_eq!(Obj(0x10).at(0x20), Obj(0x30));
    }
}
