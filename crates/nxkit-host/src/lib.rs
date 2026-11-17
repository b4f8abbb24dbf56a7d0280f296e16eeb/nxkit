//! The main program of nxkit: guest memory, the loader, the gates of the rewritten code, the game threads and the window.
//!
//! The contract with the build stage is in `docs/nxkit-host.md`. Only the modules `sys` and `gate` use `unsafe`
//! (`docs/architecture.md`, rule 5). The parts that talk to macOS exist on macOS only.

pub mod error;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod frames;
pub mod gates;
pub mod input_file;
pub mod json;
pub mod layout;
pub mod macho;
pub mod manifest;
pub mod module;
pub mod npdm;
pub mod pipecache;
pub mod reloc;
pub mod settings;
pub mod stop;

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
pub mod guestsym;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod loader;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod memory;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod memreport;
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
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)]
pub mod sys;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod texreport;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod thread;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod wire;

pub use error::{HostError, Result};
pub use manifest::Game;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod input;
#[cfg(target_os = "ios")]
#[allow(unsafe_code)]
mod ios;
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod startup;
