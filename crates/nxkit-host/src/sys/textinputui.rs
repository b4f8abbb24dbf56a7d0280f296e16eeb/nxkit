//! The text input screen (`textinput.rs`): the console's software keyboard as a card over the game, in the look of the
//! Home screens (`homeviews.rs`).
//!
//! The display tick takes a request of a game thread, shows the card and puts the field in edit. The Mac keyboard
//! always types: Return sends and Escape cancels. A click on a capsule or a key takes it too. OK stays quiet, and
//! nothing sends, while the text is shorter than the game's minimum. A longer text than its maximum is cut.
//!
//! While a controller is connected the card shows an on-screen keyboard (`textinput::rows`), and the controllers
//! belong to the screen: the d-pad or the stick moves the focus (held, it repeats), A types the focused key, B
//! deletes, Y types a space, L is shift, `+` goes to OK and sends from there, `-` cancels. Shift is on at the start of
//! an empty field and goes off after a letter, as on a phone. A controller that connects or goes builds the card again
//! with the text typed so far. The answer goes back to the waiting game thread.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSView, NSWindow};
use objc2_foundation::{NSLocale, NSNotification, NSObject, NSObjectProtocol, NSString};

use super::appkit::AppEvents;
use super::homeviews::{self, KEY_CANCEL, KEY_OK, Keyboard, KeyboardCard, PadKeys, TAP_KEY};
use crate::home::{Buttons, Texts};
use crate::textinput::{self, Dir, Key, Row, TextRequest};

/// A held direction repeats after this long, then every [`REPEAT`].
const REPEAT_AFTER: Duration = Duration::from_millis(400);
const REPEAT: Duration = Duration::from_millis(90);

/// The on-screen keyboard while a controller is connected.
struct Pad {
    rows: Vec<Row>,
    focus: (usize, usize),
    shift: bool,
    /// The direction held and when it moves next.
    held: Option<(Dir, Instant)>,
}

/// The screen while it is up.
struct Shown {
    card: KeyboardCard,
    req: TextRequest,
    title: String,
    pad: Option<Pad>,
    /// Without the on-screen keyboard: the controller's focus on Cancel or OK, none before the first move.
    focus: Option<usize>,
    /// What the keys and capsules show now, so they are built again only when it changed.
    drawn: Option<(Option<(usize, usize)>, Option<usize>, bool, bool)>,
    /// The buttons of the last tick, so a press counts once.
    last: Buttons,
}

struct Ui {
    window: Retained<NSWindow>,
    overlay: Retained<NSView>,
    events: Arc<dyn AppEvents>,
    texts: Texts,
    delegate: Retained<FieldDelegate>,
    shown: Option<Shown>,
}

thread_local! {
    // Main thread only: AppKit calls every entry point there.
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    // What the field's delegate saw since the last tick: Return, Escape, an edit.
    static KEYS: RefCell<Keys> = RefCell::new(Keys::default());
}

#[derive(Default)]
struct Keys {
    send: bool,
    cancel: bool,
    edited: bool,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `MainThreadOnly` is correct: the field calls its delegate on the main thread.
    // - `FieldDelegate` does not implement `Drop`.
    /// The delegate of the field: Return, Escape and the edits.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxkitKeyboardFieldDelegate"]
    struct FieldDelegate;

    // SAFETY: `NSObjectProtocol` has no extra requirements.
    unsafe impl NSObjectProtocol for FieldDelegate {}

    impl FieldDelegate {
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, _control: &AnyObject, _view: &AnyObject, command: Sel) -> bool {
            if command == sel!(insertNewline:) {
                KEYS.with(|k| k.borrow_mut().send = true);
                true
            } else if command == sel!(cancelOperation:) {
                KEYS.with(|k| k.borrow_mut().cancel = true);
                true
            } else {
                false
            }
        }

        #[unsafe(method(controlTextDidChange:))]
        fn did_change(&self, _n: &NSNotification) {
            KEYS.with(|k| k.borrow_mut().edited = true);
        }
    }
);

impl FieldDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: `init` of `NSObject` on a fresh instance.
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

/// Puts the screen on `window`: it goes over `overlay`, the content view above the game.
pub(super) fn install(
    mtm: MainThreadMarker,
    window: &NSWindow,
    overlay: &NSView,
    events: Arc<dyn AppEvents>,
    texts: Texts,
) {
    let ui = Ui {
        window: window.retain(),
        overlay: overlay.retain(),
        events,
        texts,
        delegate: FieldDelegate::new(mtm),
        shown: None,
    };
    UI.with(|cell| *cell.borrow_mut() = Some(ui));
    textinput::set_screen(true);
}

/// The application ends: a waiting game thread gets a cancel.
pub(super) fn shutdown() {
    textinput::set_screen(false);
    UI.with(|cell| {
        if let Ok(mut ui) = cell.try_borrow_mut()
            && let Some(ui) = ui.as_mut()
            && ui.shown.take().is_some()
        {
            textinput::answer(None);
        }
    });
}

/// The display tick: shows a new request, reads the keys, the clicks and the controllers, and answers.
pub(super) fn tick() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    UI.with(|cell| {
        let Ok(mut ui) = cell.try_borrow_mut() else {
            return;
        };
        let Some(ui) = ui.as_mut() else {
            return;
        };
        if ui.shown.is_none()
            && let Some(req) = textinput::take()
        {
            let title = if req.header.trim().is_empty() {
                ui.texts.input.clone()
            } else {
                req.header.clone()
            };
            let initial = textinput::clamp(&req.initial, req.max_len).to_owned();
            eprintln!(
                "nxkit-host: text input open: {title:?}, {} to {} characters",
                req.min_len, req.max_len
            );
            super::mouse::release();
            ui.events.home_input(true);
            let app = NSApplication::sharedApplication(mtm);
            app.activate();
            ui.window.makeKeyAndOrderFront(None);
            show(ui, mtm, req, title, &initial, None);
        }
        if ui.shown.is_some() {
            step(ui, mtm);
        }
    });
}

/// The keyboard of the player's language: QWERTZ for German.
fn german() -> bool {
    NSLocale::preferredLanguages()
        .firstObject()
        .is_some_and(|l| l.to_string().starts_with("de"))
}

/// Builds the card for `req` with `text` in the field, with the on-screen keyboard when a controller is connected.
/// `keep` is the shift of a card built again.
fn show(
    ui: &mut Ui,
    mtm: MainThreadMarker,
    req: TextRequest,
    title: String,
    text: &str,
    keep: Option<bool>,
) {
    let last = ui.events.home_buttons();
    let rows = last.connected.then(|| textinput::rows(german()));
    let t = &ui.texts;
    let hints: [(&str, &str); 6] = [
        ("a.circle", &t.key_type),
        ("b.circle", &t.key_delete),
        ("y.circle", &t.key_space),
        ("l.button.roundedbottom.horizontal", &t.key_shift),
        ("plus.circle", &t.ok),
        ("minus.circle", &t.cancel),
    ];
    let card = homeviews::keyboard(
        mtm,
        ui.overlay.bounds(),
        &Keyboard {
            title: &title,
            message: Some(&req.sub),
            guide: &req.guide,
            initial: text,
            pad: rows.as_deref().map(|rows| PadKeys {
                rows,
                hints: &hints,
            }),
        },
    );
    // SAFETY: the delegate implements the `NSTextFieldDelegate` methods it answers to, and lives as long as the UI.
    unsafe {
        let _: () = msg_send![&*card.field, setDelegate: &*ui.delegate];
    }
    ui.overlay.addSubview(&card.view);
    if keep.is_none() {
        homeviews::fade_in(&card.view);
    }
    ui.window.makeFirstResponder(Some(&card.field));
    card.theme_editor();
    card.set_field_focus(true);
    KEYS.with(|k| *k.borrow_mut() = Keys::default());
    let pad = rows.map(|rows| Pad {
        focus: (1, 0),
        shift: keep.unwrap_or(text.is_empty()),
        rows,
        held: None,
    });
    let mut shown = Shown {
        card,
        req,
        title,
        pad,
        focus: None,
        drawn: None,
        last,
    };
    refresh(&mut shown, &ui.texts, mtm);
    ui.shown = Some(shown);
}

/// The text of the field, cut to the maximum when it grew past it.
fn text(shown: &Shown) -> String {
    let now = shown.card.field.stringValue().to_string();
    let cut = textinput::clamp(&now, shown.req.max_len);
    if cut.len() != now.len() {
        set_text(shown, cut);
    }
    cut.to_owned()
}

/// Puts `t` into the field with the caret at its end.
fn set_text(shown: &Shown, t: &str) {
    let field = &shown.card.field;
    field.setStringValue(&NSString::from_str(t));
    if let Some(editor) = field.currentEditor() {
        let end = objc2_foundation::NSRange::new(t.encode_utf16().count(), 0);
        editor.setSelectedRange(end);
    }
}

/// The count, and the keys and capsules when their state changed.
fn refresh(shown: &mut Shown, texts: &Texts, mtm: MainThreadMarker) {
    let t = text(shown);
    shown
        .card
        .set_count(textinput::units(&t), shown.req.max_len);
    let ok = textinput::acceptable(&t, shown.req.min_len, shown.req.max_len);
    let (key_focus, action_focus, shift) = match &shown.pad {
        Some(p) => {
            let last = p.rows.len() - 1;
            if p.focus.0 == last {
                (None, Some(p.focus.1), p.shift)
            } else {
                (Some(p.focus), None, p.shift)
            }
        }
        None => (None, shown.focus, false),
    };
    let state = (key_focus, action_focus, shift, ok);
    if shown.drawn == Some(state) {
        return;
    }
    if let Some(p) = &shown.pad {
        shown
            .card
            .set_keys(mtm, &p.rows, key_focus, shift, &texts.key_space);
    }
    shown.card.set_actions(
        mtm,
        [texts.cancel.as_str(), texts.ok.as_str()],
        action_focus,
        ok,
    );
    shown.drawn = Some(state);
}

/// What the player chose this tick.
enum Choice {
    Send(String),
    Cancel,
}

fn step(ui: &mut Ui, mtm: MainThreadMarker) {
    let texts = ui.texts.clone();
    let buttons = ui.events.home_buttons();
    let Some(shown) = ui.shown.as_mut() else {
        return;
    };
    // A controller that connects or goes: the card again, with the text so far.
    if buttons.connected != shown.pad.is_some() {
        let Some(old) = ui.shown.take() else {
            return;
        };
        let t = old.card.field.stringValue().to_string();
        let keep = old.pad.as_ref().map(|p| p.shift);
        old.card.view.removeFromSuperview();
        show(
            ui,
            mtm,
            old.req,
            old.title,
            &t,
            Some(keep.unwrap_or(t.is_empty())),
        );
        return;
    }
    let keys = KEYS.with(|k| std::mem::take(&mut *k.borrow_mut()));
    let pressed = |now: bool, before: bool| now && !before;
    let last = shown.last;
    shown.last = buttons;
    let tapped = homeviews::take_tap();
    let mut choice = None;
    if keys.cancel || tapped == Some(KEY_CANCEL) {
        choice = Some(Choice::Cancel);
    }
    let mut t = text(shown);
    let ok = |t: &str, req: &TextRequest| textinput::acceptable(t, req.min_len, req.max_len);
    if (keys.send || tapped == Some(KEY_OK)) && ok(&t, &shown.req) {
        choice = Some(Choice::Send(t.clone()));
    }
    if let Some(p) = shown.pad.as_mut() {
        // A tap on a key of the on-screen keyboard types it.
        if let Some(code) = tapped.and_then(|c| c.checked_sub(TAP_KEY)) {
            let (r, i) = (code / 64, code % 64);
            if let Some((Key::Char(c), _)) = p.rows.get(r).and_then(|row| row.get(i)) {
                p.focus = (r, i);
                if let Some(n) = textinput::typed(&t, *c, p.shift, shown.req.max_len) {
                    t = n;
                    p.shift = false;
                    set_text(shown, &t);
                }
            }
        }
        let p = shown.pad.as_mut().expect("the pad above");
        // The directions, with a repeat while one is held.
        let held = [
            (buttons.up, Dir::Up),
            (buttons.down, Dir::Down),
            (buttons.left, Dir::Left),
            (buttons.right, Dir::Right),
        ]
        .into_iter()
        .find(|(down, _)| *down)
        .map(|(_, d)| d);
        let now = Instant::now();
        match (held, p.held) {
            (Some(d), Some((h, next))) if d == h => {
                if now >= next {
                    p.focus = textinput::step(&p.rows, p.focus, d);
                    p.held = Some((d, now + REPEAT));
                }
            }
            (Some(d), _) => {
                p.focus = textinput::step(&p.rows, p.focus, d);
                p.held = Some((d, now + REPEAT_AFTER));
            }
            (None, _) => p.held = None,
        }
        let ok_at = textinput::find(&p.rows, Key::Ok);
        if pressed(buttons.l, last.l) {
            p.shift = !p.shift;
        }
        if pressed(buttons.y, last.y)
            && let Some(n) = textinput::typed(&t, ' ', false, shown.req.max_len)
        {
            t = n;
            set_text(shown, &t);
        }
        let p = shown.pad.as_mut().expect("the pad above");
        if pressed(buttons.b, last.b) && !t.is_empty() {
            t = textinput::deleted(&t);
            p.shift = t.is_empty();
            set_text(shown, &t);
        }
        let p = shown.pad.as_mut().expect("the pad above");
        if pressed(buttons.minus, last.minus) {
            choice = Some(Choice::Cancel);
        }
        if pressed(buttons.plus, last.plus) {
            if Some(p.focus) == ok_at {
                if ok(&t, &shown.req) {
                    choice = Some(Choice::Send(t.clone()));
                }
            } else if let Some(at) = ok_at {
                p.focus = at;
            }
        }
        let p = shown.pad.as_mut().expect("the pad above");
        if pressed(buttons.a, last.a) {
            let key = p.rows[p.focus.0][p.focus.1].0;
            match key {
                Key::Char(c) => {
                    if let Some(n) = textinput::typed(&t, c, p.shift, shown.req.max_len) {
                        t = n;
                        p.shift = false;
                        set_text(shown, &t);
                    }
                }
                Key::Shift => p.shift = !p.shift,
                Key::Space => {
                    if let Some(n) = textinput::typed(&t, ' ', false, shown.req.max_len) {
                        t = n;
                        set_text(shown, &t);
                    }
                }
                Key::Back => {
                    t = textinput::deleted(&t);
                    p.shift = t.is_empty();
                    set_text(shown, &t);
                }
                Key::Cancel => choice = Some(Choice::Cancel),
                Key::Ok => {
                    if ok(&t, &shown.req) {
                        choice = Some(Choice::Send(t.clone()));
                    }
                }
            }
        }
    } else {
        // Without the on-screen keyboard: left and right choose Cancel or OK, A takes it, B cancels.
        if pressed(buttons.left, last.left) {
            shown.focus = Some(KEY_CANCEL);
        }
        if pressed(buttons.right, last.right) {
            shown.focus = Some(KEY_OK);
        }
        shown.card.set_field_focus(shown.focus.is_none());
        if pressed(buttons.b, last.b) {
            choice = Some(Choice::Cancel);
        }
        if pressed(buttons.a, last.a) {
            match shown.focus.unwrap_or(KEY_OK) {
                KEY_CANCEL => choice = Some(Choice::Cancel),
                _ if ok(&t, &shown.req) => choice = Some(Choice::Send(t.clone())),
                _ => {}
            }
        }
    }
    refresh(shown, &texts, mtm);
    match choice {
        Some(Choice::Send(t)) => close(ui, Some(t)),
        Some(Choice::Cancel) => close(ui, None),
        None => {}
    }
}

fn close(ui: &mut Ui, answer: Option<String>) {
    let Some(shown) = ui.shown.take() else {
        return;
    };
    match &answer {
        Some(t) => eprintln!(
            "nxkit-host: text input sent ({} characters)",
            textinput::units(t)
        ),
        None => eprintln!("nxkit-host: text input cancelled"),
    }
    ui.window.makeFirstResponder(None);
    homeviews::fade_out(&shown.card.view);
    ui.events.home_input(false);
    textinput::answer(answer);
}
