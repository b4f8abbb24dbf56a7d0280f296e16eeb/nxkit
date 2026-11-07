//! The video service: the `movie::Decoder` of the game's SDK, for a VP9 stream, decoded by VideoToolbox.
//!
//! Breath of the Wild plays its cut scenes as VP9 WebM files (R43). The game's SDK decoder would open the NVIDIA
//! decoder device, so this crate answers the methods of `movie::Decoder` itself (R14). It opens no IPC service.
//!
//! The host calls [`init`] with a [`Config`] before the game starts, and puts [`exports`] into the import table of the
//! game. [`movie_playing`] tells the pacing service that a movie is on screen.

mod backend;
mod decoder;
mod exports;
mod frame;
mod guest_api;
mod pacing;
mod vp9;

#[cfg(target_os = "macos")]
mod sys;
#[cfg(target_os = "macos")]
pub mod texture;

use std::sync::{Arc, RwLock};
use std::time::Duration;

use nxkit_abi::export::Export;
use nxkit_abi::ipc::Named;

pub use frame::{Frame, Nv12};
pub use pacing::playing as movie_playing;

/// What the host gives the service.
pub trait Host: Send + Sync {
    /// The address of a function or an object of the game by its mangled name, or `None` when the game has no such
    /// symbol.
    fn resolve(&self, symbol: &str) -> Option<usize>;

    /// Runs `job` on a guest thread after `delay`. `SignalEvent` of the game's SDK is guest code, so the events of a
    /// decoder go out through this.
    fn run_on_guest_thread(&self, delay: Duration, job: Box<dyn FnOnce() + Send>);
}

/// Takes a decoded picture into a texture of the game when the game asks for native texture output.
pub trait TextureSink: Send + Sync {
    /// Converts `frame` from NV12 to the format of the NVN texture object at `texture` (a guest address) and writes it.
    fn present(&self, texture: u64, frame: &Frame) -> Result<(), String>;

    /// Marks the NVN sync object at `sync` (a guest address) as signalled. The picture is on the GPU by then.
    fn signal_sync(&self, sync: u64) -> Result<(), String>;
}

/// The settings of the service.
#[derive(Clone)]
pub struct Config {
    /// The symbol table and the guest threads.
    pub host: Arc<dyn Host>,
    /// The texture hand-off. Without it a decoder in native texture mode is refused with a log line.
    pub sink: Option<Arc<dyn TextureSink>>,
}

static CONFIG: RwLock<Option<Config>> = RwLock::new(None);

/// Gives the service its settings. The host calls it once before the game starts.
pub fn init(config: &Config) {
    *CONFIG.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(config.clone());
}

pub(crate) fn config() -> Option<Config> {
    CONFIG.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// The functions that replace the methods of `movie::Decoder`.
pub fn exports() -> Vec<Export> {
    exports::table()
}

/// The Horizon services of this crate. It has none: a movie needs no IPC service.
pub fn services() -> Vec<Named> {
    Vec::new()
}
