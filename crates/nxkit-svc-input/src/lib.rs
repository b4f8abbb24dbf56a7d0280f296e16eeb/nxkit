//! The input service: controllers, keyboard, mouse, rumble and motion for the guest.
//!
//! The game's `nn::hid` calls are answered by [`exports`] (R14, R20). Each call reads the controller at that
//! moment through the platform layer and writes the answer into the game's buffer (R39). Nothing polls between
//! calls. The `hid` IPC service of [`services`] answers the SDK code that the exports do not replace and hands out
//! the HID shared memory for games that read it directly (R40). The host calls [`tick`] on each display tick, which
//! renews the rumble pulses and writes the shared memory.
//!
//! Player 1 also gets the keyboard and the mouse in the PC layout of [`bind`] (R63). The keys are read at the call
//! like a controller. The mouse comes from the events of the host window through [`mouse_moved`], [`mouse_button`]
//! and [`mouse_wheel`], because its motion cannot be read at a call. The keyboard and the mouse add to a controller,
//! so they only change the state while a key or button is held or the mouse moves.
//!
//! Where the Mac has no GameController, the platform layer reports no devices.

mod bind;
mod config;
mod exports;
mod ipc;
mod keys;
mod look;
mod motion;
mod npad;
mod pointer;
mod rumble;
mod seam;
mod shmem;
mod slots;
mod state;
mod sys;
mod touch;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use nxkit_abi::export::Export;
use nxkit_abi::ipc::Named;

pub use config::Config;
pub use look::LookConfig;
pub use seam::{SystemEventHooks, set_system_event_hooks};
pub use touch::Touch;

use state::Input;

static INPUT: OnceLock<Mutex<Input>> = OnceLock::new();

fn cell() -> &'static Mutex<Input> {
    INPUT.get_or_init(|| Mutex::new(Input::new(&Config::default())))
}

/// Runs `f` on the state of the service. A panic in an earlier call does not stop the service.
pub(crate) fn with<R>(f: impl FnOnce(&mut Input) -> R) -> R {
    let mut guard = cell().lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut guard)
}

/// Sets up the service with the settings of the host. The host calls it before the game starts. A service that is
/// not set up uses [`Config::default`].
pub fn init(config: &Config) {
    with(|input| *input = Input::new(config));
    pointer::reset(config.look);
    MOUSE_CAPTURE.store(config.keyboard && config.mouse_capture, Ordering::Relaxed);
}

/// The messages of the `(key, action)` pairs of [`Config::binds`] that do not parse. The service skips them.
pub fn bind_errors(pairs: &[(String, String)]) -> Vec<String> {
    bind::Bindings::pc().rebind_all(pairs)
}

/// The `nn::hid` and `nn::kpr` functions that the game gets in place of the SDK's own.
pub fn exports() -> Vec<Export> {
    exports::all()
}

/// The functions that must run on a big stack. The input service has none.
pub fn big_stack_exports() -> Vec<Export> {
    Vec::new()
}

/// The IPC services of the input service: `hid`.
pub fn services() -> Vec<Named> {
    ipc::named()
}

static FOCUSED: AtomicBool = AtomicBool::new(true);

/// Tells the service that the window gained or lost the key focus. A window without focus reports no key and no mouse
/// button, so a key that was held at the loss is released.
pub fn set_focus(focused: bool) {
    FOCUSED.store(focused, Ordering::Relaxed);
    if !focused {
        mouse_release();
    }
}

pub(crate) fn focused() -> bool {
    FOCUSED.load(Ordering::Relaxed)
}

static CONTROLLER: AtomicBool = AtomicBool::new(false);

/// True when a game controller is connected, as of the last lookup. The window frees the mouse while one is.
pub fn controller_connected() -> bool {
    CONTROLLER.load(Ordering::Relaxed)
}

pub(crate) fn set_controller_connected(connected: bool) {
    CONTROLLER.store(connected, Ordering::Relaxed);
}

static MOUSE_CAPTURE: AtomicBool = AtomicBool::new(false);

/// True when the window may hold the mouse for the game: the service is set up with keyboard and mouse and with
/// [`Config::mouse_capture`].
pub fn mouse_capture() -> bool {
    MOUSE_CAPTURE.load(Ordering::Relaxed)
}

/// The held mouse moved by `dx`, `dy` points, x right and y down. It turns the camera stick of player 1.
pub fn mouse_moved(dx: f64, dy: f64) {
    pointer::moved(dx as f32, dy as f32);
}

/// A mouse button went down or up. `number` is the AppKit button number: 0 left, 1 right, 2 middle, 3 and 4 the
/// side buttons.
pub fn mouse_button(number: u8, down: bool) {
    pointer::button(number, down);
}

/// The wheel turned by `dy`, positive away from the user. A notch is a short press of its binding.
pub fn mouse_wheel(dy: f64) {
    pointer::wheel(dy as f32);
}

/// The window let go of the mouse: every button is up and the camera stops.
pub fn mouse_release() {
    pointer::with(pointer::Pointer::release);
}

/// The display tick. It looks up the controllers, renews the rumble pulses and writes the HID shared memory when a
/// game has asked for it.
pub fn tick() {
    with(|input| {
        input.tick();
        let _ = shmem::fill(input);
    });
}

/// Sets the touch points that are down now, in coordinates from 0 to 1 over the screen. The host calls it when the
/// touches change. Only the handheld mode reports them to the game.
pub fn set_touches(points: &[Touch]) {
    with(|input| input.set_touches(points));
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

    use nxkit_abi::guest;

    const WORDS: usize = 0x10000;
    const BYTES: u64 = WORDS as u64 * 8;

    static LOCK: Mutex<()> = Mutex::new(());

    /// A zeroed block of guest memory of 0x80000 bytes that the guest range points at. The guard keeps other tests from
    /// using the range at the same time.
    pub(crate) fn guest_memory() -> (u64, MutexGuard<'static, ()>) {
        static BLOCK: OnceLock<u64> = OnceLock::new();
        let guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let base =
            *BLOCK.get_or_init(|| Box::leak(vec![0u64; WORDS].into_boxed_slice()).as_ptr() as u64);
        guest::set_range(base, BYTES);
        let _ = guest::fill(base, BYTES, 0);
        (base, guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::guest_memory;

    #[test]
    fn the_service_is_named_hid_and_has_exports() {
        assert_eq!(services().len(), 1);
        assert!(exports().len() > 100);
        assert!(big_stack_exports().is_empty());
    }

    #[test]
    fn tick_without_shared_memory_does_nothing() {
        let (_base, _guard) = guest_memory();
        tick();
    }

    #[test]
    fn mouse_events_are_accepted_at_any_time() {
        mouse_moved(3.0, -2.0);
        mouse_button(0, true);
        mouse_wheel(1.0);
        mouse_release();
        assert!(bind_errors(&[("f".to_owned(), "zr".to_owned())]).is_empty());
        assert_eq!(bind_errors(&[("f".to_owned(), "jump".to_owned())]).len(), 1);
    }

    #[test]
    fn touches_are_accepted_at_any_time() {
        set_touches(&[Touch {
            id: 0,
            x: 0.5,
            y: 0.5,
        }]);
        set_touches(&[]);
    }
}
