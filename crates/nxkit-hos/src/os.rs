//! The SDK-level take-overs of `dm/host/hle_os.c`: entropy, the environment and `StandardAllocator::Allocate`.
//!
//! nxnative lets the SDK's own `nn::os` run on a replaced kernel, so it takes over only four SDK or libc functions
//! here. [`exports`] gives them as nxkit exports (R14). Every function is `extern "C"`, takes a pointer as a
//! guest address and reads or writes guest memory through `nxkit_abi::guest`.
//!
//! | Name | nxnative entry | What it does |
//! | --- | --- | --- |
//! | `getentropy` | `HLE` | Fills the buffer from the entropy stream. Returns 0, or -1 for a bad buffer. |
//! | `nn::crypto::GenerateCryptographicallyRandomBytes` | `HLE_FORCE` | The same stream. The csrng service is not opened. |
//! | `getenv` | `HLE_FORCE` | The game's `game_env` answers its names. The SDK's own `getenv` answers the rest. |
//! | `nn::mem::StandardAllocator::Allocate(size)` | `HLE_FORCE` | The SDK's own `Allocate`. With `alloc_big_align` a size of 64 KB or more is 64 KB aligned. |
//!
//! [`forced_names`] lists the entries that nxnative registers with `HLE_FORCE`: the host resolves every reference to
//! such a name to the export, even where an SDK module defines it. `getentropy` is a plain `HLE`, so it fills only a
//! reference that no module defines. `Export` has no flag for this, so the host takes it from `forced_names`.
//!
//! `getenv` and `Allocate` call the SDK's own function, found by [`crate::guest_sym`]. The lookup must answer with the
//! SDK's definition and not with the export. When the lookup or the symbol is missing the process aborts with a
//! message that names it, as nxnative does (R15).

use crate::entropy;
use crate::guestsym::require;
use crate::sys::GuestFn;
use nxkit_abi::export::Export;
use nxkit_abi::guest;
use std::ffi::CString;
use std::sync::OnceLock;

const GETENV: &str = "getenv";
const ALLOCATE: &str = "_ZN2nn3mem17StandardAllocator8AllocateEm";
const ALLOCATE_ALIGNED: &str = "_ZN2nn3mem17StandardAllocator8AllocateEmm";
const CSRNG: &str = "_ZN2nn6crypto36GenerateCryptographicallyRandomBytesEPvm";

/// Allocations of this size or more get [`BIG_ALIGN`] with `alloc_big_align`.
const BIG_SIZE: u64 = 0x1_0000;
const BIG_ALIGN: u64 = 0x1_0000;
/// The longest environment variable name that `getenv` reads.
const NAME_MAX: usize = 4096;

/// The facts of the game that these take-overs need. They come from the game's profile in `game.json`.
#[derive(Clone, Debug, Default)]
pub struct OsConfig {
    /// The environment that `getenv` answers: `(name, value)` pairs. Horizon gives a program no environment.
    pub game_env: Vec<(String, String)>,
    /// Aligns `StandardAllocator::Allocate` of 64 KB or more to 64 KB (profile key `alloc_big_align`).
    pub alloc_big_align: bool,
}

struct OsState {
    env: Vec<(Vec<u8>, CString)>,
    big_align: bool,
}

static STATE: OnceLock<OsState> = OnceLock::new();

/// Sets the facts of the game. Call it once before the game starts. A later call is an error and changes nothing.
pub fn init_os(config: OsConfig) -> Result<(), String> {
    let mut env = Vec::with_capacity(config.game_env.len());
    for (name, value) in config.game_env {
        if name.is_empty() || name.contains('=') {
            return Err(format!(
                "game_env: {name:?} is not an environment variable name"
            ));
        }
        let value = CString::new(value)
            .map_err(|_| format!("game_env: the value of {name} has a NUL byte"))?;
        env.push((name.into_bytes(), value));
    }
    if !env.is_empty() {
        eprintln!("nxkit-hos: game environment has {} variable(s)", env.len());
    }
    if config.alloc_big_align {
        eprintln!(
            "nxkit-hos: StandardAllocator::Allocate of 64 KB or more is 64 KB aligned (alloc_big_align)"
        );
    }
    STATE
        .set(OsState {
            env,
            big_align: config.alloc_big_align,
        })
        .map_err(|_| "init_os was called twice".to_string())
}

/// The SDK-level functions of the kernel layer, by Switch mangled name (R14).
pub fn exports() -> Vec<Export> {
    vec![
        Export::new("getentropy", getentropy as *const () as usize),
        Export::new(CSRNG, csrng_bytes as *const () as usize),
        Export::new(GETENV, getenv as *const () as usize),
        Export::new(ALLOCATE, standard_allocate as *const () as usize),
    ]
}

/// The exports that nxnative registers with `HLE_FORCE`: all references to the name go to the export.
pub fn forced_names() -> &'static [&'static str] {
    &[CSRNG, GETENV, ALLOCATE]
}

fn fill_guest(what: &str, ptr: u64, len: u64) -> bool {
    match guest::with_slice_mut(ptr, len, entropy::fill) {
        Ok(()) => true,
        Err(fault) => {
            eprintln!("nxkit-hos: {what}: {fault}");
            false
        }
    }
}

/// `int getentropy(void *buf, size_t len)`. A buffer outside guest memory gives -1.
extern "C" fn getentropy(buf: u64, len: u64) -> i32 {
    if fill_guest("getentropy", buf, len) {
        0
    } else {
        -1
    }
}

/// `void GenerateCryptographicallyRandomBytes(void *buf, size_t len)`. It has no result, so a bad buffer is logged.
extern "C" fn csrng_bytes(buf: u64, len: u64) {
    fill_guest("GenerateCryptographicallyRandomBytes", buf, len);
}

fn fatal(what: &str, why: &str) -> ! {
    eprintln!("nxkit-hos: {what}: {why}");
    std::process::abort()
}

fn sdk_getenv() -> GuestFn {
    static FN: OnceLock<Result<GuestFn, String>> = OnceLock::new();
    match FN.get_or_init(|| require(GETENV)) {
        Ok(f) => *f,
        Err(why) => fatal("getenv", why),
    }
}

/// `char *getenv(const char *name)`. A name of the game's environment gets its value, in a string that stays valid
/// for the whole run. Any other name goes to the SDK's own `getenv`. A NULL name or a name outside guest memory
/// gets NULL.
extern "C" fn getenv(name: u64) -> u64 {
    if name == 0 {
        return 0;
    }
    let key = match guest::read_cstr(name, NAME_MAX) {
        Ok(key) => key,
        Err(fault) => {
            eprintln!("nxkit-hos: getenv: {fault}");
            return 0;
        }
    };
    if !key.is_empty() {
        let own = STATE
            .get()
            .and_then(|s| s.env.iter().find(|(k, _)| *k == key));
        if let Some((_, value)) = own {
            return value.as_ptr() as usize as u64;
        }
    }
    sdk_getenv().call1(name)
}

struct Allocators {
    plain: GuestFn,
    aligned: GuestFn,
}

fn allocators() -> &'static Allocators {
    static FNS: OnceLock<Result<Allocators, String>> = OnceLock::new();
    let found = FNS.get_or_init(|| {
        Ok(Allocators {
            plain: require(ALLOCATE)?,
            aligned: require(ALLOCATE_ALIGNED)?,
        })
    });
    match found {
        Ok(a) => a,
        Err(why) => fatal("StandardAllocator::Allocate", why),
    }
}

/// `void *StandardAllocator::Allocate(size_t size)`. It goes straight to the SDK. With `alloc_big_align` a size of
/// 64 KB or more goes to `Allocate(size, 0x10000)`, which puts the storage of a GPU memory pool on a 64 KB
/// boundary (13 Sentinels and Unicorn Overlord, nxnative `hle_os.c`).
extern "C" fn standard_allocate(this: u64, size: u64) -> u64 {
    let fns = allocators();
    let big = STATE.get().is_some_and(|s| s.big_align);
    if big && size >= BIG_SIZE {
        fns.aligned.call3(this, size, BIG_ALIGN)
    } else {
        fns.plain.call2(this, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exports_are_the_four_take_overs() {
        let names: Vec<_> = exports().iter().map(|e| e.name).collect();
        assert_eq!(names, ["getentropy", CSRNG, GETENV, ALLOCATE]);
        assert!(exports().iter().all(|e| e.addr != 0));
    }

    #[test]
    fn every_forced_name_is_exported() {
        let names: Vec<_> = exports().iter().map(|e| e.name).collect();
        assert!(forced_names().iter().all(|f| names.contains(f)));
        assert!(!forced_names().contains(&"getentropy"));
    }

    #[test]
    fn a_game_env_name_must_be_a_name() {
        let bad = OsConfig {
            game_env: vec![("A=B".into(), "1".into())],
            alloc_big_align: false,
        };
        assert!(init_os(bad).is_err());
        let nul = OsConfig {
            game_env: vec![("A".into(), "1\0".into())],
            alloc_big_align: false,
        };
        assert!(init_os(nul).is_err());
    }
}
