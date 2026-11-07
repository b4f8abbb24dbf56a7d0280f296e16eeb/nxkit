//! The functions of the game's own SDK that the video service calls.
//!
//! `movie::Buffer` and `movie::MediaData` are classes of the guest. The service does not know their layout, so it
//! calls their methods through the symbol table of the game (R14). A symbol that is missing is a named error, never
//! a silent skip (R15).

use std::ffi::c_char;
use std::sync::OnceLock;

use crate::Host;

/// The SDK functions, resolved once.
pub(crate) struct Fns {
    pub(crate) set_data: extern "C" fn(u64, u64, u64),
    pub(crate) set_range: extern "C" fn(u64, i32, i32),
    pub(crate) data: extern "C" fn(u64) -> u64,
    pub(crate) capacity: extern "C" fn(u64) -> u64,
    pub(crate) set_int32: extern "C" fn(u64, *const c_char, i32) -> u32,
    pub(crate) set_rect: extern "C" fn(u64, *const c_char, i32, i32, i32, i32) -> u32,
    pub(crate) find_int32: extern "C" fn(u64, *const c_char, *mut i32) -> u32,
    pub(crate) signal_event: extern "C" fn(u64),
    pub(crate) delete: extern "C" fn(u64),
    pub(crate) nvn_sync: Option<extern "C" fn(u64) -> u64>,
    /// The address of the vtable of `movie::Decoder` plus 0x10, which is where an object's first word points.
    pub(crate) vtable: u64,
}

fn symbol(host: &dyn Host, name: &str) -> Result<usize, String> {
    host.resolve(name).ok_or_else(|| format!("the SDK symbol {name} is missing in this game"))
}

fn either(host: &dyn Host, first: &str, second: &str) -> Result<usize, String> {
    match host.resolve(first) {
        Some(a) => Ok(a),
        None => symbol(host, second),
    }
}

fn load(host: &dyn Host) -> Result<Fns, String> {
    let set_data = symbol(host, "_ZN5movie6Buffer18SetDataAndCapacityEPvm")?;
    let set_range = symbol(host, "_ZN5movie6Buffer8SetRangeEii")?;
    let data = either(host, "_ZNK5movie6Buffer4DataEv", "_ZN5movie6Buffer4DataEv")?;
    let capacity = either(host, "_ZNK5movie6Buffer8CapacityEv", "_ZN5movie6Buffer8CapacityEv")?;
    let set_int32 = symbol(host, "_ZN5movie9MediaData8SetInt32EPKci")?;
    let set_rect = symbol(host, "_ZN5movie9MediaData7SetRectEPKciiii")?;
    let find_int32 = symbol(host, "_ZNK5movie9MediaData9FindInt32EPKcPi")?;
    let signal_event = symbol(host, "_ZN2nn2os11SignalEventEPNS0_9EventTypeE")?;
    let delete = symbol(host, "_ZdlPv")?;
    let vtable = symbol(host, "_ZTVN5movie7DecoderE")? as u64 + 0x10;
    let nvn_sync = host.resolve("_ZNK5movie6Buffer10GetNvnSyncEv");
    // SAFETY: each address is the entry of a function of the game with the signature of the C++ method in the symbol,
    // on the arm64 calling convention where `this` is the first argument. `bool` comes back in `w0` and is read as a
    // `u32` that the caller masks.
    unsafe {
        Ok(Fns {
            set_data: std::mem::transmute::<usize, extern "C" fn(u64, u64, u64)>(set_data),
            set_range: std::mem::transmute::<usize, extern "C" fn(u64, i32, i32)>(set_range),
            data: std::mem::transmute::<usize, extern "C" fn(u64) -> u64>(data),
            capacity: std::mem::transmute::<usize, extern "C" fn(u64) -> u64>(capacity),
            set_int32: std::mem::transmute::<usize, extern "C" fn(u64, *const c_char, i32) -> u32>(set_int32),
            set_rect: std::mem::transmute::<usize, extern "C" fn(u64, *const c_char, i32, i32, i32, i32) -> u32>(set_rect),
            find_int32: std::mem::transmute::<usize, extern "C" fn(u64, *const c_char, *mut i32) -> u32>(find_int32),
            signal_event: std::mem::transmute::<usize, extern "C" fn(u64)>(signal_event),
            delete: std::mem::transmute::<usize, extern "C" fn(u64)>(delete),
            nvn_sync: nvn_sync.map(|a| std::mem::transmute::<usize, extern "C" fn(u64) -> u64>(a)),
            vtable,
        })
    }
}

static FNS: OnceLock<Result<Fns, String>> = OnceLock::new();

/// The SDK functions of the game that `init` was given, or the name of the first symbol that is missing.
pub(crate) fn fns() -> Result<&'static Fns, String> {
    let r = FNS.get_or_init(|| match crate::config() {
        Some(c) => load(&*c.host),
        None => Err("the video service was not initialised".to_string()),
    });
    r.as_ref().map_err(Clone::clone)
}
