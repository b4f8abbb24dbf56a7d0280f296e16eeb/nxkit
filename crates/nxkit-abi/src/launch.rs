//! The launch options of the host: the rows of `launcher.json` that a launcher shows for a game app.
//!
//! A launcher (NX2Apple) shows the options of an app without knowing them. The build writes one row per option of
//! [`OPTIONS`] into the run folder as [`FILE`]. The launcher draws the rows and keeps the choice of the player for each
//! row as the string `launcher.<env>` ([`saved_key`]) in the preferences domain of the app (its bundle identifier),
//! only when it is not the default. At start the host reads these choices for the options of [`OPTIONS`], keeps each
//! one that [`Opt::accepts`], and uses it for a variable that the environment does not set. The host's `hostenv`
//! reads the variables by the names of this table, so the table and what the host reads stay the same.
//!
//! The file has the schema of the old engine's launcher (`dm/games/settings.py`, schema 1):
//!
//! ```text
//! {"schema": 1, "game": "0100…", "settings": [
//!   {"env": "NXKIT_OP_MODE", "kind": "choice", "label": "Console mode", "note": "…", "default": "docked",
//!    "section": "Game", "level": "basic", "options": [["docked", "TV (docked)"], ["handheld", "Handheld"]]},
//!   {"env": "NXKIT_FULLSCREEN", "kind": "toggle", "on": "1", "off": "0", …},
//!   {"env": "…", "kind": "range", "min": 0.5, "max": 2, "step": 0.25, …}]}
//! ```
//!
//! The labels are English. A launcher can show its own text for the keys `launch.set.<env>`,
//! `launch.set.<env>.note` and `launch.opt.<env>.<value>`. A new option of the host is one row here and its read in
//! `hostenv`. The launcher does not change.

/// The file name of the list, in the run folder of the app (`Contents/Resources/nxkit`).
pub const FILE: &str = "launcher.json";

/// The schema of the list. Schema 1 only gets optional fields.
pub const SCHEMA: u32 = 1;

/// The prefix of a saved choice in the preferences domain of the app.
pub const SAVED_PREFIX: &str = "launcher.";

/// `docked` or `handheld`: the operation mode that the game sees. Unset: the mode of the title module.
pub const OP_MODE: &str = "NXKIT_OP_MODE";

/// `1`: the window starts in full screen.
pub const FULLSCREEN: &str = "NXKIT_FULLSCREEN";

/// `1`: the game pauses when the window loses the focus.
pub const PAUSE_UNFOCUSED: &str = "NXKIT_PAUSE_UNFOCUSED";

/// `0`: a short press of the Home button does not open the Home menu.
pub const HOME_MENU: &str = "NXKIT_HOME_MENU";

/// `0`: holding the Home button does not open the quit prompt.
pub const HOME_QUIT: &str = "NXKIT_HOME_QUIT";

/// A console language tag such as `de` or `en-GB`. Empty or unset: the Mac's language.
pub const LANGUAGE: &str = "NXKIT_LANGUAGE";

/// `0`: the controllers do not rumble.
pub const RUMBLE: &str = "NXKIT_RUMBLE";

/// 0 to 3: the factor on the rumble of the game, 1 plays it as it is.
pub const RUMBLE_GAIN: &str = "NXKIT_RUMBLE_GAIN";

/// 0 to 0.9: the rumble below which a controller does not move.
pub const RUMBLE_MIN: &str = "NXKIT_RUMBLE_MIN";

/// `1`: the Metal performance HUD is shown over the game.
pub const METAL_HUD: &str = "NXKIT_METAL_HUD";

/// `30`, `60` or `120`: the frame rate in place of `fps_target` of the title module. Unset: the title's.
pub const FPS_TARGET: &str = "NXKIT_FPS_TARGET";

/// 0 to 50: the radial dead zone of the left stick of a controller, in percent of its travel. 0: none.
pub const STICK_DEADZONE_L: &str = "NXKIT_STICK_DEADZONE_L";

/// 0 to 50: the radial dead zone of the right stick of a controller, in percent of its travel. 0: none.
pub const STICK_DEADZONE_R: &str = "NXKIT_STICK_DEADZONE_R";

/// `1`: every controller is player 1.
pub const PAD_MERGE: &str = "NXKIT_PAD_MERGE";

/// `1`: a new controller becomes a player at its first button press, the lowest player that is free.
pub const PAD_PRESS_TO_JOIN: &str = "NXKIT_PAD_PRESS_TO_JOIN";

/// How a launcher shows an option, and the values that it takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// One of a list of `(value, label)`.
    Choice(&'static [(&'static str, &'static str)]),
    /// A switch with the value `on` or `off`.
    Toggle {
        /// The value when the switch is on.
        on: &'static str,
        /// The value when the switch is off.
        off: &'static str,
    },
    /// A number from `min` to `max`, in steps of `step`.
    Range {
        /// The lowest value.
        min: f64,
        /// The highest value.
        max: f64,
        /// The step of the slider.
        step: f64,
    },
}

impl Kind {
    /// The name of the kind in the file: `choice`, `toggle` or `range`.
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Choice(_) => "choice",
            Kind::Toggle { .. } => "toggle",
            Kind::Range { .. } => "range",
        }
    }
}

/// One launch option: one row of the file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opt {
    /// The variable that the option sets.
    pub env: &'static str,
    /// How the launcher shows it.
    pub kind: Kind,
    /// The English label.
    pub label: &'static str,
    /// The English note under the label, can be empty.
    pub note: &'static str,
    /// The default value. The build puts the mode and the frame rate of the title module in place of the defaults of
    /// [`OP_MODE`] and [`FPS_TARGET`].
    pub default: &'static str,
    /// The section of the launcher: `Game`, `Controls` or `Performance`.
    pub section: &'static str,
    /// Whether the launcher shows the row first (`"level": "basic"`).
    pub basic: bool,
    /// `(env, value)`: a sub-setting, which the launcher shows only while the row of `env` has `value` (`"needs"`).
    pub needs: Option<(&'static str, &'static str)>,
}

impl Opt {
    /// Whether `v` is a value of the option: one of its choices or switch values, or a number in its range (no
    /// space at the start, no hex, not NaN), as the launcher checks it.
    pub fn accepts(&self, v: &str) -> bool {
        match self.kind {
            Kind::Choice(list) => list.iter().any(|(value, _)| *value == v),
            Kind::Toggle { on, off } => v == on || v == off,
            Kind::Range { min, max, .. } => {
                let plain =
                    !v.is_empty() && !v.starts_with(char::is_whitespace) && !v.contains(['x', 'X']);
                plain
                    && v.parse::<f64>()
                        .is_ok_and(|n| n.is_finite() && n >= min && n <= max)
            }
        }
    }
}

/// The options of [`OP_MODE`].
pub const OP_MODES: &[(&str, &str)] = &[("docked", "TV (docked)"), ("handheld", "Handheld")];

/// The options of [`LANGUAGE`]: the empty value for the Mac's language, then the console languages in the order of
/// the bits of `SupportedLanguageFlag` in the game's `control.nacp` (bit 0 American English to bit 15 Brazilian
/// Portuguese), each with its name in its own language. The build lists only the languages of the game.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("", "Same as the Mac"),
    ("en-US", "English (US)"),
    ("en-GB", "English (UK)"),
    ("ja", "日本語"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("es-419", "Español (Latinoamérica)"),
    ("es", "Español (España)"),
    ("it", "Italiano"),
    ("nl", "Nederlands"),
    ("fr-CA", "Français (Canada)"),
    ("pt", "Português"),
    ("ru", "Русский"),
    ("ko", "한국어"),
    ("zh-Hant", "繁體中文"),
    ("zh-Hans", "简体中文"),
    ("pt-BR", "Português (Brasil)"),
];

/// The options of [`LANGUAGE`] for a game whose `control.nacp` has the `SupportedLanguageFlag` `flags`: the Mac's
/// language and the languages of the game. A game without flags gets every language.
pub fn game_languages(flags: u32) -> Vec<(&'static str, &'static str)> {
    let all = flags & 0xffff == 0;
    LANGUAGES
        .iter()
        .enumerate()
        .filter(|(i, _)| *i == 0 || all || flags & (1 << (i - 1)) != 0)
        .map(|(_, l)| *l)
        .collect()
}

/// The options of [`FPS_TARGET`]. The build lists only the rates of the title ([`game_fps_targets`]).
pub const FPS_TARGETS: &[(&str, &str)] = &[("30", "30 fps"), ("60", "60 fps"), ("120", "120 fps")];

/// The options of [`FPS_TARGET`] for a title module with `fps_target` and `fps_max`: the rates up to `fps_max` and
/// the title's own. None when the title has no target (0): the game keeps its own frame limit and has nothing to
/// choose.
pub fn game_fps_targets(fps_target: u32, fps_max: u32) -> Vec<(&'static str, &'static str)> {
    if fps_target == 0 {
        return Vec::new();
    }
    let top = fps_max.max(fps_target);
    FPS_TARGETS
        .iter()
        .filter(|(v, _)| v.parse::<u32>().is_ok_and(|fps| fps <= top))
        .copied()
        .collect()
}

/// The stick dead zones in percent: 0 (none) to 50, in whole steps.
const DEADZONE: Kind = Kind::Range {
    min: 0.0,
    max: 50.0,
    step: 1.0,
};

const ON_OFF: Kind = Kind::Toggle { on: "1", off: "0" };

/// The launch options of the host, in the order of the launcher.
pub const OPTIONS: &[Opt] = &[
    Opt {
        env: OP_MODE,
        kind: Kind::Choice(OP_MODES),
        label: "Console mode",
        note: "The mode the game sees: the TV picture of the docked console or the screen of the handheld. Games \
               made for the touch screen work best in handheld mode.",
        default: "docked",
        section: "Game",
        basic: true,
        needs: None,
    },
    Opt {
        env: FPS_TARGET,
        kind: Kind::Choice(FPS_TARGETS),
        label: "Frame rate",
        note: "",
        default: "60",
        section: "Game",
        basic: true,
        needs: None,
    },
    Opt {
        env: FULLSCREEN,
        kind: ON_OFF,
        label: "Full screen",
        note: "The window starts in full screen.",
        default: "0",
        section: "Game",
        basic: true,
        needs: None,
    },
    Opt {
        env: LANGUAGE,
        kind: Kind::Choice(LANGUAGES),
        label: "Game language",
        note: "",
        default: "",
        section: "Game",
        basic: true,
        needs: None,
    },
    Opt {
        env: PAUSE_UNFOCUSED,
        kind: ON_OFF,
        label: "Pause in the background",
        note: "The game pauses when its window loses the focus.",
        default: "0",
        section: "Game",
        basic: false,
        needs: None,
    },
    Opt {
        env: RUMBLE,
        kind: ON_OFF,
        label: "Rumble",
        note: "",
        default: "1",
        section: "Controls",
        basic: true,
        needs: None,
    },
    Opt {
        env: RUMBLE_GAIN,
        kind: Kind::Range {
            min: 0.0,
            max: 3.0,
            step: 0.1,
        },
        label: "Rumble strength",
        note: "",
        default: "1.0",
        section: "Controls",
        basic: false,
        needs: Some((RUMBLE, "1")),
    },
    Opt {
        env: RUMBLE_MIN,
        kind: Kind::Range {
            min: 0.0,
            max: 0.9,
            step: 0.05,
        },
        label: "Lightest rumble",
        note: "A rumble weaker than this does not move the controller.",
        default: "0",
        section: "Controls",
        basic: false,
        needs: Some((RUMBLE, "1")),
    },
    Opt {
        env: STICK_DEADZONE_L,
        kind: DEADZONE,
        label: "Left stick dead zone",
        note: "In percent of the stick's travel. A controller's left stick reads centred inside it.",
        default: "0",
        section: "Controls",
        basic: false,
        needs: None,
    },
    Opt {
        env: STICK_DEADZONE_R,
        kind: DEADZONE,
        label: "Right stick dead zone",
        note: "In percent of the stick's travel. A controller's right stick reads centred inside it.",
        default: "0",
        section: "Controls",
        basic: false,
        needs: None,
    },
    Opt {
        env: PAD_MERGE,
        kind: ON_OFF,
        label: "All controllers are player 1",
        note: "Every controller plays as player 1.",
        default: "0",
        section: "Controls",
        basic: false,
        needs: None,
    },
    Opt {
        env: PAD_PRESS_TO_JOIN,
        kind: ON_OFF,
        label: "Press a button to join",
        note: "A new controller becomes a player at its first button press, as the lowest free player.",
        default: "0",
        section: "Controls",
        basic: false,
        needs: None,
    },
    Opt {
        env: HOME_MENU,
        kind: ON_OFF,
        label: "Home menu",
        note: "A short press of the controller's Home button opens the Home menu.",
        default: "1",
        section: "Controls",
        basic: false,
        needs: None,
    },
    Opt {
        env: HOME_QUIT,
        kind: ON_OFF,
        label: "Hold Home to quit",
        note: "Holding the controller's Home button for 2 seconds asks whether to quit the game.",
        default: "1",
        section: "Controls",
        basic: false,
        needs: None,
    },
    Opt {
        env: METAL_HUD,
        kind: ON_OFF,
        label: "Performance HUD",
        note: "Metal's HUD over the game: frame rate, frame time and GPU time.",
        default: "0",
        section: "Performance",
        basic: false,
        needs: None,
    },
];

/// The key of the saved choice of the variable `env`: `launcher.<env>`.
pub fn saved_key(env: &str) -> String {
    format!("{SAVED_PREFIX}{env}")
}

/// The option of the variable `env`.
pub fn option(env: &str) -> Option<&'static Opt> {
    OPTIONS.iter().find(|o| o.env == env)
}

/// The value of [`OP_MODE`] for the operation mode of a title module (`console` or `docked`, `handheld`).
pub fn op_mode_value(title: &str) -> &'static str {
    if title == "handheld" {
        "handheld"
    } else {
        "docked"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_is_a_value_and_every_name_is_once() {
        for (i, o) in OPTIONS.iter().enumerate() {
            assert!(o.accepts(o.default), "{}: default {:?}", o.env, o.default);
            assert!(o.env.starts_with(crate::env::PREFIX), "{}", o.env);
            assert!(
                ["Game", "Controls", "Performance"].contains(&o.section),
                "{}",
                o.env
            );
            assert!(
                !OPTIONS[..i].iter().any(|p| p.env == o.env),
                "{} twice",
                o.env
            );
        }
    }

    #[test]
    fn a_value_is_checked_as_the_launcher_checks_it() {
        let op = option(OP_MODE).unwrap();
        assert!(op.accepts("handheld") && op.accepts("docked") && !op.accepts("console"));
        let full = option(FULLSCREEN).unwrap();
        assert!(full.accepts("1") && full.accepts("0") && !full.accepts("yes"));
        let r = Opt {
            kind: Kind::Range {
                min: 0.5,
                max: 2.0,
                step: 0.25,
            },
            ..*op
        };
        assert!(r.accepts("1") && r.accepts("2.0") && r.accepts("0.5"));
        for bad in ["", " 1", "0x1", "NaN", "inf", "2.5", "0.25", "1 "] {
            assert!(!r.accepts(bad), "{bad:?}");
        }
        assert_eq!(saved_key(OP_MODE), "launcher.NXKIT_OP_MODE");
        let lang = option(LANGUAGE).unwrap();
        assert!(
            lang.accepts("")
                && lang.accepts("de")
                && lang.accepts("zh-Hant")
                && !lang.accepts("de-DE")
        );
        assert_eq!(
            (op_mode_value("console"), op_mode_value("handheld")),
            ("docked", "handheld")
        );
    }

    #[test]
    fn the_languages_follow_the_nacp_bits() {
        let de_en = game_languages(1 << 0 | 1 << 4);
        assert_eq!(
            de_en.iter().map(|l| l.0).collect::<Vec<_>>(),
            ["", "en-US", "de"]
        );
        assert_eq!(game_languages(0).len(), LANGUAGES.len());
        assert_eq!(game_languages(1 << 15).last().map(|l| l.0), Some("pt-BR"));
    }

    #[test]
    fn the_frame_rates_follow_the_title() {
        let values = |t, max| {
            game_fps_targets(t, max)
                .iter()
                .map(|r| r.0)
                .collect::<Vec<_>>()
        };
        assert_eq!(values(60, 60), ["30", "60"]);
        assert_eq!(values(30, 120), ["30", "60", "120"]);
        assert_eq!(values(120, 60), ["30", "60", "120"], "the title's own rate");
        assert_eq!(values(30, 30), ["30"]);
        assert!(values(0, 120).is_empty(), "no target: the game's own limit");
    }

    #[test]
    fn a_sub_setting_names_a_row_and_its_value() {
        for o in OPTIONS {
            if let Some((env, value)) = o.needs {
                assert!(
                    option(env).is_some_and(|p| p.accepts(value)),
                    "{}: needs {env}={value}",
                    o.env
                );
            }
        }
    }
}
