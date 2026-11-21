//! The views of the Home menu and the quit prompt (`home.rs`), drawn over the game in the look of NX2Apple's console
//! mode: the question card of `ConsolePages.swift` (`ConsoleQuestion`) with the tokens of NX2Apple's
//! `design-tokens/tokens.json`. A dim scrim in the page colour, a card of the surface colour with a hairline, a cream
//! symbol, the title and the note in Manrope, and capsules: the one primary action cream, the others a quiet white.
//! The controller's focus is a ring of 2 points, cream (black on the cream capsule), as `PadFocus` draws it. A window
//! narrower than 650 points gets the compact sizes of the console mode.
//!
//! The views are AppKit views, so they draw while the game presents. The scrim is flipped, so the layout goes down from
//! the top, and it keeps the mouse from the game's view.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{Ivars, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAnimationContext, NSAutoresizingMaskOptions, NSBezierPath, NSBox, NSBoxType, NSColor,
    NSEvent, NSFontWeightLight, NSImage, NSImageSymbolConfiguration, NSImageView, NSLineBreakMode,
    NSLineCapStyle, NSResponder, NSTextAlignment, NSTextField, NSTitlePosition, NSView,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSObject, NSString};

use super::font::{self, Weight};

/// The colours of NX2Apple's tokens (`color.*`), as `0xRRGGBB` and an alpha.
mod tokens {
    /// `color.background`: the page, and the scrim at 70 %.
    pub(super) const BACKGROUND: u32 = 0x0E0E0F;
    /// `color.surface`: a card.
    pub(super) const SURFACE: u32 = 0x1A1A1C;
    /// `color.accent`: the cream of the one primary action, of a selection and of the focus.
    pub(super) const ACCENT: u32 = 0xFFF0D6;
    /// `color.accentOn`: text and the focus ring on the cream.
    pub(super) const ACCENT_ON: u32 = 0x000000;
    /// White, for the text and the quiet fills at their alphas.
    pub(super) const WHITE: u32 = 0xFFFFFF;
    /// The scrim over the game (`ConsoleView`: the background at 0.7).
    pub(super) const SCRIM: f64 = 0.7;
    /// `color.text.primary`.
    pub(super) const TEXT_PRIMARY: f64 = 0.90;
    /// `color.text.secondary`.
    pub(super) const TEXT_SECONDARY: f64 = 0.60;
    /// The fill of a quiet capsule.
    pub(super) const QUIET_FILL: f64 = 0.09;
    /// The label of a quiet capsule.
    pub(super) const QUIET_TEXT: f64 = 0.90;
    /// The card's hairline.
    pub(super) const HAIRLINE: f64 = 0.10;
    /// `LauncherPalette.focusRingWidth`.
    pub(super) const FOCUS_RING: f64 = 2.0;
    /// `motion.standard`, in seconds.
    pub(super) const MOTION: f64 = 0.2;
}

fn rgb(hex: u32, alpha: f64) -> Retained<NSColor> {
    let c = |shift: u32| f64::from((hex >> shift) & 0xFF) / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(c(16), c(8), c(0), alpha)
}

/// The sizes of `ConsoleQuestion`, wide or compact.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Look {
    pub(super) compact: bool,
}

impl Look {
    /// The console mode is compact below 650 points of width.
    pub(super) fn for_width(width: f64) -> Look {
        Look {
            compact: width < 650.0,
        }
    }
    fn pick(self, wide: f64, compact: f64) -> f64 {
        if self.compact { compact } else { wide }
    }
    fn card_width(self, room: f64) -> f64 {
        let free = (room - 48.0).max(200.0);
        if self.compact { free } else { free.min(560.0) }
    }
    fn padding(self) -> f64 {
        self.pick(36.0, 24.0)
    }
    fn spacing(self) -> f64 {
        self.pick(20.0, 16.0)
    }
    fn symbol(self) -> f64 {
        self.pick(42.0, 34.0)
    }
    fn title(self) -> f64 {
        self.pick(34.0, 26.0)
    }
    fn message(self) -> f64 {
        self.pick(20.0, 16.0)
    }
    fn button_text(self) -> f64 {
        self.pick(20.0, 18.0)
    }
    fn button_height(self) -> f64 {
        self.pick(58.0, 52.0)
    }
    fn buttons_top(self) -> f64 {
        self.pick(12.0, 8.0)
    }
}

define_class!(
    // SAFETY:
    // - `NSView` has no subclassing requirements beyond calling its designated initialiser, which `Scrim::new` does.
    // - `MainThreadOnly` is correct, because AppKit uses views on the main thread only.
    // - `Scrim` does not implement `Drop`.
    /// The dim layer over the game. It takes the mouse, so a click does not reach the game's view.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxkitHomeScrim"]
    pub(super) struct Scrim;

    impl Scrim {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, rect: CGRect) {
            rgb(tokens::BACKGROUND, tokens::SCRIM).setFill();
            NSBezierPath::fillRect(rect);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _e: &NSEvent) {}

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _e: &NSEvent) {}

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _e: &NSEvent) {}

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _e: &NSEvent) {}

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, _e: &NSEvent) {}

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, _e: &NSEvent) {}
    }
);

define_class!(
    // SAFETY: as `Scrim`.
    /// A flipped layer that the mouse goes through: the hold indicator.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxkitHomePane"]
    struct Pane;

    impl Pane {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(hitTest:))]
        fn hit_test(&self, _point: CGPoint) -> *mut NSView {
            std::ptr::null_mut()
        }
    }
);

define_class!(
    // SAFETY: as `Scrim`. The ivar is a `Cell` read and written on the main thread only.
    /// The ring of the hold indicator: a track, and the cream arc from the top, clockwise, as full as the hold is.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxkitHomeRing"]
    pub(super) struct HoldRing {
        fill: Cell<f64>,
    }

    impl HoldRing {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _rect: CGRect) {
            let b = self.bounds();
            let line = 3.5;
            let r = b.size.width.min(b.size.height) / 2.0 - line / 2.0;
            let centre = CGPoint::new(b.size.width / 2.0, b.size.height / 2.0);
            let track = NSBezierPath::bezierPathWithOvalInRect(CGRect::new(
                CGPoint::new(centre.x - r, centre.y - r),
                CGSize::new(2.0 * r, 2.0 * r),
            ));
            track.setLineWidth(line);
            rgb(tokens::WHITE, 0.18).setStroke();
            track.stroke();
            let fill = self.fill().get().clamp(0.0, 1.0);
            if fill > 0.0 {
                let arc = NSBezierPath::bezierPath();
                arc.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
                    centre,
                    r,
                    90.0,
                    90.0 - 360.0 * fill,
                    true,
                );
                arc.setLineWidth(line);
                arc.setLineCapStyle(NSLineCapStyle::Round);
                rgb(tokens::ACCENT, 1.0).setStroke();
                arc.stroke();
            }
        }
    }
);

impl Scrim {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        // SAFETY: `initWithFrame:` is the designated initialiser of `NSView`, called once on a fresh instance.
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }
}

impl Pane {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        // SAFETY: as `Scrim::new`.
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }
}

impl HoldRing {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars::<HoldRing> {
            fill: Cell::new(0.0),
        });
        // SAFETY: as `Scrim::new`, on an instance whose ivars are set.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// Fills the ring to `fill` (0 to 1).
    pub(super) fn set(&self, fill: f64) {
        if (self.fill().get() - fill).abs() > 0.001 {
            self.fill().set(fill);
            self.setNeedsDisplay(true);
        }
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(x.round(), y.round()),
        CGSize::new(w.round(), h.round()),
    )
}

fn fills_parent(view: &NSView) {
    view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
}

/// A label of `text`: wrapped to `width`, or on one line. Its size comes with it.
fn label(
    mtm: MainThreadMarker,
    text: &str,
    font: &objc2_app_kit::NSFont,
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
fn surface(
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

/// A capsule with its label centred, cream when `primary`, with the focus ring when `focused`. The label gets a
/// smaller font, down to 80 %, then is cut, so every capsule keeps its width (`minimumScaleFactor(0.8)`).
fn capsule(
    mtm: MainThreadMarker,
    parent: &NSView,
    frame: CGRect,
    text: &str,
    primary: bool,
    focused: bool,
    size: f64,
) {
    let (fill, fg, ring) = if primary {
        (
            rgb(tokens::ACCENT, 1.0),
            rgb(tokens::BACKGROUND, 1.0),
            rgb(tokens::ACCENT_ON, 1.0),
        )
    } else {
        (
            rgb(tokens::WHITE, tokens::QUIET_FILL),
            rgb(tokens::WHITE, tokens::QUIET_TEXT),
            rgb(tokens::ACCENT, 1.0),
        )
    };
    let width = if focused { tokens::FOCUS_RING } else { 0.0 };
    let b = surface(mtm, frame, &fill, frame.size.height / 2.0, (&ring, width));
    parent.addSubview(&b);
    let room = frame.size.width - 32.0;
    let mut f = font::manrope(size, Weight::Bold);
    let (_, measured) = label(mtm, text, &f, &fg, f64::MAX, false);
    if measured.width > room {
        f = font::manrope((size * room / measured.width).max(size * 0.8), Weight::Bold);
    }
    let (t, s) = label(mtm, text, &f, &fg, room, false);
    t.setAlignment(NSTextAlignment::Center);
    t.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    let y = frame.origin.y + (frame.size.height - s.height) / 2.0;
    t.setFrame(rect(frame.origin.x + 16.0, y, room, s.height));
    parent.addSubview(&t);
}

/// The SF Symbol `name` at `size` points, light, in the cream.
fn symbol(mtm: MainThreadMarker, name: &str, size: f64) -> Option<(Retained<NSImageView>, CGSize)> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        None,
    )?;
    // SAFETY: `NSFontWeightLight` is a constant of AppKit.
    let weight = unsafe { NSFontWeightLight };
    let conf = NSImageSymbolConfiguration::configurationWithPointSize_weight(size, weight);
    let image = image.imageWithSymbolConfiguration(&conf)?;
    let s = image.size();
    let view = NSImageView::imageViewWithImage(&image, mtm);
    view.setContentTintColor(Some(&rgb(tokens::ACCENT, 1.0)));
    Some((view, s))
}

/// What a card shows.
pub(super) struct Card<'a> {
    /// The SF Symbol at the top.
    pub(super) symbol: &'a str,
    /// The title.
    pub(super) title: &'a str,
    /// The note under the title, if any.
    pub(super) message: Option<&'a str>,
    /// The actions with their label and whether each is the primary one, in the order shown.
    pub(super) actions: &'a [(&'a str, bool)],
    /// The actions are rows (the menu), not side by side (a question).
    pub(super) rows: bool,
    /// The index of the focused action.
    pub(super) focused: usize,
}

/// The scrim over `bounds` and the card in its middle.
pub(super) fn card(mtm: MainThreadMarker, bounds: CGRect, c: &Card<'_>) -> Retained<NSView> {
    let look = Look::for_width(bounds.size.width);
    let scrim = Scrim::new(mtm, bounds);
    fills_parent(&scrim);
    let width = look.card_width(bounds.size.width);
    let (pad, gap) = (look.padding(), look.spacing());
    let inner = width - 2.0 * pad;
    let sym = symbol(mtm, c.symbol, look.symbol());
    let (title, ts) = label(
        mtm,
        c.title,
        &font::manrope(look.title(), Weight::Bold),
        &rgb(tokens::WHITE, tokens::TEXT_PRIMARY),
        inner,
        true,
    );
    let message = c.message.map(|m| {
        label(
            mtm,
            m,
            &font::manrope(look.message(), Weight::Regular),
            &rgb(tokens::WHITE, tokens::TEXT_SECONDARY),
            inner,
            true,
        )
    });
    let bh = look.button_height();
    let n = c.actions.len() as f64;
    let actions_h = if c.rows {
        n * bh + (n - 1.0).max(0.0) * 12.0
    } else {
        bh
    };
    let sym_h = sym.as_ref().map_or(0.0, |(_, s)| s.height + gap);
    let msg_h = message.as_ref().map_or(0.0, |(_, s)| gap + s.height);
    let height = pad + sym_h + ts.height + msg_h + gap + look.buttons_top() + actions_h + pad;
    let x0 = ((bounds.size.width - width) / 2.0).max(0.0);
    let y0 = ((bounds.size.height - height) / 2.0).max(0.0);
    let hairline = rgb(tokens::WHITE, tokens::HAIRLINE);
    let frame = rect(x0, y0, width, height);
    scrim.addSubview(&surface(
        mtm,
        frame,
        &rgb(tokens::SURFACE, 1.0),
        24.0,
        (&hairline, 1.0),
    ));
    let (x, mut y) = (x0 + pad, y0 + pad);
    if let Some((view, s)) = sym {
        view.setFrame(rect(x, y, s.width, s.height));
        scrim.addSubview(&view);
        y += s.height + gap;
    }
    title.setFrame(rect(x, y, inner, ts.height));
    scrim.addSubview(&title);
    y += ts.height;
    if let Some((m, s)) = message {
        y += gap;
        m.setFrame(rect(x, y, inner, s.height));
        scrim.addSubview(&m);
        y += s.height;
    }
    y += gap + look.buttons_top();
    let each = if c.rows {
        inner
    } else {
        (inner - 14.0 * (n - 1.0)) / n
    };
    for (i, (text, primary)) in c.actions.iter().enumerate() {
        let (ax, ay) = if c.rows {
            (x, y + i as f64 * (bh + 12.0))
        } else {
            (x + i as f64 * (each + 14.0), y)
        };
        let f = rect(ax, ay, each, bh);
        capsule(
            mtm,
            &scrim,
            f,
            text,
            *primary,
            i == c.focused,
            look.button_text(),
        );
    }
    Retained::into_super(scrim)
}

/// The hold indicator at the top of `bounds`: a pill with the ring and its label (`text`).
pub(super) fn hold(
    mtm: MainThreadMarker,
    bounds: CGRect,
    text: &str,
) -> (Retained<NSView>, Retained<HoldRing>) {
    let pane = Pane::new(mtm, bounds);
    fills_parent(&pane);
    let (h, d) = (56.0, 26.0);
    let (t, s) = label(
        mtm,
        text,
        &font::manrope(20.0, Weight::Semibold),
        &rgb(tokens::WHITE, tokens::TEXT_PRIMARY),
        bounds.size.width,
        false,
    );
    let w = 18.0 + d + 12.0 + s.width + 24.0;
    let (x, y) = ((bounds.size.width - w) / 2.0, 40.0);
    let border = rgb(tokens::WHITE, 0.12);
    let pill = surface(
        mtm,
        rect(x, y, w, h),
        &rgb(tokens::BACKGROUND, 0.92),
        h / 2.0,
        (&border, 1.0),
    );
    pane.addSubview(&pill);
    let ring = HoldRing::new(mtm, rect(x + 18.0, y + (h - d) / 2.0, d, d));
    pane.addSubview(&ring);
    t.setFrame(rect(
        x + 18.0 + d + 12.0,
        y + (h - s.height) / 2.0,
        s.width + 2.0,
        s.height,
    ));
    pane.addSubview(&t);
    (Retained::into_super(pane), ring)
}

/// Shows `view` over `motion.standard`.
pub(super) fn fade_in(view: &NSView) {
    view.setAlphaValue(0.0);
    animate(view, 1.0);
}

/// Hides `view` over `motion.standard`, then takes it out of its superview.
pub(super) fn fade_out(view: &NSView) {
    animate(view, 0.0);
    let none: Option<&AnyObject> = None;
    // SAFETY: `performSelector:withObject:afterDelay:` of `NSObject` on the main thread; `removeFromSuperview` takes no
    // argument (the nil object is ignored). The run loop retains the view until then.
    unsafe {
        let _: () = msg_send![
            view,
            performSelector: sel!(removeFromSuperview),
            withObject: none,
            afterDelay: tokens::MOTION + 0.02
        ];
    }
}

fn animate(view: &NSView, alpha: f64) {
    NSAnimationContext::beginGrouping();
    NSAnimationContext::currentContext().setDuration(tokens::MOTION);
    // SAFETY: `animator` of `NSView` (`NSAnimatablePropertyContainer`) returns a proxy of the view.
    let proxy: Retained<NSView> = unsafe { msg_send![view, animator] };
    proxy.setAlphaValue(alpha);
    NSAnimationContext::endGrouping();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_below_650_points() {
        assert!(Look::for_width(640.0).compact);
        assert!(!Look::for_width(1920.0).compact);
        assert_eq!(Look::for_width(1920.0).card_width(1920.0), 560.0);
        assert_eq!(Look::for_width(400.0).card_width(400.0), 352.0);
    }

    #[test]
    fn the_tokens_make_the_colours() {
        let c = rgb(tokens::ACCENT, 1.0);
        assert!((c.redComponent() - 1.0).abs() < 1e-6);
        assert!((c.blueComponent() - 214.0 / 255.0).abs() < 1e-6);
    }
}
