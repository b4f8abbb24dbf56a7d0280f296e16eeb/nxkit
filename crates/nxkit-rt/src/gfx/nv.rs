//! `nv::` take-overs. Source: nxnative `dm/gfx/hle_nv.c`.
//!
//! `nv::` is the driver glue beneath NVN. It makes the transfer memory and connects to the `nvdrv` service. NVN is
//! replaced wholesale by [`crate::nvn`], so the layer has nothing to do. The host records what the game hands in:
//! the allocator callbacks and the graphics pool. `nv::InitCheck` and `nv::GetGraphicsMemoryInfo` answer from that
//! record, because the SDK versions read the driver object that the host never creates.

use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, MutexGuard, PoisonError};

use nxkit_abi::guest;

use super::{checked, fatal, log, log_once_with, old_sdk, table};

/// The allocator that the game installs: the callbacks of `nv::SetGraphicsAllocator` and their user pointer. All
/// are guest addresses. The graphics layer takes them from here when it allocates with the allocator of the game.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Allocator {
    /// `void *(*)(size_t size, size_t align, void *user)`
    pub(crate) alloc: u64,
    /// `void (*)(void *p, void *user)`
    pub(crate) free: u64,
    /// `void *(*)(void *p, size_t size, void *user)`
    pub(crate) realloc: u64,
    /// The pointer that the callbacks get.
    pub(crate) user: u64,
}

/// The graphics pool that the game installs. Nothing is allocated from it on the host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Pool {
    mem: u64,
    size: u64,
    inited: bool,
}

#[derive(Debug, Default)]
struct State {
    alloc: Allocator,
    devtools_alloc: Allocator,
    pool: Pool,
}

static STATE: Mutex<State> = Mutex::new(State {
    alloc: Allocator {
        alloc: 0,
        free: 0,
        realloc: 0,
        user: 0,
    },
    devtools_alloc: Allocator {
        alloc: 0,
        free: 0,
        realloc: 0,
        user: 0,
    },
    pool: Pool {
        mem: 0,
        size: 0,
        inited: false,
    },
});

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The allocator of `nv::SetGraphicsAllocator`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn allocator() -> Allocator {
    state().alloc
}

/// The allocator of `nv::SetGraphicsDevtoolsAllocator`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn devtools_allocator() -> Allocator {
    state().devtools_alloc
}

/// `nv::SetGraphicsAllocator(alloc, free, realloc, user)`: NVN has been replaced wholesale with a host device, so
/// the driver glue layer has nothing to do. The allocator is recorded.
extern "C" fn set_allocator(alloc: u64, free: u64, realloc: u64, user: u64) {
    state().alloc = Allocator {
        alloc,
        free,
        realloc,
        user,
    };
}

/// `nv::SetGraphicsDevtoolsAllocator(alloc, free, realloc, user)`: as above, for the devtools allocator.
extern "C" fn set_devtools_allocator(alloc: u64, free: u64, realloc: u64, user: u64) {
    state().devtools_alloc = Allocator {
        alloc,
        free,
        realloc,
        user,
    };
}

/// `nv::InitializeGraphics(void *mem, size_t size)`: the pool is recorded for `nv::GetGraphicsMemoryInfo`. The
/// graphics layer allocates its own Metal objects.
extern "C" fn initialize_graphics(mem: u64, size: u64) {
    state().pool = Pool {
        mem,
        size,
        inited: true,
    };
    log(format_args!(
        "nv::InitializeGraphics: pool {:.2} MB, recorded (the host's graphics layer allocates its own)",
        size as f64 / 1_048_576.0
    ));
}

/// `nv::FinalizeGraphics()`.
extern "C" fn finalize_graphics() {
    state().pool = Pool::default();
}

/// `nv::InitCheck()`: ends the game unless graphics are initialised. The SDK tests its own flag, which
/// `nv::InitializeGraphics` sets and which runs on the host instead, so the record of the host answers.
extern "C" fn init_check() {
    if !state().pool.inited {
        fatal(
            "nv::InitCheck",
            "graphics not initialised (nv::InitializeGraphics was not called)",
        );
    }
}

/// `nn::gs::sdk::CheckGraphicsSystemInitialized(const char *who, size_t n)`: the graphics system is the graphics
/// layer of the host, up from the start. The SDK aborts unless its own flag reads 1, which its graphics boot sets.
extern "C" fn check_graphics_system_initialized(_who: u64, _n: u64) {}

/// Bytes of `nv::MemoryInfo` up to SDK 10 (four counters).
const MEMORY_INFO_OLD: u64 = 0x20;
/// Bytes of `nv::MemoryInfo` from SDK 11 on.
const MEMORY_INFO: u64 = 0x40;

static INFO_NOTE: AtomicBool = AtomicBool::new(false);

/// `nv::GetGraphicsMemoryInfo(nv::MemoryInfo *out)`: the pool that the game installed, with nothing used from it.
/// The counters are 64 bits: total at 0, used at 8 (0) and free at 16. The size of the record follows the SDK, 0x20
/// bytes up to SDK 10 and 0x40 from SDK 11 on. Writing 0x40 on an old SDK overruns the record of the caller.
extern "C" fn get_graphics_memory_info(out: u64) {
    const NAME: &str = "nv::GetGraphicsMemoryInfo";
    let len = if old_sdk() {
        MEMORY_INFO_OLD
    } else {
        MEMORY_INFO
    };
    checked(NAME, guest::fill(out, len, 0));
    let pool = state().pool;
    if pool.inited {
        checked(NAME, guest::write_u64(out, pool.size));
        checked(NAME, guest::write_u64(out + 16, pool.size));
    } else {
        log_once_with(&INFO_NOTE, || {
            format!("{NAME}: graphics are not initialised, all counters are 0")
        });
    }
}

/// `nv::DumpGraphicsMemoryInfo()`: the dump goes to the log of the host.
extern "C" fn dump_graphics_memory_info() {
    let size = state().pool.size;
    log(format_args!(
        "nv::DumpGraphicsMemoryInfo: pool {:.2} MB, 0 used (host graphics layer)",
        size as f64 / 1_048_576.0
    ));
}

table! {
    "_ZN2nv20SetGraphicsAllocatorEPFPvmmS0_EPFvS0_S0_EPFS0_S0_mS0_ES0_" => set_allocator,
    "_ZN2nv28SetGraphicsDevtoolsAllocatorEPFPvmmS0_EPFvS0_S0_EPFS0_S0_mS0_ES0_" => set_devtools_allocator,
    "_ZN2nv18InitializeGraphicsEPvm" => initialize_graphics,
    "_ZN2nv16FinalizeGraphicsEv" => finalize_graphics,
    "_ZN2nv9InitCheckEv" => init_check,
    "_ZN2nn2gs3sdk30CheckGraphicsSystemInitializedEPKcm" => check_graphics_system_initialized,
    "_ZN2nv21GetGraphicsMemoryInfoEPNS_10MemoryInfoE" => get_graphics_memory_info,
    "_ZN2nv22DumpGraphicsMemoryInfoEv" => dump_graphics_memory_info,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::testing;

    /// The tests share the state of the module, so one test holds it.
    #[test]
    fn the_pool_allocator_and_memory_info_follow_the_calls() {
        set_allocator(1, 2, 3, 4);
        set_devtools_allocator(5, 6, 7, 8);
        assert_eq!(
            allocator(),
            Allocator {
                alloc: 1,
                free: 2,
                realloc: 3,
                user: 4
            }
        );
        assert_eq!(
            devtools_allocator(),
            Allocator {
                alloc: 5,
                free: 6,
                realloc: 7,
                user: 8
            }
        );

        let out = testing::alloc(0x100);
        guest::fill(out, 0x100, 0xAA).expect("fill");
        get_graphics_memory_info(out);
        assert_eq!(guest::read_u64(out).expect("total"), 0);
        assert_eq!(guest::read_u64(out + 0x38).expect("tail"), 0);
        assert_eq!(
            guest::read_u64(out + 0x40).expect("past the record"),
            0xAAAA_AAAA_AAAA_AAAA
        );

        initialize_graphics(0x1000, 3 << 20);
        init_check();
        get_graphics_memory_info(out);
        assert_eq!(guest::read_u64(out).expect("total"), 3 << 20);
        assert_eq!(guest::read_u64(out + 8).expect("used"), 0);
        assert_eq!(guest::read_u64(out + 16).expect("free"), 3 << 20);
        assert_eq!(guest::read_u64(out + 24).expect("fourth"), 0);

        finalize_graphics();
        get_graphics_memory_info(out);
        assert_eq!(guest::read_u64(out).expect("total"), 0);
    }

    #[test]
    fn the_exports_are_the_names_of_the_list() {
        let names: Vec<&str> = exports().iter().map(|e| e.name).collect();
        assert_eq!(names.len(), 8);
        assert!(names.contains(&"_ZN2nv18InitializeGraphicsEPvm"));
    }
}
