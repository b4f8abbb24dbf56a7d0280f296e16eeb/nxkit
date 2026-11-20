//! Reflection and library loading for the opt-in tessellation replay.

use std::sync::Arc;

use super::{BindKind, Json, Programs, ShaderLibrary, Skip, StageProgram, parse, read_stage};

impl Programs {
    /// Loads a dedicated bounds-checked indexed capture library.
    pub(crate) fn capture_indexed(&self, key: &str) -> Result<Arc<StageProgram>, Skip> {
        self.stage_with_mode(key, 3)
    }

    pub(super) fn load_indexed_capture(&self, key: &str) -> Result<StageProgram, Skip> {
        if key.len() != 16 || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(Skip::Program);
        }
        let text = std::fs::read_to_string(self.dir.join(format!("{key}_bt.json")))
            .map_err(|_| Skip::Program)?;
        let json = parse(&text).ok_or(Skip::Program)?;
        if !indexed_capture_marker(&json) {
            return Err(Skip::Program);
        }
        let lib = ShaderLibrary::from_path(&self.ctx, &self.dir.join(format!("{key}_bt.metallib")))
            .map_err(|_| Skip::Program)?;
        // Read bindings from the actual variant, never from the ordinary vertex library.
        let mut program = read_stage(&json, lib, true)?;
        program.entry_geometry_capture = Some("main0_tess_indexed".to_owned());
        program.pull = true;
        Ok(program)
    }
}

fn indexed_capture_marker(json: &Json) -> bool {
    json.get("stage").and_then(Json::as_str) == Some("vs")
        && json.get("entry").and_then(Json::as_str) == Some("main0_tess_indexed")
        && json.get("indexed_capture").is_some_and(|m| {
            m.get("version").and_then(Json::as_i64) == Some(1)
                && m.get("entry").and_then(Json::as_str) == Some("main0_tess_indexed")
        })
}

#[cfg(test)]
#[test]
fn indexed_capture_requires_its_own_vertex_reflection() {
    let accepted = r#"{"stage":"vs","entry":"main0_tess_indexed","indexed_capture":{"version":1,"entry":"main0_tess_indexed"}}"#;
    assert!(indexed_capture_marker(&parse(accepted).unwrap()));
    for bad in [
        accepted.replace("vs", "ps"),
        accepted.replace("1", "2"),
        accepted.replacen("main0_tess_indexed", "main0", 1),
    ] {
        assert!(!indexed_capture_marker(&parse(&bad).unwrap()));
    }
}

pub(super) fn strict_tess_register(json: &Json, kind: BindKind) -> bool {
    (json.get("domain_replay").is_some() || json.get("indexed_capture").is_some())
        && matches!(
            kind,
            BindKind::Cbuf
                | BindKind::Sbuf
                | BindKind::UboPtr
                | BindKind::Tex
                | BindKind::Samp
                | BindKind::SampTex
                | BindKind::Img
        )
}

pub(super) fn domain_replay(json: &Json) -> bool {
    json.get("stage").and_then(Json::as_str) == Some("ds")
        && json.get("domain_replay").is_some_and(|d| {
            d.get("version").and_then(Json::as_i64) == Some(1)
                && d.get("mode").and_then(Json::as_str) == Some("triangles")
                && d.get("spacing").and_then(Json::as_str) == Some("fractional_odd")
                && d.get("winding").and_then(Json::as_str) == Some("ccw")
                && d.get("hull").and_then(Json::as_bool) == Some(false)
                && d.get("capture_words").and_then(Json::as_i64) == Some(144)
        })
}

pub(super) fn domain_replay_indexed(json: &Json) -> bool {
    json.get("domain_replay").is_some_and(|m| {
        m.get("indexed_capture_version").and_then(Json::as_i64) == Some(1)
            && m.get("indexed_entry").and_then(Json::as_str) == Some("main0_tess_indexed_domain")
    })
}
