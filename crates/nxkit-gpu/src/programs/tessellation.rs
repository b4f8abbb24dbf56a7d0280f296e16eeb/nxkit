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

/// The v1 native direct replay interfaces, separate from the diagnostic/indexed ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Link {
    Capture144,
    Domain(u32),
    Fragment(u32),
}

impl Link {
    pub(super) fn parse(json: &Json) -> Option<Self> {
        let link = json.get("tess_link")?;
        if link.get("version")?.as_i64()? != 1 {
            return None;
        }
        let mask = || u32::try_from(link.get("float4_locs")?.as_i64()?).ok();
        match (json.get("stage")?.as_str()?, link.get("role")?.as_str()?) {
            ("vs", "capture144") if json.get("entry_geometry_capture")?.as_str()? == "main0_gs" => {
                Some(Self::Capture144)
            }
            ("ds", "domain") if json.get("entry")?.as_str()? == "main0" => {
                Some(Self::Domain(mask()?))
            }
            ("ps", "fragment") if json.get("entry")?.as_str()? == "main0" => {
                Some(Self::Fragment(mask()?))
            }
            _ => None,
        }
    }

    /// No bound fragment is allowed; a bound fragment without v1 facts is not.
    pub(crate) fn direct(
        capture: Option<Self>,
        domain: Option<Self>,
        fragment: Option<Option<Self>>,
    ) -> bool {
        let (Some(Self::Capture144), Some(Self::Domain(outputs))) = (capture, domain) else {
            return false;
        };
        match fragment {
            None => true,
            Some(Some(Self::Fragment(inputs))) => inputs & !outputs == 0,
            _ => false,
        }
    }
}

#[cfg(test)]
#[test]
fn native_direct_requires_live_capture_and_typed_fragment_linkage() {
    let decode = |json: &str| Link::parse(&parse(json).unwrap());
    let capture = decode(
        r#"{"stage":"vs","entry_geometry_capture":"main0_gs","tess_link":{"version":1,"role":"capture144"}}"#,
    );
    let domain = decode(
        r#"{"stage":"ds","entry":"main0","tess_link":{"version":1,"role":"domain","float4_locs":1}}"#,
    );
    let empty_domain = decode(
        r#"{"stage":"ds","entry":"main0","tess_link":{"version":1,"role":"domain","float4_locs":0}}"#,
    );
    let fragment = |mask| {
        decode(&format!(
            r#"{{"stage":"ps","entry":"main0","tess_link":{{"version":1,"role":"fragment","float4_locs":{mask}}}}}"#
        ))
    };
    assert_eq!(capture, Some(Link::Capture144));
    assert_eq!(domain, Some(Link::Domain(1)));
    assert!(Link::direct(capture, domain, None));
    assert!(Link::direct(capture, domain, Some(fragment(1))));
    assert!(Link::direct(capture, empty_domain, Some(fragment(0))));
    assert!(!Link::direct(capture, domain, Some(fragment(2))));
    assert!(!Link::direct(capture, empty_domain, Some(fragment(1))));
    assert!(!Link::direct(capture, domain, Some(None)));
    assert!(!Link::direct(capture, None, None));
    assert!(!Link::direct(None, domain, None));
    assert_eq!(fragment(-1), None);
    for bad in [
        r#"{"stage":"vs","entry_geometry_capture":"main0_gs"}"#,
        r#"{"stage":"ps","entry":"main0","tess_link":{"version":1,"role":"fragment"}}"#,
        r#"{"stage":"ps","entry":"main0","tess_link":{"version":2,"role":"fragment","float4_locs":0}}"#,
        r#"{"stage":"ds","entry":"main0","tess_link":{"version":1,"role":"fragment","float4_locs":0}}"#,
        r#"{"stage":"vs","entry_geometry_capture":"main0","tess_link":{"version":1,"role":"capture144"}}"#,
    ] {
        assert_eq!(decode(bad), None);
    }
}

pub(super) fn strict_tess_register(json: &Json, kind: BindKind) -> bool {
    (json.get("domain_replay").is_some()
        || json.get("indexed_capture").is_some()
        || matches!(Link::parse(json), Some(Link::Domain(_))))
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
