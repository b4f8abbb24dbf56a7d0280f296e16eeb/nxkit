//! The views of the text input screen (`textinput.rs`, `textinputui.rs`): the question card of the Home menu
//! (`homeviews.rs`) with a text field and, with a controller, an on-screen keyboard.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{
    NSBox, NSFocusRingType, NSLineBreakMode, NSTextAlignment, NSTextField, NSView,
};
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_foundation::{NSDictionary, NSString};

use super::font::{self, Weight};
use super::homeviews::*;
use super::look::{fills_parent, label, rect, rgb, surface, tokens};

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
