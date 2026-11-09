//! The hooks that the host sets for the SDK-level exports (R38a).
//!
//! An export replaces a function of the game SDK, and some of them need what only the host or the game has: a symbol
//! of the game, the connected controller, a keyboard front end. The host fills the fields that
//! it can serve with [`set_seams`]. An export whose hook is missing answers as a console without the feature, for
//! example with a cancelled keyboard.

use std::sync::OnceLock;

/// What the software keyboard asks the front end for.
#[derive(Clone, Debug, Default)]
pub struct TextRequest {
    /// The header text of the keyboard.
    pub header: String,
    /// The sub text of the keyboard.
    pub sub: String,
    /// The guide text of the empty input line.
    pub guide: String,
    /// The text that the input line starts with.
    pub initial: String,
    /// The maximum length in UTF-16 units.
    pub max_len: u32,
    /// The minimum length in UTF-16 units.
    pub min_len: u32,
}

/// The hooks of the host. Every field is optional.
#[derive(Clone, Copy)]
pub struct Seams {
    /// The address of a symbol that the game defines, by its mangled name. The engine `guest_sym`. It gives the function
    /// of the game itself, never the export that replaces it, because `ScalableFontEngine::LoadFont` and the SDK
    /// software Opus decoder run through it. The exports that make a system event use it too.
    pub guest_sym: Option<fn(&str) -> Option<u64>>,
    /// The number of the first connected controller, for `nn::nfp::GetNpadId`. The engine `hle_hid_connected_npad`.
    pub hid_connected_npad: Option<fn() -> u32>,
    /// The software keyboard: the text that the player types, or `None` for a cancel.
    pub text_input: Option<fn(&TextRequest) -> Option<String>>,
    /// A new blank native image of the size, for the GameChat camera storage. Gives the image handle.
    pub native_image_blank: Option<fn(u32, u32) -> u32>,
    /// Releases a native image that `native_image_blank` made.
    pub native_image_free: Option<fn(u32)>,
    /// The nanoseconds of the guest clock. The host clock is the default.
    pub guest_ns: Option<fn() -> u64>,
    /// Starts another program of the application (`nn::oe::ExecuteProgram`). Gets the program index and the parameter
    /// bytes. The host ends the running program. The export does not return after the hook.
    pub execute_program: Option<fn(i32, &[u8])>,
    /// Maps the NRO image at the guest address with its size and gives the module token (`nn::ro::LoadModule`).
    pub nro_load: Option<fn(u64, u32) -> Option<u64>>,
    /// Unloads the module of a token (`nn::ro::UnloadModule`).
    pub nro_unload: Option<fn(u64)>,
    /// A symbol of the module of a token (`nn::ro::LookupModuleSymbol`).
    pub nro_symbol: Option<fn(u64, &str) -> Option<u64>>,
    /// The part of the graceful exit that only the host can do: pause the audio and suspend the graphics. Runs from
    /// [`quit`](crate::quit) before the process ends.
    pub quit: Option<fn()>,
}

impl Seams {
    /// No hook set.
    pub const NONE: Seams = Seams {
        guest_sym: None,
        hid_connected_npad: None,
        text_input: None,
        native_image_blank: None,
        native_image_free: None,
        guest_ns: None,
        execute_program: None,
        nro_load: None,
        nro_unload: None,
        nro_symbol: None,
        quit: None,
    };
}

impl Default for Seams {
    fn default() -> Seams {
        Seams::NONE
    }
}

static SEAMS: OnceLock<Seams> = OnceLock::new();

/// Sets the hooks of the host once, before the game runs. The first call wins. The exports read them without a lock
/// (R7).
pub fn set_seams(seams: Seams) {
    let _ = SEAMS.set(seams);
}

/// The hooks that are set.
pub(crate) fn seams() -> Seams {
    SEAMS.get().copied().unwrap_or(Seams::NONE)
}

/// The address of a symbol of the game.
pub(crate) fn guest_sym(name: &str) -> Option<u64> {
    (seams().guest_sym?)(name)
}

/// The nanoseconds of the guest clock.
pub(crate) fn guest_ns() -> u64 {
    match seams().guest_ns {
        Some(f) => f(),
        None => crate::sys::monotonic_ns(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guest_clock_defaults_to_the_host_clock() {
        assert!(seams().guest_ns.is_some() || guest_ns() > 0);
    }
}
