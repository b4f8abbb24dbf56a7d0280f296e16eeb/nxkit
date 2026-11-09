//! The audio service of the nxkit host.
//!
//! It serves a game the way the engine does: at SDK level. [`exports`] replaces the `nn::audio` functions of AudioOut,
//! AudioIn, the output stream, the audio device and the audio renderer. The IPC services [`services`] (`audout:u` and
//! `audren:u`) answer SDK code that the take-overs do not replace.
//!
//! Output runs on one mixer in the callback of the output unit (R20). The callback takes no lock and allocates nothing.
//! The audio renderer is the SDK's own CPU renderer, which runs in the guest. The host reads the DeviceSink out of its
//! command list and plays it on the mixer (R41).
//!
//! `unsafe` is in the module `sys` only.

mod engine;
mod host;
mod ipc_out;
mod ipc_ren;
mod ren_cmd;
mod ren_run;
mod ren_scan;
mod ren_state;
mod ren_work;
mod sdk_dev;
mod sdk_out;
mod sdk_ren;
mod sdk_stream;
mod sys;
mod util;

pub use host::{Host, set_host};

use nxkit_abi::export::Export;
use nxkit_abi::ipc::Named;
use std::sync::Arc;

/// The settings of the audio service.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    /// The gain of the final mix. 1.0 leaves it unchanged.
    pub master_volume: f32,
    /// When true, no output device is opened and the mix is dropped. The game still sees its buffers played.
    pub silent_sink: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            master_volume: 1.0,
            silent_sink: false,
        }
    }
}

/// Applies `cfg`. Call it once before the game starts.
pub fn init(cfg: &Config) {
    engine::mixer().configure(cfg.master_volume, cfg.silent_sink);
}

/// Both export lists: the ones that run on the guest stack and the ones that need a big stack.
fn lists() -> (Vec<Export>, Vec<Export>) {
    let mut v = Vec::new();
    let mut big = Vec::new();
    sdk_out::exports(&mut v, &mut big);
    sdk_stream::exports(&mut v, &mut big);
    sdk_dev::exports(&mut v);
    sdk_ren::exports(&mut v);
    (v, big)
}

/// The SDK functions that this crate takes over, each by its Switch mangled name. They run on the guest stack.
pub fn exports() -> Vec<Export> {
    lists().0
}

/// The take-overs that the engine registers as `HLE_FORCE_BIG`. The host runs them on a big stack.
pub fn big_stack_exports() -> Vec<Export> {
    lists().1
}

/// The IPC services `audout:u` and `audren:u`.
pub fn services() -> Vec<Named> {
    vec![
        Named {
            name: "audout:u",
            root: Arc::new(ipc_out::AudioOutManager),
        },
        Named {
            name: "audren:u",
            root: Arc::new(ipc_ren::AudioRendererManager),
        },
    ]
}
