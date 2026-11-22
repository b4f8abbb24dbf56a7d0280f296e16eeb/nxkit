//! `launcher.json`: the launch options of the host for a launcher (`nxkit_abi::launch`).
//!
//! One row per option of the table, in the schema of the old engine's launcher, so that NX2Apple draws the rows
//! without knowing them. Four rows follow the title: the default of the console mode is the mode of the title module
//! (a touch screen game starts in handheld mode), the game language lists the languages of the game's
//! `control.nacp`, the frame rate lists the rates up to `fps_max` of the title module, with its `fps_target` as
//! the default, and spatial audio is on by default when the title module sets `spatial_audio`. A game with one
//! language gets no language row, and a game without a target frame rate or with one rate gets no frame rate row. The
//! render size row is there only when the family or the title names the program arguments of the size.

use nxb_pyjson::Value;
use nxkit_abi::launch::{self, Kind, Opt};

/// The offset of `SupportedLanguageFlag` (a little-endian `u32`) in `control.nacp`.
const NACP_LANGUAGES: usize = 0x302c;

type Choices = Vec<(&'static str, &'static str)>;

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

/// `SupportedLanguageFlag` of `control.nacp`, 0 when there is no NACP or it is too short.
fn language_flags(nacp: Option<&[u8]>) -> u32 {
    nacp.and_then(|n| n.get(NACP_LANGUAGES..NACP_LANGUAGES + 4))
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

fn choices(list: &[(&str, &str)]) -> Value {
    Value::List(
        list.iter()
            .map(|(v, l)| Value::List(vec![s(v), s(l)]))
            .collect(),
    )
}

/// What the rows take from the game.
struct Game {
    op_mode: &'static str,
    fps_target: u32,
    render_size: u32,
    spatial_audio: bool,
    languages: Choices,
    fps_targets: Choices,
    render_args: bool,
}

impl Game {
    fn new(settings: &nxtitle::Settings, nacp: Option<&[u8]>, render_size: bool) -> Self {
        Game {
            render_args: render_size,
            op_mode: settings.op_mode,
            fps_target: settings.fps_target,
            render_size: settings.render_size,
            spatial_audio: settings.spatial_audio,
            languages: launch::game_languages(language_flags(nacp)),
            fps_targets: launch::game_fps_targets(settings.fps_target, settings.fps_max),
        }
    }

    /// The default of `o`: the title's for the console mode, the frame rate, the render size and spatial audio.
    fn default(&self, o: &Opt) -> &'static str {
        match o.env {
            launch::OP_MODE => launch::op_mode_value(self.op_mode),
            launch::SPATIAL_AUDIO => match o.kind {
                Kind::Toggle { on, off } => {
                    if self.spatial_audio {
                        on
                    } else {
                        off
                    }
                }
                _ => o.default,
            },
            launch::RENDER_SIZE => launch::RENDER_SIZES
                .iter()
                .find(|(v, _)| v.parse() == Ok(self.render_size))
                .map_or(o.default, |r| r.0),
            launch::FPS_TARGET => self
                .fps_targets
                .iter()
                .find(|(v, _)| v.parse() == Ok(self.fps_target))
                .map_or(o.default, |r| r.0),
            _ => o.default,
        }
    }

    /// The options of the choice `o` with the table's `list`, None when the game has nothing to choose.
    fn options<'a>(
        &'a self,
        o: &Opt,
        list: &'a [(&'a str, &'a str)],
    ) -> Option<&'a [(&'a str, &'a str)]> {
        match o.env {
            // the Mac's language and at least two of the game's
            launch::LANGUAGE => (self.languages.len() >= 3).then_some(&self.languages[..]),
            launch::FPS_TARGET => (self.fps_targets.len() >= 2).then_some(&self.fps_targets[..]),
            launch::RENDER_SIZE => self.render_args.then_some(list),
            _ => Some(list),
        }
    }
}

/// The row of `o`, None for a row that the game does not have.
fn row(o: &Opt, game: &Game) -> Option<Value> {
    let mut f = vec![
        ("env".to_string(), s(o.env)),
        ("kind".to_string(), s(o.kind.name())),
        ("label".to_string(), s(o.label)),
        ("note".to_string(), s(o.note)),
        ("default".to_string(), s(game.default(o))),
        ("section".to_string(), s(o.section)),
    ];
    if o.basic {
        f.push(("level".to_string(), s("basic")));
    }
    if let Some((env, value)) = o.needs {
        f.push(("needs".to_string(), s(&format!("{env}={value}"))));
    }
    match o.kind {
        Kind::Choice(list) => f.push(("options".to_string(), choices(game.options(o, list)?))),
        Kind::Toggle { on, off } => {
            f.push(("on".to_string(), s(on)));
            f.push(("off".to_string(), s(off)));
        }
        Kind::Range { min, max, step } => {
            f.push(("min".to_string(), Value::Float(min)));
            f.push(("max".to_string(), Value::Float(max)));
            f.push(("step".to_string(), Value::Float(step)));
        }
    }
    Some(Value::Object(f))
}

/// The text of `launcher.json` for the title `title_id` with the run settings of its module, with the game's
/// `control.nacp` when the extract has one. `render_size`: the title names the program arguments of its render size.
pub(crate) fn launcher_json(
    title_id: u64,
    settings: &nxtitle::Settings,
    nacp: Option<&[u8]>,
    render_size: bool,
) -> String {
    let game = Game::new(settings, nacp, render_size);
    let v = Value::Object(vec![
        ("schema".to_string(), Value::Int(i64::from(launch::SCHEMA))),
        ("game".to_string(), s(&format!("{title_id:016X}"))),
        (
            "settings".to_string(),
            Value::List(
                launch::OPTIONS
                    .iter()
                    .filter_map(|o| row(o, &game))
                    .collect(),
            ),
        ),
    ]);
    let mut t = nxb_pyjson::dumps_indent(&v, 1);
    t.push('\n');
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(json: &str) -> Vec<Value> {
        let v = nxb_pyjson::parse(json).unwrap();
        v.get("settings").and_then(Value::as_list).unwrap().to_vec()
    }

    fn find<'a>(rows: &'a [Value], env: &str) -> Option<&'a Value> {
        rows.iter()
            .find(|r| r.get("env").and_then(Value::as_str) == Some(env))
    }

    fn nacp(flags: u32) -> Vec<u8> {
        let mut n = vec![0; 0x4000];
        n[NACP_LANGUAGES..NACP_LANGUAGES + 4].copy_from_slice(&flags.to_le_bytes());
        n
    }

    fn title(op_mode: &'static str, fps_target: u32, fps_max: u32) -> nxtitle::Settings {
        nxtitle::Settings {
            op_mode,
            fps_target,
            fps_max,
            ..nxtitle::Settings::DEFAULT
        }
    }

    fn values(row: &Value) -> Vec<&str> {
        row.get("options")
            .and_then(Value::as_list)
            .unwrap()
            .iter()
            .filter_map(|o| o.as_list()?.first()?.as_str())
            .collect()
    }

    #[test]
    fn the_rows_follow_the_table_and_the_title() {
        let text = launcher_json(
            0x0100_3460_1730_4000,
            &title("handheld", 60, 60),
            None,
            true,
        );
        let v = nxb_pyjson::parse(&text).unwrap();
        assert_eq!(v.get("schema").and_then(Value::as_i64), Some(1));
        assert_eq!(
            v.get("game").and_then(Value::as_str),
            Some("0100346017304000")
        );
        let rows = rows(&text);
        assert_eq!(
            rows.len(),
            launch::OPTIONS.len(),
            "no NACP: every language, two frame rates and a render size, so every row"
        );
        let op = &rows[0];
        assert_eq!(op.get("env").and_then(Value::as_str), Some(launch::OP_MODE));
        assert_eq!(op.get("kind").and_then(Value::as_str), Some("choice"));
        assert_eq!(op.get("default").and_then(Value::as_str), Some("handheld"));
        assert_eq!(op.get("level").and_then(Value::as_str), Some("basic"));
        let first = op.get("options").and_then(Value::as_list).unwrap()[0]
            .as_list()
            .unwrap();
        assert_eq!(first[0].as_str(), Some("docked"));
        let size = find(&rows, launch::RENDER_SIZE).unwrap();
        assert_eq!(size.get("default").and_then(Value::as_str), Some("0"));
        let mut big = title("console", 60, 60);
        big.render_size = 2160;
        let big = self::rows(&launcher_json(1, &big, None, true));
        let size = find(&big, launch::RENDER_SIZE).unwrap();
        assert_eq!(
            size.get("default").and_then(Value::as_str),
            Some("2160"),
            "the title's own default"
        );
        let none = self::rows(&launcher_json(1, &title("console", 60, 60), None, false));
        assert!(
            find(&none, launch::RENDER_SIZE).is_none(),
            "no render arguments, no row"
        );
        let full = find(&rows, launch::FULLSCREEN).unwrap();
        assert_eq!(full.get("on").and_then(Value::as_str), Some("1"));
        assert_eq!(full.get("off").and_then(Value::as_str), Some("0"));
        let gain = find(&rows, launch::RUMBLE_GAIN).unwrap();
        assert_eq!(
            gain.get("needs").and_then(Value::as_str),
            Some("NXKIT_RUMBLE=1")
        );
        assert_eq!(gain.get("kind").and_then(Value::as_str), Some("range"));
        let docked = rows_of(launcher_json(1, &title("console", 0, 60), None, false));
        assert_eq!(
            docked[0].get("default").and_then(Value::as_str),
            Some("docked")
        );
    }

    fn rows_of(text: String) -> Vec<Value> {
        rows(&text)
    }

    #[test]
    fn the_language_row_lists_the_languages_of_the_game() {
        let console = title("console", 0, 60);
        let both = rows_of(launcher_json(
            1,
            &console,
            Some(&nacp(1 << 0 | 1 << 4)),
            false,
        ));
        let lang = find(&both, launch::LANGUAGE).unwrap();
        assert_eq!(values(lang), ["", "en-US", "de"]);
        assert_eq!(lang.get("default").and_then(Value::as_str), Some(""));
        let one = rows_of(launcher_json(1, &console, Some(&nacp(1 << 2)), false));
        assert!(
            find(&one, launch::LANGUAGE).is_none(),
            "one language: nothing to choose"
        );
        assert_eq!(
            language_flags(Some(&[0; 16])),
            0,
            "a short NACP has no flags"
        );
    }

    #[test]
    fn the_frame_rate_row_lists_the_rates_of_the_title() {
        let high = rows_of(launcher_json(1, &title("console", 30, 120), None, false));
        let fps = find(&high, launch::FPS_TARGET).unwrap();
        assert_eq!(values(fps), ["30", "60", "120"]);
        assert_eq!(fps.get("default").and_then(Value::as_str), Some("30"));
        assert_eq!(fps.get("level").and_then(Value::as_str), Some("basic"));
        let own = rows_of(launcher_json(1, &title("console", 0, 120), None, false));
        assert!(
            find(&own, launch::FPS_TARGET).is_none(),
            "no target: the game's own limit"
        );
        let one = rows_of(launcher_json(1, &title("console", 30, 30), None, false));
        assert!(
            find(&one, launch::FPS_TARGET).is_none(),
            "one rate: nothing to choose"
        );
        let dz = find(&high, launch::STICK_DEADZONE_L).unwrap();
        assert_eq!(dz.get("max"), Some(&Value::Float(50.0)));
    }

    #[test]
    fn the_spatial_audio_row_has_the_default_of_the_title() {
        let default = |spatial_audio| {
            let t = nxtitle::Settings {
                spatial_audio,
                ..title("console", 0, 60)
            };
            let rows = rows_of(launcher_json(1, &t, None, false));
            let row = find(&rows, launch::SPATIAL_AUDIO).unwrap();
            row.get("default")
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        assert_eq!(default(false).as_deref(), Some("0"));
        assert_eq!(default(true).as_deref(), Some("1"));
    }
}
