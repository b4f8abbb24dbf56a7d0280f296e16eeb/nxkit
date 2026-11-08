//! The NVN C ABI of the host.
//!
//! A game gets every NVN function by name from `nvnBootstrapLoader` or `nvnDeviceGetProcAddress`. Both end in
//! [`lookup`]. The functions are `extern "C"` and follow the signature of real NVN: an argument of fewer than 32 bits
//! is a `u32`, a pointer is a `u64` (a guest address is a host address, R18), an enum is an `i32`, and a boolean result
//! is a `u32`. No function is variadic.
//!
//! The groups:
//! - [`device`], [`queue`], [`sync`], [`pool`], [`buffer`], [`window`], [`event`] and [`program`]: objects and
//!   functions without a recording. A group keeps plain data in the guest object ([`abi::Obj`]) and host data in a
//!   [`abi::Registry`].
//! - [`texture`], [`sampler`], [`state`] and [`cmdbuf`]: texture and sampler objects with the descriptor pools, the
//!   state setters and the command buffers. They record through the front end ([`ctx::Gpu`]).
//!
//! Each group has `procs()`, its names and addresses. [`procs`] joins them. `FFI_GROUPS` in `nxkit-nvn` names what each
//! group must export, and a test per group compares the two.
//!
//! A name that the game asks for and that no group exports:
//! - If `nxnative` knows it, [`lookup`] gives a function that ends the game with the name when it is called (R24).
//!   A game that probes an optional function and never calls it keeps running.
//! - If nobody knows it, [`lookup`] logs the name once and gives NULL.
//!
//! The host calls [`install`] once with the backend, before the game starts.

pub(crate) mod abi;
pub(crate) mod buffer;
pub(crate) mod cmdbuf;
pub(crate) mod ctx;
pub(crate) mod device;
pub(crate) mod event;
pub(crate) mod pool;
pub(crate) mod program;
pub(crate) mod queue;
pub(crate) mod sampler;
pub(crate) mod state;
pub(crate) mod sync;
pub(crate) mod texture;
mod wire;
pub(crate) mod window;

pub use cmdbuf::{CommandSettings, set_command_settings};
pub use ctx::{InstallError, Settings, install};
pub use device::{NVN2_API_MAJOR, NVN2_API_MINOR, scan_sdk_driver, sdk_item};

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock, PoisonError};

use nxkit_abi::guest;
use nxkit_nvn::entries::{NVN_NAMES, parse_names};

use abi::{fatal, guard};

/// The longest function name that a game asks for.
const NAME_MAX: usize = 256;

/// The name under which the game imports the loader. It is a C name, not mangled (`HLE_FORCE("nvnBootstrapLoader")` in nxnative).
pub(crate) const BOOTSTRAP_LOADER_NAME: &str = "nvnBootstrapLoader";

/// Every function that the groups export, in the order of the groups.
pub(crate) fn procs() -> &'static [(&'static str, usize)] {
    static PROCS: OnceLock<Vec<(&'static str, usize)>> = OnceLock::new();
    PROCS.get_or_init(|| {
        let mut all = Vec::new();
        all.extend(device::procs());
        all.extend(queue::procs());
        all.extend(sync::procs());
        all.extend(pool::procs());
        all.extend(buffer::procs());
        all.extend(window::procs());
        all.extend(event::procs());
        all.extend(program::procs());
        all.extend(texture::procs());
        all.extend(sampler::procs());
        all.extend(state::procs());
        all.extend(cmdbuf::procs());
        all
    })
}

/// The names that `nxnative` knows, in the order of the stubs.
fn known_names() -> &'static [&'static str] {
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| parse_names(NVN_NAMES).collect())
}

/// The function that stands for the name number `N` of [`known_names`] until a group exports it.
extern "C" fn stub<const N: usize>() -> ! {
    let name = known_names().get(N).copied().unwrap_or("a function");
    fatal(name, "is not implemented")
}

macro_rules! row {
    ($b:literal) => {
        [
            stub::<{ $b }> as extern "C" fn() -> !,
            stub::<{ $b + 1 }> as extern "C" fn() -> !,
            stub::<{ $b + 2 }> as extern "C" fn() -> !,
            stub::<{ $b + 3 }> as extern "C" fn() -> !,
            stub::<{ $b + 4 }> as extern "C" fn() -> !,
            stub::<{ $b + 5 }> as extern "C" fn() -> !,
            stub::<{ $b + 6 }> as extern "C" fn() -> !,
            stub::<{ $b + 7 }> as extern "C" fn() -> !,
            stub::<{ $b + 8 }> as extern "C" fn() -> !,
            stub::<{ $b + 9 }> as extern "C" fn() -> !,
        ]
    };
}

/// Rows of ten stubs. The rows cover the numbers 0 to 749, which holds every name of `nvn_names.txt`.
const STUB_ROWS: usize = 75;

/// One stub for each name number. A test checks that the table covers every known name.
fn stubs() -> &'static [[extern "C" fn() -> !; 10]; STUB_ROWS] {
    static STUBS: [[extern "C" fn() -> !; 10]; STUB_ROWS] = [
        row!(0),
        row!(10),
        row!(20),
        row!(30),
        row!(40),
        row!(50),
        row!(60),
        row!(70),
        row!(80),
        row!(90),
        row!(100),
        row!(110),
        row!(120),
        row!(130),
        row!(140),
        row!(150),
        row!(160),
        row!(170),
        row!(180),
        row!(190),
        row!(200),
        row!(210),
        row!(220),
        row!(230),
        row!(240),
        row!(250),
        row!(260),
        row!(270),
        row!(280),
        row!(290),
        row!(300),
        row!(310),
        row!(320),
        row!(330),
        row!(340),
        row!(350),
        row!(360),
        row!(370),
        row!(380),
        row!(390),
        row!(400),
        row!(410),
        row!(420),
        row!(430),
        row!(440),
        row!(450),
        row!(460),
        row!(470),
        row!(480),
        row!(490),
        row!(500),
        row!(510),
        row!(520),
        row!(530),
        row!(540),
        row!(550),
        row!(560),
        row!(570),
        row!(580),
        row!(590),
        row!(600),
        row!(610),
        row!(620),
        row!(630),
        row!(640),
        row!(650),
        row!(660),
        row!(670),
        row!(680),
        row!(690),
        row!(700),
        row!(710),
        row!(720),
        row!(730),
        row!(740),
    ];
    &STUBS
}

/// The address of the function for `name`, or 0.
///
/// A function of a group is returned as it is. A name that `nxnative` knows and no group exports gets its stub.
/// An unknown name is logged once.
pub(crate) fn lookup(name: &str) -> usize {
    static BY_NAME: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    static INDEX: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    let by_name = BY_NAME.get_or_init(|| procs().iter().copied().collect());
    if let Some(addr) = by_name.get(name) {
        return *addr;
    }
    let index = INDEX.get_or_init(|| {
        known_names()
            .iter()
            .enumerate()
            .map(|(i, n)| (*n, i))
            .collect()
    });
    match index.get(name) {
        Some(&i) => stubs()[i / 10][i % 10] as usize,
        None => {
            static LOGGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
            let logged = LOGGED.get_or_init(|| Mutex::new(HashSet::new()));
            let first = logged
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(name.to_owned());
            if first {
                eprintln!("[RED] NVN address lookup of an unknown name: {name}");
            }
            0
        }
    }
}

/// Reads the name that a game passes to a loader.
pub(crate) fn name_at(name: u64) -> abi::AbiResult<String> {
    let bytes = guest::read_cstr(name, NAME_MAX)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `nvnBootstrapLoader(const char *name)`: the first function that a game calls. It gives the function for a name,
/// as `nvnDeviceGetProcAddress` does, without a device.
pub(crate) extern "C" fn nvn_bootstrap_loader(name: u64) -> u64 {
    guard("nvnBootstrapLoader", || Ok(lookup(&name_at(name)?) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_nvn::entries::FFI_GROUPS;

    #[test]
    fn the_stub_table_covers_every_known_name() {
        assert!(known_names().len() <= STUB_ROWS * 10);
    }

    #[test]
    fn no_name_is_exported_twice() {
        let mut seen = HashSet::new();
        for (name, _) in procs() {
            assert!(seen.insert(*name), "{name} is exported twice");
        }
    }

    #[test]
    fn every_group_that_has_a_row_exports_exactly_that_row() {
        let groups: [(&str, Vec<(&str, usize)>); 12] = [
            ("device", device::procs()),
            ("queue", queue::procs()),
            ("sync", sync::procs()),
            ("pool", pool::procs()),
            ("buffer", buffer::procs()),
            ("window", window::procs()),
            ("event", event::procs()),
            ("program", program::procs()),
            ("texture", texture::procs()),
            ("sampler", sampler::procs()),
            ("state", state::procs()),
            ("cmdbuf", cmdbuf::procs()),
        ];
        for (group, got) in groups {
            let mut got: Vec<&str> = got.iter().map(|(n, _)| *n).collect();
            let mut want: Vec<&str> = FFI_GROUPS
                .iter()
                .filter(|(g, _)| *g == group)
                .flat_map(|(_, names)| names.iter().copied())
                .collect();
            got.sort_unstable();
            want.sort_unstable();
            assert_eq!(got, want, "group {group}");
        }
    }

    #[test]
    fn a_known_name_without_a_group_gets_a_stub_and_an_unknown_name_gets_null() {
        let stub_addr = lookup("nvnCommandBufferEvaluateDlss");
        assert_ne!(stub_addr, 0);
        assert_eq!(lookup("nvnNoSuchFunction"), 0);
        assert_eq!(
            lookup("nvnDeviceGetProcAddress"),
            device::procs()
                .iter()
                .find(|(n, _)| *n == "nvnDeviceGetProcAddress")
                .map_or(0, |(_, a)| *a)
        );
    }

    #[test]
    fn every_known_name_has_an_address() {
        for name in known_names() {
            assert_ne!(lookup(name), 0, "{name}");
        }
    }
}
