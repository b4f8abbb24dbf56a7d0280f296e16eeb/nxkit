//! The display modes of a screen: the refresh rate it shows now, and a switch to another rate.
//!
//! `NSScreen.maximumFramesPerSecond` is the highest rate the panel can take. It is not the rate of the current mode,
//! and a screen that adapts its rate (ProMotion) reports 0 for the mode. This module asks CoreGraphics for the mode
//! that the display is in. [`switch_refresh`] changes the refresh rate to an even multiple of the frame rate of the
//! game, for the time of the run: the switch is for this application only, so the system puts the old mode back when
//! the process ends, also after a crash. [`ModeSwitch::restore`] puts it back earlier.

use std::ffi::c_void;
use std::sync::Mutex;

type DisplayId = u32;
type ModeRef = *const c_void;
type CfArray = *const c_void;
type ConfigRef = *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayCopyDisplayMode(display: DisplayId) -> ModeRef;
    fn CGDisplayCopyAllDisplayModes(display: DisplayId, options: *const c_void) -> CfArray;
    fn CGDisplayModeGetRefreshRate(mode: ModeRef) -> f64;
    fn CGDisplayModeGetWidth(mode: ModeRef) -> usize;
    fn CGDisplayModeGetHeight(mode: ModeRef) -> usize;
    fn CGDisplayModeGetPixelWidth(mode: ModeRef) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: ModeRef) -> usize;
    fn CGDisplayModeIsUsableForDesktopGUI(mode: ModeRef) -> bool;
    fn CGDisplayModeRetain(mode: ModeRef) -> ModeRef;
    fn CGDisplayModeRelease(mode: ModeRef);
    fn CGBeginDisplayConfiguration(config: *mut ConfigRef) -> i32;
    fn CGConfigureDisplayWithDisplayMode(
        config: ConfigRef,
        display: DisplayId,
        mode: ModeRef,
        options: *const c_void,
    ) -> i32;
    fn CGCompleteDisplayConfiguration(config: ConfigRef, option: u32) -> i32;
    fn CGCancelDisplayConfiguration(config: ConfigRef) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: CfArray) -> isize;
    fn CFArrayGetValueAtIndex(array: CfArray, index: isize) -> *const c_void;
    fn CFRelease(object: *const c_void);
}

/// `kCGConfigureForAppOnly`: the change holds for this process and ends with it.
const FOR_APP_ONLY: u32 = 0;

/// What one display mode looks like. The refresh rate is 0 on a screen that adapts it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModeInfo {
    /// The width in points.
    pub width: usize,
    /// The height in points.
    pub height: usize,
    /// The width in pixels.
    pub pixel_width: usize,
    /// The height in pixels.
    pub pixel_height: usize,
    /// The refresh rate in Hz, 0 when the mode adapts it.
    pub hz: f64,
    /// Whether the desktop can use the mode.
    pub usable: bool,
}

/// An owned `CGDisplayModeRef`.
struct Mode(ModeRef);

// SAFETY: a `CGDisplayMode` is an immutable Core Foundation object whose reference count is atomic.
unsafe impl Send for Mode {}

impl Mode {
    /// The current mode of `display`.
    fn current(display: DisplayId) -> Option<Mode> {
        // SAFETY: the call takes an id and returns a retained mode or null.
        let m = unsafe { CGDisplayCopyDisplayMode(display) };
        (!m.is_null()).then_some(Mode(m))
    }

    fn info(&self) -> ModeInfo {
        // SAFETY: `self.0` is a live mode, which `Mode` owns.
        unsafe {
            ModeInfo {
                width: CGDisplayModeGetWidth(self.0),
                height: CGDisplayModeGetHeight(self.0),
                pixel_width: CGDisplayModeGetPixelWidth(self.0),
                pixel_height: CGDisplayModeGetPixelHeight(self.0),
                hz: CGDisplayModeGetRefreshRate(self.0),
                usable: CGDisplayModeIsUsableForDesktopGUI(self.0),
            }
        }
    }
}

impl Drop for Mode {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a mode that `Mode` retained and releases once.
        unsafe { CGDisplayModeRelease(self.0) };
    }
}

/// All modes of `display`, retained.
fn all_modes(display: DisplayId) -> Vec<Mode> {
    // SAFETY: a null options dictionary asks for the default list. The call returns a retained array or null.
    let list = unsafe { CGDisplayCopyAllDisplayModes(display, std::ptr::null()) };
    if list.is_null() {
        return Vec::new();
    }
    // SAFETY: `list` is a live array. Each value is a mode that the array owns; `CGDisplayModeRetain` gives us our
    // own reference, which `Mode` releases. The array is released once, after the loop.
    let modes = unsafe {
        (0..CFArrayGetCount(list))
            .map(|i| CGDisplayModeRetain(CFArrayGetValueAtIndex(list, i)))
            .filter(|m| !m.is_null())
            .map(Mode)
            .collect()
    };
    // SAFETY: `list` came from a `Copy` function and is released once.
    unsafe { CFRelease(list) };
    modes
}

/// The refresh rate of the mode that `display` is in, in Hz. `None` when the system gives no mode or the mode adapts
/// its rate (the rate is then 0).
pub fn current_refresh_hz(display: u32) -> Option<f64> {
    let hz = Mode::current(display)?.info().hz;
    (hz.is_finite() && hz > 0.0).then_some(hz)
}

/// The index in `modes` of the mode to switch to for `want_hz`: the same size and pixels as `current`, usable by the
/// desktop, with the rate nearest to `want_hz` and at most 1 Hz away.
pub fn pick_mode(modes: &[ModeInfo], current: &ModeInfo, want_hz: f64) -> Option<usize> {
    modes
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.usable
                && (m.width, m.height, m.pixel_width, m.pixel_height)
                    == (current.width, current.height, current.pixel_width, current.pixel_height)
                && (m.hz - want_hz).abs() <= 1.0
        })
        .min_by(|a, b| (a.1.hz - want_hz).abs().total_cmp(&(b.1.hz - want_hz).abs()))
        .map(|(i, _)| i)
}

/// The refresh rates of the modes that `display` has at the size of its current mode, ascending and without repeats.
pub fn refresh_choices(display: u32) -> Vec<f64> {
    let Some(current) = Mode::current(display).map(|m| m.info()) else {
        return Vec::new();
    };
    let mut hz: Vec<f64> = all_modes(display)
        .iter()
        .map(Mode::info)
        .filter(|m| {
            m.usable
                && (m.width, m.height, m.pixel_width, m.pixel_height)
                    == (current.width, current.height, current.pixel_width, current.pixel_height)
                && m.hz > 0.0
        })
        .map(|m| m.hz)
        .collect();
    hz.sort_by(f64::total_cmp);
    hz.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    hz
}

/// The rate among `choices` that is an even multiple of `target` and nearest to `near`, when there is one. A rate
/// within 0.5% of a multiple counts (119.88 for 60).
pub fn even_choice(choices: &[f64], target: u32, near: f64) -> Option<f64> {
    let t = f64::from(target.max(1));
    choices
        .iter()
        .copied()
        .filter(|hz| {
            let n = (hz / t).round();
            n >= 1.0 && (hz / t - n).abs() <= 0.005 * n
        })
        .min_by(|a, b| (a - near).abs().total_cmp(&(b - near).abs()))
}

/// A refresh rate switch of a display. The old mode comes back on [`ModeSwitch::restore`], on drop, and when the
/// process ends.
pub struct ModeSwitch {
    display: DisplayId,
    old: Option<Mode>,
    /// The rate the display was switched to, in Hz.
    pub to_hz: f64,
    /// The rate the display had before, in Hz.
    pub from_hz: f64,
}

impl ModeSwitch {
    /// Puts the old mode back. A second call does nothing.
    pub fn restore(&mut self) {
        if let Some(old) = self.old.take() {
            let _ = configure(self.display, &old);
        }
    }
}

impl Drop for ModeSwitch {
    fn drop(&mut self) {
        self.restore();
    }
}

fn configure(display: DisplayId, mode: &Mode) -> Result<(), String> {
    let mut config: ConfigRef = std::ptr::null_mut();
    // SAFETY: `config` is a valid out pointer. The configuration is completed or cancelled on every path below.
    let err = unsafe { CGBeginDisplayConfiguration(&mut config) };
    if err != 0 {
        return Err(format!("CGBeginDisplayConfiguration failed ({err})"));
    }
    // SAFETY: `config` is the open configuration, `mode` is a live mode of this display, and null options are valid.
    let err = unsafe { CGConfigureDisplayWithDisplayMode(config, display, mode.0, std::ptr::null()) };
    if err != 0 {
        // SAFETY: `config` is open and not completed.
        unsafe { CGCancelDisplayConfiguration(config) };
        return Err(format!("CGConfigureDisplayWithDisplayMode failed ({err})"));
    }
    // SAFETY: `config` is open. Completing it consumes it.
    let err = unsafe { CGCompleteDisplayConfiguration(config, FOR_APP_ONLY) };
    if err != 0 {
        return Err(format!("CGCompleteDisplayConfiguration failed ({err})"));
    }
    Ok(())
}

/// Switches `display` to the mode with the refresh rate `want_hz` at the size it has now. `Ok(None)` when it shows
/// that rate already. The switch is for this process only.
pub fn switch_refresh(display: u32, want_hz: f64) -> Result<Option<ModeSwitch>, String> {
    let old = Mode::current(display).ok_or("the display has no current mode")?;
    let cur = old.info();
    if cur.hz > 0.0 && (cur.hz - want_hz).abs() <= 0.5 {
        return Ok(None);
    }
    let modes = all_modes(display);
    let infos: Vec<ModeInfo> = modes.iter().map(Mode::info).collect();
    let i = pick_mode(&infos, &cur, want_hz)
        .ok_or_else(|| format!("the display has no mode of {want_hz} Hz at {}x{}", cur.width, cur.height))?;
    let to = modes.get(i).ok_or("mode index")?;
    configure(display, to)?;
    Ok(Some(ModeSwitch { display, old: Some(old), to_hz: infos.get(i).map_or(want_hz, |m| m.hz), from_hz: cur.hz }))
}

/// The switch that the host made, kept so that the exit path can restore it.
static ACTIVE: Mutex<Option<ModeSwitch>> = Mutex::new(None);

/// Keeps `switch` until [`restore_active`]. An earlier one is restored first.
pub fn keep_active(switch: ModeSwitch) {
    let mut g = ACTIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    *g = Some(switch);
}

/// Puts the mode back that [`keep_active`] holds. The host calls it when the application ends.
pub fn restore_active() {
    let mut g = ACTIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(mut s) = g.take() {
        s.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(w: usize, hz: f64) -> ModeInfo {
        ModeInfo { width: w, height: 1440, pixel_width: w, pixel_height: 1440, hz, usable: true }
    }

    #[test]
    fn the_nearest_mode_of_the_same_size_wins() {
        let cur = mode(2560, 144.0);
        let modes = [mode(1920, 120.0), mode(2560, 60.0), mode(2560, 119.88), mode(2560, 120.0), mode(2560, 144.0)];
        assert_eq!(pick_mode(&modes, &cur, 120.0), Some(3));
        assert_eq!(pick_mode(&modes, &cur, 100.0), None);
    }

    #[test]
    fn an_unusable_mode_is_not_picked() {
        let cur = mode(2560, 144.0);
        let mut m = mode(2560, 120.0);
        m.usable = false;
        assert_eq!(pick_mode(&[m], &cur, 120.0), None);
    }

    #[test]
    fn an_even_choice_is_a_multiple_of_the_target() {
        let hz = [50.0, 60.0, 100.0, 119.88, 144.0];
        assert_eq!(even_choice(&hz, 60, 144.0), Some(119.88));
        assert_eq!(even_choice(&hz, 60, 50.0), Some(60.0));
        assert_eq!(even_choice(&[50.0, 144.0], 60, 144.0), None);
        assert_eq!(even_choice(&hz, 48, 144.0), Some(144.0));
    }

    #[test]
    fn the_main_display_has_a_mode() {
        // The id of the main display is 1 on every Mac that has one; CI has none, so the answer may be `None`.
        let _ = current_refresh_hz(1);
        let _ = refresh_choices(1);
    }
}
