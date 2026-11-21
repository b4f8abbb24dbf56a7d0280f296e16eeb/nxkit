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
    /// The default value. The build puts the mode of the title module in place of the default of [`OP_MODE`].
    pub default: &'static str,
    /// The section of the launcher: `Game`, `Controls` or `Performance`.
    pub section: &'static str,
    /// Whether the launcher shows the row first (`"level": "basic"`).
    pub basic: bool,
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
    },
    Opt {
        env: FULLSCREEN,
        kind: ON_OFF,
        label: "Full screen",
        note: "The window starts in full screen.",
        default: "0",
        section: "Game",
        basic: true,
    },
    Opt {
        env: PAUSE_UNFOCUSED,
        kind: ON_OFF,
        label: "Pause in the background",
        note: "The game pauses when its window loses the focus.",
        default: "0",
        section: "Game",
        basic: false,
    },
    Opt {
        env: HOME_MENU,
        kind: ON_OFF,
        label: "Home menu",
        note: "A short press of the controller's Home button opens the Home menu.",
        default: "1",
        section: "Controls",
        basic: false,
    },
    Opt {
        env: HOME_QUIT,
        kind: ON_OFF,
        label: "Hold Home to quit",
        note: "Holding the controller's Home button for 2 seconds asks whether to quit the game.",
        default: "1",
        section: "Controls",
        basic: false,
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
        assert_eq!(
            (op_mode_value("console"), op_mode_value("handheld")),
            ("docked", "handheld")
        );
    }
}
