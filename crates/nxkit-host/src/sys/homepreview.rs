//! `nxkit-host home-preview <folder>`: the screens of the Home button drawn into PNG files, wide (1920 x 1080 points)
//! and compact (600 x 400), over a stand-in of the game, so the look can be checked against NX2Apple's console mode
//! without a game or a controller.

use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBitmapImageFileType, NSBox, NSBoxType, NSColor, NSTitlePosition, NSView};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSDictionary, NSString};

use super::homeviews::{self, Card};
use crate::home::Texts;

/// Draws every screen into `dir` and returns the files.
pub fn run(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mtm = MainThreadMarker::new().ok_or("the preview must run on the main thread")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let t = Texts::from_lines(None, "The Legend of Zelda: Breath of the Wild");
    let rows = [
        (t.resume.as_str(), true),
        (t.launcher.as_str(), false),
        (t.quit.as_str(), false),
    ];
    let answers = [(t.cancel.as_str(), false), (t.quit.as_str(), true)];
    let menu = |focused| Card {
        symbol: "house",
        title: &t.menu_title,
        message: None,
        actions: &rows,
        rows: true,
        focused,
    };
    let prompt = |focused| Card {
        symbol: "xmark.circle",
        title: &t.title,
        message: Some(&t.note),
        actions: &answers,
        rows: false,
        focused,
    };
    let mut out = Vec::new();
    for (size, tag) in [((1920.0, 1080.0), "wide"), ((600.0, 400.0), "compact")] {
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size.0, size.1));
        let shots: [(&str, Retained<NSView>); 5] = [
            ("menu", homeviews::card(mtm, bounds, &menu(0))),
            ("menu-quit", homeviews::card(mtm, bounds, &menu(2))),
            ("prompt", homeviews::card(mtm, bounds, &prompt(0))),
            ("prompt-quit", homeviews::card(mtm, bounds, &prompt(1))),
            ("hold", homeviews::hold(mtm, bounds, &t.hold).0),
        ];
        for (name, view) in shots {
            if name == "hold" {
                set_ring(&view, 0.6);
            }
            let path = dir.join(format!("home-{name}-{tag}.png"));
            write(mtm, bounds, &view, &path)?;
            out.push(path);
        }
        // The text input screen: empty with the game's guide, typed with the field's focus, and the controller on OK.
        let rows = crate::textinput::rows(true);
        let hints: [(&str, &str); 6] = [
            ("a.circle", &t.key_type),
            ("b.circle", &t.key_delete),
            ("y.circle", &t.key_space),
            ("l.button.roundedbottom.horizontal", &t.key_shift),
            ("plus.circle", &t.ok),
            ("minus.circle", &t.cancel),
        ];
        let request = |initial, pad: bool| homeviews::Keyboard {
            title: "Wie heißt du?",
            message: Some("Gib deinen Namen ein. Er erscheint im Spiel."),
            guide: "Dein Name",
            initial,
            pad: pad.then(|| homeviews::PadKeys {
                rows: &rows,
                hints: &hints,
            }),
        };
        let labels = [t.cancel.as_str(), t.ok.as_str()];
        for (name, initial, focus, field) in [
            ("keyboard-empty", "", None, true),
            ("keyboard-typed", "Zijad", None, true),
            ("keyboard-pad-ok", "Zijad", Some(homeviews::KEY_OK), false),
        ] {
            let card = homeviews::keyboard(mtm, bounds, &request(initial, false));
            let n = crate::textinput::units(initial);
            card.set_count(n, 10);
            card.set_field_focus(field);
            card.set_actions(
                mtm,
                labels,
                focus,
                crate::textinput::acceptable(initial, 1, 10),
            );
            let path = dir.join(format!("home-{name}-{tag}.png"));
            write(mtm, bounds, &card.view, &path)?;
            out.push(path);
        }
        // With a controller: the on-screen keyboard, the focus on a letter with shift on, and on OK.
        for (name, initial, focus, shift) in [
            ("keyboard-pad-keys", "", (2, 4), true),
            ("keyboard-pad-keys-ok", "Zijad", (5, 1), false),
        ] {
            let card = homeviews::keyboard(mtm, bounds, &request(initial, true));
            card.set_count(crate::textinput::units(initial), 10);
            card.set_field_focus(true);
            let on_actions = focus.0 == rows.len() - 1;
            card.set_keys(
                mtm,
                &rows,
                (!on_actions).then_some(focus),
                shift,
                &t.key_space,
            );
            card.set_actions(
                mtm,
                labels,
                on_actions.then_some(focus.1),
                crate::textinput::acceptable(initial, 1, 10),
            );
            let path = dir.join(format!("home-{name}-{tag}.png"));
            write(mtm, bounds, &card.view, &path)?;
            out.push(path);
        }
    }
    let card = super::loading::preview(mtm, "Super Mario Bros. Wonder", 1240, 3400);
    let bounds = card.frame();
    let path = dir.join("loading.png");
    write(mtm, bounds, &card, &path)?;
    out.push(path);
    Ok(out)
}

/// The ring of a hold view, 60 % full in the preview.
fn set_ring(view: &NSView, fill: f64) {
    for sub in view.subviews().iter() {
        if let Ok(ring) = sub.downcast::<homeviews::HoldRing>() {
            ring.set(fill);
        }
    }
}

/// `view` over a stand-in of the game, as a PNG at the screen's scale of 2.
fn write(mtm: MainThreadMarker, bounds: CGRect, view: &NSView, path: &Path) -> Result<(), String> {
    let root = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    let game = NSBox::new(mtm);
    game.setBoxType(NSBoxType::Custom);
    game.setTitlePosition(NSTitlePosition::NoTitle);
    game.setBorderWidth(0.0);
    game.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.25, 0.42, 0.48, 1.0,
    ));
    game.setFrame(bounds);
    root.addSubview(&game);
    root.addSubview(view);
    let rep = root
        .bitmapImageRepForCachingDisplayInRect(bounds)
        .ok_or("no bitmap for the view")?;
    root.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
    // SAFETY: an empty dictionary of properties.
    let data = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .ok_or("no PNG of the bitmap")?;
    let p = NSString::from_str(&path.to_string_lossy());
    if data.writeToFile_atomically(&p, true) {
        Ok(())
    } else {
        Err(format!("{}: not written", path.display()))
    }
}
