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

/// `1`: the game gets six channels (5.1), and the host plays them as spatial audio on the Mac's speakers and on
/// headphones (`nxkit-svc-audio`). `0`: stereo. Unset: the `spatial_audio` of the title module.
pub const SPATIAL_AUDIO: &str = "NXKIT_SPATIAL_AUDIO";

/// `1`: spatial audio on AirPods follows the movement of the head.
pub const HEAD_TRACKING: &str = "NXKIT_HEAD_TRACKING";

/// 0 to 3: the factor on the rumble of the game, 1 plays it as it is.
pub const RUMBLE_GAIN: &str = "NXKIT_RUMBLE_GAIN";

/// 0 to 0.9: the rumble below which a controller does not move.
pub const RUMBLE_MIN: &str = "NXKIT_RUMBLE_MIN";

/// `0`: a Switch Pro Controller rumbles through Core Haptics like any other controller, not with HD Rumble.
pub const HD_RUMBLE: &str = "NXKIT_HD_RUMBLE";

/// `1`: the Metal performance HUD is shown over the game.
pub const METAL_HUD: &str = "NXKIT_METAL_HUD";

/// `0`: the host does not set `MTL_CAPTURE_ENABLED=1`, so Metal makes no GPU trace capture (the `gputrace` verb of the
/// control socket then fails). Not a launcher option.
pub const GPU_CAPTURE: &str = "NXKIT_GPU_CAPTURE";

/// `off` or `metalfx`: how the present scales a picture that is smaller than the window, as `DM_PRESENT_UPSCALE` of
/// the old engine. `metalfx` scales it through MetalFX's spatial scaler, `off` by nearest pixel.
pub const PRESENT_UPSCALE: &str = "NXKIT_PRESENT_UPSCALE";

/// `30`, `60` or `120`: the frame rate in place of `fps_target` of the title module. Unset: the title's.
pub const FPS_TARGET: &str = "NXKIT_FPS_TARGET";

/// `0`, `720`, `1080`, `1440` or `2160`: the height of the size the game renders at, 16:9. `0`: the game's own size.
/// Only for a title whose family or module names the program arguments of the size (`RuntimeHooks::render_args`).
pub const RENDER_SIZE: &str = "NXKIT_RENDER_SIZE";

/// 0 to 50: the radial dead zone of the left stick of a controller, in percent of its travel. 0: none.
pub const STICK_DEADZONE_L: &str = "NXKIT_STICK_DEADZONE_L";

/// 0 to 50: the radial dead zone of the right stick of a controller, in percent of its travel. 0: none.
pub const STICK_DEADZONE_R: &str = "NXKIT_STICK_DEADZONE_R";

/// `1`: every controller is player 1.
pub const PAD_MERGE: &str = "NXKIT_PAD_MERGE";

/// `1`: a new controller becomes a player at its first button press, the lowest player that is free.
pub const PAD_PRESS_TO_JOIN: &str = "NXKIT_PAD_PRESS_TO_JOIN";

/// What the buttons of a controller send, as `DM_PAD_MAP` of the old engine: `<button>=<button>,…`. Not a row of
/// [`OPTIONS`]: a launcher sets it from its own button map editor. `nxkit_svc_input::PadMap` parses it.
pub const PAD_MAP: &str = "NXKIT_PAD_MAP";

/// Functions on other keys, as `DM_KEYMAP` of the old engine: `<function>=<macOS key code>,…`. Not a row of
/// [`OPTIONS`]: a launcher sets it from its own key map editor. `nxkit_svc_input::KeyMap` parses it.
pub const KEYMAP: &str = "NXKIT_KEYMAP";

/// The save folder of the game, as `DM_SAVE` of the old engine: the folder that is the committed save of user 1. Its
/// siblings hold the work folder, the device save, the other users' saves and the sync state. Unset: the folder
/// `~/Library/Application Support/nxkit/<title id>/save`. Not a row of [`OPTIONS`]: a launcher that keeps the save
/// elsewhere sets it.
pub const SAVE: &str = "NXKIT_SAVE";

/// `1` to `8`: the user that the game starts with, as the preselected user. A user that is not in the registry yet is
/// added at the start, with a save of its own. Unset: user 1. Not a row of [`OPTIONS`]: a launcher that offers profiles
/// sets it. The old engine has no such variable, its players choose in the game's user selector.
pub const USER: &str = "NXKIT_USER";

/// The nickname of user 1, as `DM_NICK` of the old engine. Unset: `Player`. Not a row of [`OPTIONS`].
pub const NICK: &str = "NXKIT_NICK";

/// The cloud saves folder, as `DM_SAVE_SYNC` of the old engine: a folder, `icloud` for `iCloud Drive/nxnative Saves`,
/// or `0` and unset for off. The save of user 1 and the device save go there as `<folder>/<title id>/save.dmsync`, the
/// file of the old engine. Not a row of [`OPTIONS`]: the launcher sets the folder the player chose.
pub const SAVE_SYNC: &str = "NXKIT_SAVE_SYNC";

/// The player's choice before the start when both saves changed, as `DM_SAVE_SYNC_CHOICE` of the old engine: `ask`,
/// `cloud:<hash>`, `local:<hash>`, `local:none` or `offline`. Not a row of [`OPTIONS`]: the launcher sets it from the
/// report of `nxkit-host --save-sync-report`.
pub const SAVE_SYNC_CHOICE: &str = "NXKIT_SAVE_SYNC_CHOICE";

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
    /// The default value. The build puts the mode, the frame rate and the spatial audio of the title module in place
    /// of the defaults of [`OP_MODE`], [`FPS_TARGET`] and [`SPATIAL_AUDIO`].
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

/// The options of [`PRESENT_UPSCALE`], with the labels of the old engine.
pub const PRESENT_UPSCALES: &[(&str, &str)] = &[("off", "Standard"), ("metalfx", "MetalFX")];

/// The options of [`RENDER_SIZE`].
pub const RENDER_SIZES: &[(&str, &str)] = &[
    ("0", "The game's own"),
    ("720", "1280x720"),
    ("1080", "1920x1080"),
    ("1440", "2560x1440"),
    ("2160", "3840x2160"),
];

/// The width and height of a [`RENDER_SIZE`] value: `None` for `0` (the game's own size) and for no option.
pub fn render_size(value: &str) -> Option<(u32, u32)> {
    let h: u32 = value.parse().ok()?;
    (h != 0 && RENDER_SIZES.iter().any(|(v, _)| *v == value)).then_some((h * 16 / 9, h))
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
        env: RENDER_SIZE,
        kind: Kind::Choice(RENDER_SIZES),
        label: "Render resolution",
        note: "The size the game draws at, menus included. Larger is sharper and costs more GPU time.",
        default: "0",
        section: "Game",
        basic: false,
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
        env: PRESENT_UPSCALE,
        kind: Kind::Choice(PRESENT_UPSCALES),
        label: "Upscaling",
        note: "MetalFX sharpens the picture where the window is larger than the game renders it. Uses more GPU time and \
               can increase heat.",
        default: "off",
        section: "Game",
        basic: false,
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
        env: SPATIAL_AUDIO,
        kind: ON_OFF,
        label: "Spatial audio",
        note: "The game plays in 5.1 surround, and the Mac makes it spatial on its speakers and on headphones such \
               as AirPods. Only games with surround sound gain from it.",
        default: "0",
        section: "Game",
        basic: false,
        needs: None,
    },
    Opt {
        env: HEAD_TRACKING,
        kind: ON_OFF,
        label: "Head tracking",
        note: "On AirPods, the sound stays in place when you turn your head.",
        default: "0",
        section: "Game",
        basic: false,
        needs: Some((SPATIAL_AUDIO, "1")),
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
        env: HD_RUMBLE,
        kind: ON_OFF,
        label: "HD Rumble",
        note: "A Switch Pro Controller plays the rumble of the game as on the Switch.",
        default: "1",
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
#[path = "launch_tests.rs"]
mod tests;
