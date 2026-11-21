//! The look of NX2Apple's console mode for the host's own windows: the tokens of NX2Apple's
//! `design-tokens/tokens.json` and the views every screen is made of (the Home screens in `homeviews.rs`, the loading
//! screen in `loading.rs`).

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBox, NSBoxType, NSColor, NSFont, NSTextField, NSTitlePosition,
    NSView,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;

/// The colours of NX2Apple's tokens (`color.*`), as `0xRRGGBB` and an alpha.
pub(super) mod tokens {
    /// `color.background`: the page, and the scrim at 70 %.
    pub(in crate::sys) const BACKGROUND: u32 = 0x0E0E0F;
    /// `color.surface`: a card.
    pub(in crate::sys) const SURFACE: u32 = 0x1A1A1C;
    /// `color.accent`: the cream of the one primary action, of a selection and of the focus.
    pub(in crate::sys) const ACCENT: u32 = 0xFFF0D6;
    /// `color.accentOn`: text and the focus ring on the cream.
    pub(in crate::sys) const ACCENT_ON: u32 = 0x000000;
    /// White, for the text and the quiet fills at their alphas.
    pub(in crate::sys) const WHITE: u32 = 0xFFFFFF;
    /// The dim over the blurred game: lighter than the console's 0.7 over a page, as the blur already calms the
    /// picture.
    pub(in crate::sys) const SCRIM: f64 = 0.45;
    /// `color.text.primary`.
    pub(in crate::sys) const TEXT_PRIMARY: f64 = 0.90;
    /// `color.text.secondary`.
    pub(in crate::sys) const TEXT_SECONDARY: f64 = 0.60;
    /// The fill of a quiet capsule.
    pub(in crate::sys) const QUIET_FILL: f64 = 0.09;
    /// The label of a quiet capsule.
    pub(in crate::sys) const QUIET_TEXT: f64 = 0.90;
    /// The card's hairline.
    pub(in crate::sys) const HAIRLINE: f64 = 0.10;
    /// `LauncherPalette.focusRingWidth`.
    pub(in crate::sys) const FOCUS_RING: f64 = 2.0;
    /// `motion.standard`, in seconds.
    pub(in crate::sys) const MOTION: f64 = 0.2;
}

/// A colour of the tokens.
pub(super) fn rgb(hex: u32, alpha: f64) -> Retained<NSColor> {
    let c = |shift: u32| f64::from((hex >> shift) & 0xFF) / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(c(16), c(8), c(0), alpha)
}

/// A frame with its edges on whole points.
pub(super) fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(x.round(), y.round()),
        CGSize::new(w.round(), h.round()),
    )
}

/// The view follows the size of its superview.
pub(super) fn fills_parent(view: &NSView) {
    view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
}

/// A label of `text`: wrapped to `width`, or on one line. Its size comes with it.
pub(super) fn label(
    mtm: MainThreadMarker,
    text: &str,
    font: &NSFont,
    color: &NSColor,
    width: f64,
    wrap: bool,
) -> (Retained<NSTextField>, CGSize) {
    let s = NSString::from_str(text);
    let t = if wrap {
        NSTextField::wrappingLabelWithString(&s, mtm)
    } else {
        NSTextField::labelWithString(&s, mtm)
    };
    t.setFont(Some(font));
    t.setTextColor(Some(color));
    if wrap {
        t.setPreferredMaxLayoutWidth(width);
    }
    let size = t.fittingSize();
    (t, CGSize::new(size.width.min(width), size.height))
}

/// A rounded box of `fill` with a border of `border` (none at width 0).
pub(super) fn surface(
    mtm: MainThreadMarker,
    frame: CGRect,
    fill: &NSColor,
    radius: f64,
    border: (&NSColor, f64),
) -> Retained<NSBox> {
    let b = NSBox::new(mtm);
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setFillColor(fill);
    b.setBorderColor(border.0);
    b.setBorderWidth(border.1);
    b.setCornerRadius(radius);
    b.setFrame(frame);
    b
}
