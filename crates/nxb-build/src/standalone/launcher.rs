//! `launcher.json`: the launch options of the host for a launcher (`nxkit_abi::launch`).
//!
//! One row per option of the table, in the schema of the old engine's launcher, so that NX2Apple draws the rows
//! without knowing them. The default of the console mode is the mode of the title module: a touch screen game starts
//! in handheld mode.

use nxb_pyjson::Value;
use nxkit_abi::launch::{self, Kind, Opt};

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

fn row(o: &Opt, op_mode: &str) -> Value {
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
    match o.kind {
        Kind::Choice(list) => f.push((
            "options".to_string(),
            Value::List(
                list.iter()
                    .map(|(v, l)| Value::List(vec![s(v), s(l)]))
                    .collect(),
            ),
        )),
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
    Value::Object(f)
}

/// The text of `launcher.json` for the title `title_id` whose module sets the operation mode `op_mode`.
pub(crate) fn launcher_json(title_id: u64, op_mode: &str) -> String {
    let v = Value::Object(vec![
        ("schema".to_string(), Value::Int(i64::from(launch::SCHEMA))),
        ("game".to_string(), s(&format!("{title_id:016X}"))),
        (
            "settings".to_string(),
            Value::List(launch::OPTIONS.iter().map(|o| row(o, op_mode)).collect()),
        ),
    ]);
    let mut t = nxb_pyjson::dumps_indent(&v, 1);
    t.push('\n');
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rows_follow_the_table_and_the_title() {
        let v = nxb_pyjson::parse(&launcher_json(0x0100_3460_1730_4000, "handheld")).unwrap();
        assert_eq!(v.get("schema").and_then(Value::as_i64), Some(1));
        assert_eq!(
            v.get("game").and_then(Value::as_str),
            Some("0100346017304000")
        );
        let rows = v.get("settings").and_then(Value::as_list).unwrap();
        assert_eq!(rows.len(), launch::OPTIONS.len());
        let op = &rows[0];
        assert_eq!(op.get("env").and_then(Value::as_str), Some(launch::OP_MODE));
        assert_eq!(op.get("kind").and_then(Value::as_str), Some("choice"));
        assert_eq!(op.get("default").and_then(Value::as_str), Some("handheld"));
        assert_eq!(op.get("level").and_then(Value::as_str), Some("basic"));
        let first = op.get("options").and_then(Value::as_list).unwrap()[0]
            .as_list()
            .unwrap();
        assert_eq!(first[0].as_str(), Some("docked"));
        let full = &rows[1];
        assert_eq!(full.get("on").and_then(Value::as_str), Some("1"));
        assert_eq!(full.get("off").and_then(Value::as_str), Some("0"));
        let docked = nxb_pyjson::parse(&launcher_json(1, "console")).unwrap();
        let rows = docked.get("settings").and_then(Value::as_list).unwrap();
        assert_eq!(
            rows[0].get("default").and_then(Value::as_str),
            Some("docked")
        );
    }
}
