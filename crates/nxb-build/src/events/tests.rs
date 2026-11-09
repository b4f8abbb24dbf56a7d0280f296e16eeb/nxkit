//! Tests of the event writer.

use super::*;
use std::sync::Arc;

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Schema: every line is one JSON object with `ev`, `code`, `severity`, `t` last, and the pair is a row of the table.
#[test]
fn every_event_has_a_table_code_and_old_fields_stay() {
    let buf = Buf::default();
    let p = Progress::new(Mode::Json, Box::new(buf.clone()));
    p.plan(2);
    p.stage("extract", "first");
    p.set(Update {
        step: Some("second"),
        done: Some(1),
        total: Some(4),
        unit: Some("files"),
        note: None,
    });
    p.say("plain");
    p.say("[RED] broken");
    p.say("[YELLOW] careful");
    p.end("done", true);
    p.stage("pack", "");
    p.end("boom", false);
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    let mut seen = Vec::new();
    for line in text.lines() {
        let v = nxb_pyjson::parse(line).unwrap();
        let Value::Object(fields) = &v else {
            panic!("{line}")
        };
        let ev = v.get("ev").and_then(Value::as_str).unwrap();
        let code = v.get("code").and_then(Value::as_str).unwrap();
        let sev = v.get("severity").and_then(Value::as_str).unwrap();
        let row = nxb_obs::codes::lookup(code).unwrap();
        assert_eq!((row.ev, row.severity.name()), (ev, sev), "{line}");
        assert_eq!(fields.last().unwrap().0, "t");
        seen.push((ev.to_string(), code.to_string()));
    }
    let want = |ev: &str, code: &str| {
        assert!(
            seen.contains(&(ev.into(), code.into())),
            "{ev} {code} in {seen:?}"
        )
    };
    want("stage", "NXB1001");
    want("step", "NXB1004");
    want("progress", "NXB1005");
    want("say", "NXB1006");
    want("say", "NXB1007");
    want("say", "NXB1008");
    want("stage_end", "NXB1002");
    want("stage_end", "NXB1003");
}
