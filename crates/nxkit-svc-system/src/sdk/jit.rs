//! `nn::jit`: code generation at run time. Source: nxnative `dm/host/hle_jit.c`.
//!
//! On a console the jit service loads the JIT plugin of the game, an NRO, into a process of its own. It maps the code
//! memory of the game read-write there and read and execute in the game, and calls the plugin entry points. Only Super
//! Mario 3D All-Stars (Super Mario 64) uses it. nxnative takes over the whole API and runs the plugin on the calling
//! thread.
//!
//! The host of nxkit has no part of what that needs: it loads no NRO (the `nro_*` seams stop the game), it has no code
//! memory with write protection that the guest can run, and it has no loader that relocates a module alone against
//! itself. So `Initialize` and `Finalize` are ported as they are, and `CreateJitEnvironment` stops the game with a
//! `[RED]` note instead of letting the SDK read the missing service session (R15, R61). The game gets no handle, so
//! `DestroyJitEnvironment`, `GetJitEnvironmentInfo`, `Control` and `GenerateCode` cannot be reached and are not
//! taken over.

use super::{fatal, log_once, nothing};
use std::sync::atomic::AtomicBool;

static INIT_LOGGED: AtomicBool = AtomicBool::new(false);

/// `Initialize()`: connects to the jit service; the host runs the plugin itself.
extern "C" fn initialize() {
    log_once(&INIT_LOGGED, "init takeover: jit::Initialize (no-op)");
}

/// `CreateJitEnvironment(JitHandle*, const JitEnvironmentConfiguration&, const JitPluginInfo&, void*, size_t)`: the
/// jit service: load the plugin NRO into the host and give it code memory the game can run. The host cannot do that
/// yet.
extern "C" fn create_jit_environment() -> u32 {
    fatal(
        "[RED] jit::CreateJitEnvironment: the host cannot run a JIT plugin (it loads no NRO and has no code memory that \
         the plugin can write and the game can run), so Super Mario 64 of Super Mario 3D All-Stars cannot go on",
    )
}

table! {
    "_ZN2nn3jit10InitializeEv" => initialize,
    "_ZN2nn3jit8FinalizeEv" => nothing,
    "_ZN2nn3jit20CreateJitEnvironmentEPNS0_9JitHandleERKNS0_27JitEnvironmentConfigurationERKNS0_13JitPluginInfoEPvm" => create_jit_environment,
}
