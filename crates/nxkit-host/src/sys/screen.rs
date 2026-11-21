//! The screens: the one the window opens on (`NXKIT_SCREEN`), its facts for the refresh plan and the display link rate.

use nxkit_metal::{FrameInterval, LinkRate};
use objc2::MainThreadMarker;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSScreen, NSWindow};
use objc2_foundation::NSString;

use crate::refresh::{Refresh, ScreenFacts};

/// The screen of the window: `NXKIT_SCREEN` names it by its index in the screen list (0 is the built-in one), by `id:`
/// and its CoreGraphics display id (`NSScreenNumber`, what a launcher knows of the screen it is on), or by a part of
/// its name, for example `XB273U`. Without it, or when nothing matches, it is the main screen.
pub(super) fn target_screen(mtm: MainThreadMarker) -> Option<Retained<NSScreen>> {
    if let Some(want) = crate::hostenv::host().screen.clone() {
        let screens = NSScreen::screens(mtm);
        let by_id = want
            .strip_prefix("id:")
            .map(|id| id.trim().parse::<u32>().ok());
        let found = match (by_id, want.parse::<usize>()) {
            (Some(id), _) => id.and_then(|id| screens.iter().find(|s| display_id(s) == Some(id))),
            (None, Ok(i)) if i < screens.count() => Some(screens.objectAtIndex(i)),
            (None, Ok(_)) => None,
            (None, Err(_)) => {
                let want = want.to_lowercase();
                screens
                    .iter()
                    .find(|s| s.localizedName().to_string().to_lowercase().contains(&want))
            }
        };
        match found {
            Some(s) => return Some(s),
            None => eprintln!(
                "nxkit-host: NXKIT_SCREEN={want} matches no screen, using the main screen"
            ),
        }
    }
    NSScreen::mainScreen(mtm)
}

/// The CoreGraphics display id of a screen (`NSScreenNumber` of its device description).
pub(super) fn display_id(screen: &NSScreen) -> Option<u32> {
    // SAFETY: `deviceDescription` returns a dictionary and `objectForKey:` an optional `NSNumber`, both retained and
    // valid for the calls below. `unsignedIntValue` is a method of `NSNumber`.
    unsafe {
        let desc: Retained<AnyObject> = msg_send![screen, deviceDescription];
        let key = NSString::from_str("NSScreenNumber");
        let number: Option<Retained<AnyObject>> = msg_send![&*desc, objectForKey: &*key];
        let id: u32 = msg_send![&*number?, unsignedIntValue];
        Some(id)
    }
}

/// What the host reads of a screen. A screen whose refresh interval can change (ProMotion, adaptive sync) has a longest
/// interval above its shortest. `NSScreen` documents both as equal for a screen that cannot.
pub(super) fn screen_facts(s: &NSScreen) -> ScreenFacts {
    let (min, max) = (s.minimumRefreshInterval(), s.maximumRefreshInterval());
    ScreenFacts {
        name: s.localizedName().to_string(),
        display: display_id(s),
        max_fps: u32::try_from(s.maximumFramesPerSecond()).unwrap_or(0),
        adaptive: min > 0.0 && max > min * 1.01,
    }
}

/// The display link rate for the screen that shows the window. Before the window is on a screen it is the screen
/// that `NXKIT_SCREEN` names, else the main one. The link rate is the refresh rate of the current mode of that screen
/// (`refresh.rs`), not its highest one: the game's own pacing picks the frames it renders, and the link ticks at a rate
/// that holds each frame an exact number of ticks. On an adaptive screen with a variable frame step it is a range
/// (variable refresh, `crate::refresh::choose_presentation`).
pub(super) fn screen_interval(
    mtm: MainThreadMarker,
    window: Option<&NSWindow>,
    refresh: &Refresh,
) -> LinkRate {
    match window
        .and_then(|w| w.screen())
        .or_else(|| target_screen(mtm))
    {
        Some(s) => {
            let plan = refresh.apply(&screen_facts(&s));
            // On an adaptive screen with a variable frame step the link gets a range, not one rate.
            match refresh.vrr_range() {
                Some(range) => LinkRate::Variable(range),
                None => LinkRate::Fixed(FrameInterval::from_hz(plan.link_hz)),
            }
        }
        None => LinkRate::Fixed(FrameInterval::Hz60),
    }
}

pub(super) fn content_size(mtm: MainThreadMarker, size: Option<(f64, f64)>) -> (f64, f64) {
    if let Some(s) = size {
        return s;
    }
    match target_screen(mtm) {
        Some(s) => {
            let f = s.visibleFrame();
            (f.size.width, f.size.height)
        }
        None => (1280.0, 720.0),
    }
}
