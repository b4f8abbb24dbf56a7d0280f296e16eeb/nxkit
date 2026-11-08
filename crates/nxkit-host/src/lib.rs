//! The main program of nxkit: guest memory, the loader, the gates of the rewritten code, the game threads and the window.
//!
//! The contract with the build stage is in `docs/nxkit-host.md`. Only the modules `sys` and `gate` use `unsafe`
//! (`docs/architecture.md`, rule 5). The parts that talk to macOS exist on macOS only.

pub mod error;
#[cfg(target_os = "macos")]
pub mod frames;
pub mod gates;
pub mod input_file;
pub mod json;
pub mod layout;
pub mod macho;
pub mod manifest;
pub mod module;
pub mod npdm;
pub mod reloc;
pub mod settings;
pub mod stop;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod bigstack;

/// The boot of the SDK on the main guest thread.
#[cfg(target_os = "macos")]
pub mod boot;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod gate;
#[cfg(target_os = "macos")]
pub mod guestsym;
#[cfg(target_os = "macos")]
pub mod loader;
#[cfg(target_os = "macos")]
pub mod memory;
#[cfg(target_os = "macos")]
pub mod place;
#[cfg(target_os = "macos")]
pub mod probe;
#[cfg(target_os = "macos")]
pub mod refresh;
#[cfg(target_os = "macos")]
pub mod run;
#[cfg(target_os = "macos")]
pub mod seams;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod sys;
#[cfg(target_os = "macos")]
pub mod thread;
#[cfg(target_os = "macos")]
pub mod wire;

pub use error::{HostError, Result};
pub use manifest::Game;
