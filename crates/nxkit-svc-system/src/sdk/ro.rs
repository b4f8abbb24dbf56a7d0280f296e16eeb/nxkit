//! `nn::ro`: loading NRO modules at run time. Source: nxnative `dm/host/hle_ro.c`.
//!
//! The console connects to the `ldr:ro` service. The host has none, and the SDK `Initialize` reads a null session.
//! The symbol lookups search the loaded modules through the `guest_sym` seam. `LoadModule`, `UnloadModule` and the
//! per-module lookup go to the `nro_load`, `nro_unload` and `nro_symbol` seams, because the host owns the loader.
//! `nro_load` runs the init of the module. It returns `None` for an image that the host does not load, and the call
//! then fails. `UnloadModule` runs the fini of the module inside `nro_unload`.
//!
//! The `Module` object of the game holds the module token at +0, the state at +8 (1 when loaded), the image at +0x10
//! and the buffer at +0x18. NRR files need no check, so registering one succeeds when it has the `NRR0` magic.
//! The load needs no lock here. The host loader orders its own state, and nxnative's lock around the SDK is the lock
//! of the host loader in nxkit.

use super::{get_cstr, get_u32, get_u64, log_once, nothing, put_u32, put_u64};
use crate::seams::{guest_sym, seams};

/// A failure under the `ro` module.
const RO_FAIL: u32 = 22 | (1 << 9);
/// The result for an image without the `NRO0` magic.
const RO_NOT_NRO: u32 = 0x816;

fn is_nro(image: u64) -> bool {
    image != 0 && get_u32(image + 0x10) == u32::from_le_bytes(*b"NRO0")
}

/// `Initialize()`: nothing to connect.
extern "C" fn initialize() -> u32 {
    log_once(&FLAG, "init takeover: ro::Initialize (no-op)");
    0
}

static FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `LookupSymbol(uintptr_t* out, const char* name)`: a definition in the loaded modules.
extern "C" fn lookup_symbol(out: u64, name: u64) -> u32 {
    if name == 0 {
        return RO_FAIL;
    }
    match guest_sym(&get_cstr(name, 512)) {
        Some(a) => {
            put_u64(out, a);
            0
        }
        None => RO_FAIL,
    }
}

/// The token of a loaded `Module` object, or 0.
fn token(module: u64) -> u64 {
    if module == 0 || get_u32(module + 8) != 1 {
        return 0;
    }
    get_u64(module)
}

/// `LookupModuleSymbol(uintptr_t* out, const Module*, const char* name)`: a definition of one loaded NRO.
extern "C" fn lookup_module_symbol(out: u64, module: u64, name: u64) -> u32 {
    let (t, f) = (token(module), seams().nro_symbol);
    let Some(f) = f else {
        return RO_FAIL;
    };
    if t == 0 || name == 0 {
        return RO_FAIL;
    }
    match f(t, &get_cstr(name, 512)) {
        Some(a) => {
            put_u64(out, a);
            0
        }
        None => RO_FAIL,
    }
}

/// `GetBufferSize(size_t* out, const void* image)`: the bss size from the NRO header.
extern "C" fn buffer_size(out: u64, image: u64) -> u32 {
    if !is_nro(image) {
        return RO_NOT_NRO;
    }
    put_u64(out, u64::from(get_u32(image + 0x38)));
    0
}

/// `LoadModule(Module*, const void* image, void* buffer, size_t size, int flag[, bool])`.
extern "C" fn load_module(module: u64, image: u64, buffer: u64, _size: u64, _flag: u32) -> u32 {
    if !is_nro(image) {
        return RO_NOT_NRO;
    }
    let Some(load) = seams().nro_load else {
        return RO_FAIL;
    };
    let Some(t) = load(image, get_u32(image + 0x18)) else {
        return RO_FAIL;
    };
    if module != 0 {
        put_u64(module, t);
        put_u32(module + 8, 1);
        put_u64(module + 0x10, image);
        put_u64(module + 0x18, buffer);
    }
    0
}

/// `UnloadModule(Module*)`.
extern "C" fn unload_module(module: u64) {
    let t = token(module);
    if t == 0 {
        return;
    }
    if let Some(f) = seams().nro_unload {
        f(t);
    }
    put_u32(module + 8, 0);
    put_u64(module, 0);
}

/// `RegisterModuleInfo(RegistrationInfo*, const void* nrr)`: any NRR with its magic.
extern "C" fn register_module_info(_info: u64, nrr: u64) -> u32 {
    if nrr == 0 || get_u32(nrr) != u32::from_le_bytes(*b"NRR0") {
        return RO_FAIL;
    }
    0
}

table! {
    "_ZN2nn2ro10InitializeEv" => initialize,
    "_ZN2nn2ro12LookupSymbolEPmPKc" => lookup_symbol,
    "_ZN2nn2ro18LookupModuleSymbolEPmPKNS0_6ModuleEPKc" => lookup_module_symbol,
    "_ZN2nn2ro13GetBufferSizeEPmPKv" => buffer_size,
    "_ZN2nn2ro10LoadModuleEPNS0_6ModuleEPKvPvmi" => load_module,
    "_ZN2nn2ro10LoadModuleEPNS0_6ModuleEPKvPvmib" => load_module,
    "_ZN2nn2ro8FinalizeEv" => nothing,
    "_ZN2nn2ro12UnloadModuleEPNS0_6ModuleE" => unload_module,
    "_ZN2nn2ro18RegisterModuleInfoEPNS0_16RegistrationInfoEPKv" => register_module_info,
    "_ZN2nn2ro20UnregisterModuleInfoEPNS0_16RegistrationInfoE" => nothing,
}
