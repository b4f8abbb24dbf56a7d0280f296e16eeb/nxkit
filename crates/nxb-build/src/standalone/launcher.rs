//! `launcher.json`: the launch options of the host for a launcher (`nxkit_abi::launch`).
//!
//! One row per option of the table, in the schema of the old engine's launcher, so that NX2Apple draws the rows
//! without knowing them. Two rows follow the title: the default of the console mode is the mode of the title module
//! (a touch screen game starts in handheld mode), and the game language lists the languages of the game's
//! `control.nacp`. A game with one language gets no language row.

use nxb_pyjson::Value;
use nxkit_abi::launch::{self, Kind, Opt};

/// The offset of `SupportedLanguageFlag` (a little-endian `u32`) in `control.nacp`.
const NACP_LANGUAGES: usize = 0x302c;

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

/// The row of `o`, None for a row that the game does not have.
fn row(o: &Opt, op_mode: &str, languages: &[(&str, &str)]) -> Option<Value> {
    let default = if o.env == launch::OP_MODE {
        launch::op_mode_value(op_mode)
    } else {
        o.default
    };
    let mut f = vec![
        ("env".to_string(), s(o.env)),
        ("kind".to_string(), s(o.kind.name())),
        ("label".to_string(), s(o.label)),
        ("note".to_string(), s(o.note)),
        ("default".to_string(), s(default)),
        ("section".to_string(), s(o.section)),
    ];
    if o.basic {
        f.push(("level".to_string(), s("basic")));
    }
    if let Some((env, value)) = o.needs {
        f.push(("needs".to_string(), s(&format!("{env}={value}"))));
    }
    match o.kind {
        // the Mac's language and at least two of the game's, else there is nothing to choose
        Kind::Choice(_) if o.env == launch::LANGUAGE && languages.len() < 3 => return None,
        Kind::Choice(_) if o.env == launch::LANGUAGE => {
            f.push(("options".to_string(), choices(languages)))
        }
        Kind::Choice(list) => f.push(("options".to_string(), choices(list))),
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

/// The text of `launcher.json` for the title `title_id` whose module sets the operation mode `op_mode`, with the
/// game's `control.nacp` when the extract has one.
pub(crate) fn launcher_json(title_id: u64, op_mode: &str, nacp: Option<&[u8]>) -> String {
    let languages = launch::game_languages(language_flags(nacp));
    let v = Value::Object(vec![
        ("schema".to_string(), Value::Int(i64::from(launch::SCHEMA))),
        ("game".to_string(), s(&format!("{title_id:016X}"))),
        (
            "settings".to_string(),
            Value::List(
                launch::OPTIONS
                    .iter()
                    .filter_map(|o| row(o, op_mode, &languages))
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

    #[test]
    fn the_rows_follow_the_table_and_the_title() {
        let text = launcher_json(0x0100_3460_1730_4000, "handheld", None);
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
            "no NACP: every language, so every row"
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
        let full = find(&rows, launch::FULLSCREEN).unwrap();
        assert_eq!(full.get("on").and_then(Value::as_str), Some("1"));
        assert_eq!(full.get("off").and_then(Value::as_str), Some("0"));
        let gain = find(&rows, launch::RUMBLE_GAIN).unwrap();
        assert_eq!(
            gain.get("needs").and_then(Value::as_str),
            Some("NXKIT_RUMBLE=1")
        );
        assert_eq!(gain.get("kind").and_then(Value::as_str), Some("range"));
        let docked = rows_of(launcher_json(1, "console", None));
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
        let both = rows_of(launcher_json(1, "console", Some(&nacp(1 << 0 | 1 << 4))));
        let lang = find(&both, launch::LANGUAGE).unwrap();
        let values: Vec<&str> = lang
            .get("options")
            .and_then(Value::as_list)
            .unwrap()
            .iter()
            .filter_map(|o| o.as_list()?.first()?.as_str())
            .collect();
        assert_eq!(values, ["", "en-US", "de"]);
        assert_eq!(lang.get("default").and_then(Value::as_str), Some(""));
        let one = rows_of(launcher_json(1, "console", Some(&nacp(1 << 2))));
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
}
