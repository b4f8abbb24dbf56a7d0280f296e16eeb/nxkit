//! The service initialisation that `nninitInitializeSdkModule` runs.
//!
//! On a console each call opens a session to a system service. The host takes the call over and opens none.
//! `nn::fs::InitializeForApplication` is in the list of the engine too, and the file system crate serves it, so it is
//! left out here.

use super::log_once;
use std::sync::atomic::AtomicBool;

macro_rules! once {
    ($name:ident, $what:literal) => {
        extern "C" fn $name() {
            static SEEN: AtomicBool = AtomicBool::new(false);
            log_once(&SEEN, concat!("init takeover: ", $what, " (no-op)"));
        }
    };
}

once!(diag_startup, "nndiagStartup");
once!(oe_initialize, "oe::Initialize");
once!(aoc_initialize, "aoc::Initialize");
once!(err_context, "err::InitializeContextWriterApi");
once!(pctl_initialize, "pctl::Initialize");
once!(abort_observer, "diag::InitializeApplicationAbortObserver");

table! {
    "nndiagStartup" => diag_startup,
    "_ZN2nn2oe10InitializeEv" => oe_initialize,
    "_ZN2nn3aoc10InitializeEv" => aoc_initialize,
    "_ZN2nn3err26InitializeContextWriterApiEv" => err_context,
    "_ZN2nn4pctl10InitializeEv" => pctl_initialize,
    "_ZN2nn4diag34InitializeApplicationAbortObserverEv" => abort_observer,
}
