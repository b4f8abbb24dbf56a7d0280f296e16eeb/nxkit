//! The main program of nxkit: guest memory, the loader, the gates of the rewritten code, the game threads and the window.
//!
//! The contract with the build stage is in `docs/nxkit-host.md`. Only the modules `sys`, `gate` and `bigstack` use
//! `unsafe` (`docs/architecture.md`, rule 5). The parts that talk to macOS exist on macOS only.
//!
//! The crate is the composition root of the runtime (layer R4, W26): `run` takes the one snapshot of the `NXKIT_*`
//! environment (`nxkit_abi::env`, W27), builds the kernel, the services, the NVN front end, the GPU backend and the
//! window, and passes each part what it needs.

#[cfg(target_os = "macos")]
pub mod capture;
pub mod control;
pub mod error;
mod fields;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod frames;
pub mod gates;
pub mod graphics;
pub mod home;
pub mod hostenv;
pub mod input_file;
mod input_verbs;
pub mod json;
pub mod layout;
pub mod limits;
pub mod logtail;
pub mod macho;
pub mod manifest;
pub mod module;
pub mod npdm;
pub mod obs;
pub mod pipecache;
pub mod reloc;
pub mod saveplace;
pub mod savesync;
pub mod settings;
#[cfg(target_os = "macos")]
mod shaderctl;
pub mod stop;
pub mod textinput;

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)]
pub mod bigstack;

/// The boot of the SDK on the main guest thread.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod boot;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)]
pub mod gate;
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod guestjobs;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod guestsym;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod loader;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod memory;
#[cfg(target_os = "macos")]
pub mod memreport;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod obswire;
#[cfg(target_os = "macos")]
pub mod patchctl;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod place;
pub mod poolreport;
#[cfg(target_os = "macos")]
pub mod probe;
#[cfg(target_os = "macos")]
pub mod probe_pools;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod refresh;
#[cfg(target_os = "macos")]
pub mod run;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod seams;
#[cfg(target_os = "macos")]
pub mod stall;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)]
pub mod sys;
pub mod texreport;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod thread;
pub mod tupscale;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod wire;

pub use error::{HostError, Result};
pub use manifest::Game;

#[cfg(target_os = "ios")]
mod ios;
