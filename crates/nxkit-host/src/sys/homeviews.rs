//! The views of the Home menu and the quit prompt (`home.rs`), drawn over the game in the look of NX2Apple's console
//! mode: the question card of `ConsolePages.swift` (`ConsoleQuestion`) with the tokens of NX2Apple's
//! `design-tokens/tokens.json`. A dim scrim in the page colour, a card of the surface colour with a hairline, a cream
//! symbol, the title and the note in Manrope, and capsules: the one primary action cream, the others a quiet white.
//! The controller's focus is a ring of 2 points, cream (black on the cream capsule), as `PadFocus` draws it. A window
//! narrower than 650 points gets the compact sizes of the console mode.
//!
//! The views are AppKit views, so they draw while the game presents. Under the card the game's picture is blurred
//! (`NSVisualEffectView` within the window) and dimmed. The scrim is flipped, so the layout goes down from the top, and
//! it keeps the mouse from the game's view.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{Ivars, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAnimationContext, NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua,
    NSBezierPath, NSBox, NSEvent, NSFocusRingType, NSFontWeightLight, NSImage,
    NSImageSymbolConfiguration, NSImageView, NSLineBreakMode, NSLineCapStyle, NSResponder,
    NSTextAlignment, NSTextField, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSDictionary, NSObject, NSString};

use super::font::{self, Weight};
use super::look::{fills_parent, label, rect, rgb, surface, tokens};

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
    // SAFETY: as `Scrim`.
    /// The dim of the scrim, over the blur.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxkitHomeDim"]
    struct Dim;

    impl Dim {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, rect: CGRect) {
            rgb(tokens::BACKGROUND, tokens::SCRIM).setFill();
            NSBezierPath::fillRect(rect);
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

impl Dim {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        // SAFETY: as `Scrim::new`.
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

/// A capsule with its label centred, cream when `primary`, with the focus ring when `focused`. The label gets a
/// smaller font, down to 80 %, then is cut, so every capsule keeps its width (`minimumScaleFactor(0.8)`). A capsule
/// that is not `enabled` is quiet and its label faint, whatever `primary` says (a disabled `DSSheetPrimaryButton`).
fn capsule(
    mtm: MainThreadMarker,
    parent: &NSView,
    frame: CGRect,
    text: &str,
    (primary, enabled): (bool, bool),
    focused: bool,
    size: f64,
) {
    let (fill, fg, ring) = if !enabled {
        (
            rgb(tokens::WHITE, tokens::QUIET_FILL),
            rgb(tokens::WHITE, tokens::TEXT_DISABLED),
            rgb(tokens::ACCENT, 1.0),
        )
    } else if primary {
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

/// A blur of what is under `bounds` in the window: the game's picture under the Home screens.
fn blur(mtm: MainThreadMarker, bounds: CGRect) -> Retained<NSVisualEffectView> {
    let v = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), bounds);
    v.setBlendingMode(NSVisualEffectBlendingMode::WithinWindow);
    v.setMaterial(NSVisualEffectMaterial::FullScreenUI);
    v.setState(NSVisualEffectState::Active);
    // SAFETY: `NSAppearanceNameDarkAqua` is a constant of AppKit.
    let dark = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua });
    v.setAppearance(dark.as_deref());
    fills_parent(&v);
    v
}

/// The scrim over `bounds`: the blurred game, dimmed, and the card in its middle.
pub(super) fn card(mtm: MainThreadMarker, bounds: CGRect, c: &Card<'_>) -> Retained<NSView> {
    let look = Look::for_width(bounds.size.width);
    let scrim = Scrim::new(mtm, bounds);
    fills_parent(&scrim);
    let local = CGRect::new(CGPoint::new(0.0, 0.0), bounds.size);
    let dim = Dim::new(mtm, local);
    fills_parent(&dim);
    scrim.addSubview(&blur(mtm, local));
    scrim.addSubview(&dim);
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
            (*primary, true),
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

define_class!(
    // SAFETY: as `Scrim`. The ivar is read on the main thread only.
    /// A click target over a capsule of the text input screen. A click (down and up inside) notes its action for the
    /// next display tick ([`take_tap`]).
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxkitKeyboardTap"]
    struct Tap {
        action: Cell<usize>,
    }

    impl Tap {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _e: &NSEvent) {}

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, e: &NSEvent) {
            let p = self.convertPoint_fromView(e.locationInWindow(), None);
            let b = self.bounds();
            if p.x >= 0.0 && p.y >= 0.0 && p.x <= b.size.width && p.y <= b.size.height {
                TAPPED.with(|t| t.set(Some(self.action().get())));
            }
        }
    }
);

define_class!(
    // SAFETY: as `Scrim`.
    /// A flipped layer whose children take the mouse: the actions of the text input screen.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxkitKeyboardActions"]
    struct Actions;

    impl Actions {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

thread_local! {
    // Main thread only: the action a click chose, until the display tick takes it.
    static TAPPED: Cell<Option<usize>> = const { Cell::new(None) };
}

/// The action that a click on a capsule of the text input screen chose since the last call.
pub(super) fn take_tap() -> Option<usize> {
    TAPPED.with(Cell::take)
}

impl Tap {
    fn new(mtm: MainThreadMarker, frame: CGRect, action: usize) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars::<Tap> {
            action: Cell::new(action),
        });
        // SAFETY: as `Scrim::new`, on an instance whose ivars are set.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

impl Actions {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        // SAFETY: as `Scrim::new`.
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }
}

/// What the text input screen shows: the game's request, in the words the game gave.
pub(super) struct Keyboard<'a> {
    /// The title: the game's header, or the host's own when the game gives none.
    pub(super) title: &'a str,
    /// The game's sub text, if any.
    pub(super) message: Option<&'a str>,
    /// The game's guide: the placeholder of the empty field.
    pub(super) guide: &'a str,
    /// The text the field starts with.
    pub(super) initial: &'a str,
    /// The on-screen keyboard, while a controller is connected.
    pub(super) pad: Option<PadKeys<'a>>,
}

/// The on-screen keyboard of the text input screen: its rows (the last one Cancel and OK) and the hints under it.
pub(super) struct PadKeys<'a> {
    /// The rows of `textinput::rows`.
    pub(super) rows: &'a [crate::textinput::Row],
    /// The hints: an SF Symbol of a controller button and what it does.
    pub(super) hints: &'a [(&'a str, &'a str)],
}

/// The tiles of the on-screen keyboard as built.
struct KeysLayer {
    layer: Retained<Actions>,
    /// The frame of every key, by row. The last row is Cancel and OK, which the capsules draw.
    frames: Vec<Vec<CGRect>>,
    unit: f64,
}

/// The text input screen as built: the parts that change while it is up.
pub(super) struct KeyboardCard {
    /// The scrim with everything on it.
    pub(super) view: Retained<NSView>,
    /// The field the player types into.
    pub(super) field: Retained<NSTextField>,
    /// The box under the field, whose edge shows the focus.
    field_box: Retained<NSBox>,
    /// The count of the characters, under the field on the right.
    counter: Retained<NSTextField>,
    /// The layer of the two capsules, built again when their state changes.
    actions: Retained<Actions>,
    /// The frames of Cancel and OK.
    frames: [CGRect; 2],
    /// The on-screen keyboard, while a controller is connected.
    keys: Option<KeysLayer>,
    look: Look,
}

/// The index of Cancel among the actions of the text input screen.
pub(super) const KEY_CANCEL: usize = 0;
/// The index of OK.
pub(super) const KEY_OK: usize = 1;
/// The first tap action of a key of the on-screen keyboard: `TAP_KEY + row * 64 + index`.
pub(super) const TAP_KEY: usize = 1000;

/// The text input screen over `bounds`: `ConsoleQuestion` with a field between the note and the actions, the field of
/// NX2Apple's player and rename sheets at the size of the console mode (`surfaceRaised`, the separator's edge, the
/// cream edge with the focus), and a count of the characters under it.
pub(super) fn keyboard(mtm: MainThreadMarker, bounds: CGRect, k: &Keyboard<'_>) -> KeyboardCard {
    let look = Look::for_width(bounds.size.width);
    let scrim = Scrim::new(mtm, bounds);
    fills_parent(&scrim);
    let local = CGRect::new(CGPoint::new(0.0, 0.0), bounds.size);
    let dim = Dim::new(mtm, local);
    fills_parent(&dim);
    scrim.addSubview(&blur(mtm, local));
    scrim.addSubview(&dim);
    // The on-screen keyboard takes a wider card: eleven keys a row stay big enough to read from the couch.
    let width = if k.pad.is_some() {
        (bounds.size.width - 48.0).clamp(200.0, 820.0)
    } else {
        look.card_width(bounds.size.width)
    };
    let (pad, gap) = (look.padding(), look.spacing());
    let inner = width - 2.0 * pad;
    let mut sym = symbol(mtm, "keyboard", look.symbol());
    let (title, ts) = label(
        mtm,
        k.title,
        &font::manrope(look.title(), Weight::Bold),
        &rgb(tokens::WHITE, tokens::TEXT_PRIMARY),
        inner,
        true,
    );
    let message = k.message.filter(|m| !m.trim().is_empty()).map(|m| {
        label(
            mtm,
            m,
            &font::manrope(look.message(), Weight::Regular),
            &rgb(tokens::WHITE, tokens::TEXT_SECONDARY),
            inner,
            true,
        )
    });
    let field_h = look.button_height();
    let count_font = font::manrope(look.pick(17.0, 15.0), Weight::Regular);
    let (counter, cs) = label(
        mtm,
        "0",
        &count_font,
        &rgb(tokens::WHITE, tokens::TEXT_SECONDARY),
        inner,
        false,
    );
    let bh = look.button_height();
    let sym_h = sym.as_ref().map_or(0.0, |(_, s)| s.height + gap);
    let msg_h = message.as_ref().map_or(0.0, |(_, s)| gap + s.height);
    // The field belongs to the question: one step below the note, its count close under it, then the actions.
    let field_block = gap + field_h + 8.0 + cs.height;
    let key_gap = look.pick(8.0, 6.0);
    let unit = (inner - key_gap * f64::from(crate::textinput::COLUMNS - 1))
        / f64::from(crate::textinput::COLUMNS);
    let mut key_h = (unit * 0.92).min(look.pick(56.0, 44.0)).floor();
    let mut hint_h = look.pick(26.0, 22.0);
    let key_rows = k
        .pad
        .as_ref()
        .map_or(0.0, |p| p.rows.len().saturating_sub(1) as f64);
    let actions_h = |key_h: f64, hint_h: f64| match &k.pad {
        Some(_) => {
            let hints = if hint_h > 0.0 { gap + hint_h } else { 0.0 };
            key_rows * key_h + (key_rows - 1.0).max(0.0) * key_gap + gap + bh + hints
        }
        None => bh,
    };
    let rest =
        |sym_h: f64| pad + sym_h + ts.height + msg_h + field_block + gap + look.buttons_top() + pad;
    let mut sym_h = sym_h;
    // A short window keeps the keyboard and its actions whole: first the symbol goes, then the hints, then the keys
    // get lower, to a floor.
    let room = bounds.size.height - 24.0;
    if k.pad.is_some() && rest(sym_h) + actions_h(key_h, hint_h) > room {
        sym = None;
        sym_h = 0.0;
    }
    if k.pad.is_some() && rest(sym_h) + actions_h(key_h, hint_h) > room {
        hint_h = 0.0;
    }
    if k.pad.is_some() && rest(sym_h) + actions_h(key_h, hint_h) > room {
        let free = room - rest(sym_h) - actions_h(0.0, hint_h);
        key_h = (free / key_rows).floor().clamp(28.0, key_h);
    }
    let height = rest(sym_h) + actions_h(key_h, hint_h);
    let x0 = ((bounds.size.width - width) / 2.0).max(0.0);
    let y0 = ((bounds.size.height - height) / 2.0).max(0.0);
    let hairline = rgb(tokens::WHITE, tokens::HAIRLINE);
    scrim.addSubview(&surface(
        mtm,
        rect(x0, y0, width, height),
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
    y += gap;
    let separator = rgb(tokens::WHITE, tokens::SEPARATOR);
    let field_box = surface(
        mtm,
        rect(x, y, inner, field_h),
        &rgb(tokens::SURFACE_RAISED, 1.0),
        16.0,
        (&separator, 1.0),
    );
    scrim.addSubview(&field_box);
    let field = text_field(mtm, look, k);
    // The field at its own height, centred in the box: in the window the field editor draws the text and the caret
    // within that height, so the caret sits in the middle of the box.
    let line = field.fittingSize().height.ceil();
    field.setFrame(rect(
        x + 18.0,
        y + (field_h - line) / 2.0,
        inner - 36.0,
        line,
    ));
    scrim.addSubview(&field);
    y += field_h + 8.0;
    counter.setAlignment(NSTextAlignment::Right);
    counter.setFrame(rect(x, y, inner, cs.height));
    scrim.addSubview(&counter);
    y += cs.height + gap + look.buttons_top();
    let mut keys = k.pad.as_ref().map(|p| {
        let mut frames = Vec::with_capacity(p.rows.len());
        let last = p.rows.len().saturating_sub(1);
        for (r, row) in p.rows.iter().enumerate() {
            let (top, h) = if r == last {
                (y + r as f64 * (key_h + key_gap) - key_gap + gap, bh)
            } else {
                (y + r as f64 * (key_h + key_gap), key_h)
            };
            let mut left = 0u32;
            let mut out = Vec::with_capacity(row.len());
            for (_, w) in row {
                let kx = x + f64::from(left) * (unit + key_gap);
                let kw = f64::from(*w) * unit + f64::from(w.saturating_sub(1)) * key_gap;
                // The last row has the gap of the capsules between its two halves.
                out.push(rect(kx, top, kw, h));
                left += u32::from(*w);
            }
            frames.push(out);
        }
        let layer = Actions::new(mtm, local);
        fills_parent(&layer);
        scrim.addSubview(&layer);
        KeysLayer {
            layer,
            frames,
            unit,
        }
    });
    // Cancel and OK are the capsules of `ConsoleQuestion`: two halves 14 points apart, also under the keyboard.
    let each = (inner - 14.0) / 2.0;
    let frames = match keys.as_mut() {
        Some(kl) => {
            let top = kl
                .frames
                .last()
                .and_then(|r| r.first())
                .map_or(y, |f| f.origin.y);
            let halves = [rect(x, top, each, bh), rect(x + each + 14.0, top, each, bh)];
            if let Some(last) = kl.frames.last_mut() {
                *last = halves.to_vec();
            }
            halves
        }
        None => [rect(x, y, each, bh), rect(x + each + 14.0, y, each, bh)],
    };
    if let (Some(p), Some(kl), true) = (&k.pad, &keys, hint_h > 0.0) {
        let bottom = kl
            .frames
            .last()
            .and_then(|r| r.first())
            .map_or(y, |f| f.origin.y + f.size.height);
        hints(
            mtm,
            &scrim,
            rect(x, bottom + gap, inner, hint_h),
            p.hints,
            look,
        );
    }
    let actions = Actions::new(mtm, local);
    fills_parent(&actions);
    scrim.addSubview(&actions);
    KeyboardCard {
        view: Retained::into_super(scrim),
        field,
        field_box,
        counter,
        actions,
        frames,
        keys,
        look,
    }
}

/// The hints under the on-screen keyboard, left to right: the SF Symbol of a controller button in the cream, what it
/// does in the secondary text.
fn hints(
    mtm: MainThreadMarker,
    parent: &NSView,
    frame: CGRect,
    items: &[(&str, &str)],
    look: Look,
) {
    let f = font::manrope(look.pick(17.0, 15.0), Weight::Regular);
    let color = rgb(tokens::WHITE, tokens::TEXT_SECONDARY);
    let mut x = frame.origin.x;
    for (glyph, text) in items {
        if let Some((view, s)) = symbol(mtm, glyph, look.pick(20.0, 17.0)) {
            view.setFrame(rect(
                x,
                frame.origin.y + (frame.size.height - s.height) / 2.0,
                s.width,
                s.height,
            ));
            parent.addSubview(&view);
            x += s.width + 8.0;
        }
        let (t, s) = label(mtm, text, &f, &color, frame.size.width, false);
        t.setFrame(rect(
            x,
            frame.origin.y + (frame.size.height - s.height) / 2.0,
            s.width + 2.0,
            s.height,
        ));
        parent.addSubview(&t);
        x += s.width + look.pick(28.0, 20.0);
    }
}

/// The editable field: the console body size in Manrope, the cream caret and selection, the guide as a placeholder in
/// the secondary text colour (the tertiary one falls below 4.5:1 on `surfaceRaised`), no bezel and no system focus
/// ring, since the box under it shows the focus.
fn text_field(mtm: MainThreadMarker, look: Look, k: &Keyboard<'_>) -> Retained<NSTextField> {
    let field = NSTextField::new(mtm);
    field.setEditable(true);
    field.setSelectable(true);
    field.setBezeled(false);
    field.setBordered(false);
    field.setDrawsBackground(false);
    field.setFocusRingType(NSFocusRingType::None);
    field.setUsesSingleLineMode(true);
    field.setLineBreakMode(NSLineBreakMode::ByClipping);
    let f = font::manrope(look.pick(20.0, 18.0), Weight::Medium);
    field.setFont(Some(&f));
    field.setTextColor(Some(&rgb(tokens::WHITE, tokens::TEXT_PRIMARY)));
    field.setStringValue(&NSString::from_str(k.initial));
    if !k.guide.is_empty() {
        let color = rgb(tokens::WHITE, tokens::TEXT_SECONDARY);
        let keys = [
            &*NSString::from_str("NSFont"),
            &*NSString::from_str("NSColor"),
        ];
        let values: [&AnyObject; 2] = [&f, &color];
        let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
        let text = NSString::from_str(k.guide);
        // SAFETY: `initWithString:attributes:` of `NSAttributedString` with an `NSString` and a dictionary of string
        // keys; `setPlaceholderAttributedString:` of `NSTextField` takes the attributed string.
        unsafe {
            let a: Retained<AnyObject> = msg_send![
                msg_send![objc2::class!(NSAttributedString), alloc],
                initWithString: &*text,
                attributes: &*attrs
            ];
            let _: () = msg_send![&*field, setPlaceholderAttributedString: &*a];
        }
    }
    field
}

impl KeyboardCard {
    /// Themes the field editor of the window once the field edits: the cream caret and selection.
    pub(super) fn theme_editor(&self) {
        let Some(editor) = self.field.currentEditor() else {
            return;
        };
        let caret = rgb(tokens::ACCENT, 1.0);
        let selection = rgb(tokens::ACCENT, tokens::SELECTION);
        let ink = rgb(tokens::WHITE, 1.0);
        let keys = [
            &*NSString::from_str("NSBackgroundColor"),
            &*NSString::from_str("NSColor"),
        ];
        let values: [&AnyObject; 2] = [&selection, &ink];
        let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
        // SAFETY: the field editor of an `NSTextField` is an `NSTextView`, which has both setters.
        unsafe {
            let _: () = msg_send![&*editor, setInsertionPointColor: &*caret];
            let _: () = msg_send![&*editor, setSelectedTextAttributes: &*attrs];
        }
    }

    /// The count under the field: `n / max`, or `n` without a limit, in digits of one width.
    pub(super) fn set_count(&self, n: u32, max: u32) {
        let text = if max == 0 {
            n.to_string()
        } else {
            format!("{n} / {max}")
        };
        self.counter.setStringValue(&NSString::from_str(&text));
        let f = font::manrope(self.look.pick(17.0, 15.0), Weight::Regular);
        self.counter.setFont(Some(&font::tabular(&f)));
    }

    /// The edge of the field: the cream ring while the keyboard types into it, else the separator.
    pub(super) fn set_field_focus(&self, focused: bool) {
        if focused {
            self.field_box.setBorderColor(&rgb(tokens::ACCENT, 1.0));
            self.field_box.setBorderWidth(tokens::FOCUS_RING);
        } else {
            self.field_box
                .setBorderColor(&rgb(tokens::WHITE, tokens::SEPARATOR));
            self.field_box.setBorderWidth(1.0);
        }
    }

    /// Builds the keys of the on-screen keyboard again: the controller's focus on `focus`, a key above the last row
    /// (the capsules draw the last row), the letters upper case and shift cream while `shift` is on, `space` on the
    /// space bar.
    pub(super) fn set_keys(
        &self,
        mtm: MainThreadMarker,
        rows: &[crate::textinput::Row],
        focus: Option<(usize, usize)>,
        shift: bool,
        space: &str,
    ) {
        use crate::textinput::Key;
        let Some(kl) = &self.keys else {
            return;
        };
        for v in kl.layer.subviews().to_vec() {
            v.removeFromSuperview();
        }
        let size = self.look.pick(22.0, 18.0).min(kl.unit * 0.42);
        let letter = font::manrope(size, Weight::Semibold);
        let small = font::manrope(self.look.pick(16.0, 14.0), Weight::Regular);
        let last = rows.len().saturating_sub(1);
        for (r, row) in rows.iter().enumerate().take(last) {
            for (i, (key, _)) in row.iter().enumerate() {
                let Some(frame) = kl.frames.get(r).and_then(|fr| fr.get(i)).copied() else {
                    continue;
                };
                let focused = focus == Some((r, i));
                let ring = rgb(tokens::ACCENT, 1.0);
                let fill = rgb(
                    tokens::WHITE,
                    if focused { 0.16 } else { tokens::QUIET_FILL },
                );
                let tile = surface(
                    mtm,
                    frame,
                    &fill,
                    12.0,
                    (&ring, if focused { tokens::FOCUS_RING } else { 0.0 }),
                );
                kl.layer.addSubview(&tile);
                let glyph = match key {
                    Key::Shift => Some(if shift { "shift.fill" } else { "shift" }),
                    Key::Back => Some("delete.left"),
                    _ => None,
                };
                if let Some(name) = glyph {
                    if let Some((view, s)) = symbol(mtm, name, size) {
                        if *key == Key::Back || !shift {
                            view.setContentTintColor(Some(&rgb(
                                tokens::WHITE,
                                tokens::TEXT_PRIMARY,
                            )));
                        }
                        view.setFrame(rect(
                            frame.origin.x + (frame.size.width - s.width) / 2.0,
                            frame.origin.y + (frame.size.height - s.height) / 2.0,
                            s.width,
                            s.height,
                        ));
                        kl.layer.addSubview(&view);
                    }
                    continue;
                }
                let (text, f, color) = match key {
                    Key::Char(c) => {
                        let t: String = if shift {
                            c.to_uppercase().collect()
                        } else {
                            c.to_string()
                        };
                        (t, &letter, rgb(tokens::WHITE, tokens::TEXT_PRIMARY))
                    }
                    Key::Space => (
                        space.to_owned(),
                        &small,
                        rgb(tokens::WHITE, tokens::TEXT_SECONDARY),
                    ),
                    _ => continue,
                };
                let (t, s) = label(mtm, &text, f, &color, frame.size.width, false);
                t.setAlignment(NSTextAlignment::Center);
                t.setFrame(rect(
                    frame.origin.x,
                    frame.origin.y + (frame.size.height - s.height) / 2.0,
                    frame.size.width,
                    s.height,
                ));
                kl.layer.addSubview(&t);
                if let Key::Char(_) = key {
                    kl.layer
                        .addSubview(&Tap::new(mtm, frame, TAP_KEY + r * 64 + i));
                }
            }
        }
    }

    /// Builds the capsules again: `labels` Cancel and OK, the controller's focus on `focused`, OK quiet while the text
    /// may not be sent.
    pub(super) fn set_actions(
        &self,
        mtm: MainThreadMarker,
        labels: [&str; 2],
        focused: Option<usize>,
        ok_enabled: bool,
    ) {
        for v in self.actions.subviews().to_vec() {
            v.removeFromSuperview();
        }
        for (i, text) in labels.iter().enumerate() {
            let primary = i == KEY_OK;
            let enabled = !primary || ok_enabled;
            capsule(
                mtm,
                &self.actions,
                self.frames[i],
                text,
                (primary, enabled),
                focused == Some(i),
                self.look.button_text(),
            );
            if enabled {
                self.actions.addSubview(&Tap::new(mtm, self.frames[i], i));
            }
        }
    }
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
    // SAFETY: `performSelector:withObject:afterDelay:` of `NSObject` on the main thread. `removeFromSuperview` takes no
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
