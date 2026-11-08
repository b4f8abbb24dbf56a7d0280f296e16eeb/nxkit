//! The programs that the game gave to the backend, and what each of their stages is.
//!
//! `Backend::program_set` gives the keys of the stages of a program. The type of a stage (vertex, fragment, compute)
//! is in the `stage` field of the `_b.json` of the shader step. The walker reads it once for each key.

use std::path::PathBuf;
use std::sync::Arc;

use nxkit_nvn::backend::StageKey;

use crate::draws::FastMap;
use crate::json::{Json, parse};

/// The kind of a shader stage, as the shader step writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Vertex,
    Fragment,
    Compute,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stage {
    key: Arc<str>,
    kind: Kind,
}

/// The stages of each program.
pub(super) struct Programs {
    dir: PathBuf,
    kinds: HashMap<String, Kind>,
    by_program: HashMap<u64, Vec<Stage>>,
}

impl Programs {
    pub(super) fn new(dir: PathBuf) -> Self {
        Self { dir, kinds: HashMap::new(), by_program: HashMap::new() }
    }

    /// Replaces the stages of `program`. A stage without a key is left out.
    pub(super) fn set(&mut self, program: u64, keys: &[StageKey]) {
        let stages = keys
            .iter()
            .filter_map(|s| {
                let key: Arc<str> = s.key?.to_hex().into();
                let kind = self.kind(&key, s.index);
                Some(Stage { key, kind })
            })
            .collect();
        self.by_program.insert(program, stages);
    }

    pub(super) fn release(&mut self, program: u64) {
        self.by_program.remove(&program);
    }

    /// The key of the stage of `kind` in `program`.
    pub(super) fn find(&self, program: u64, kind: Kind) -> Option<&Arc<str>> {
        self.by_program.get(&program)?.iter().find(|s| s.kind == kind).map(|s| &s.key)
    }

    fn kind(&mut self, key: &str, index: u8) -> Kind {
        if let Some(k) = self.kinds.get(key) {
            return *k;
        }
        let kind = self.read_kind(key).unwrap_or(match index {
            0 => Kind::Vertex,
            1 => Kind::Fragment,
            _ => Kind::Other,
        });
        self.kinds.insert(key.to_owned(), kind);
        kind
    }

    fn read_kind(&self, key: &str) -> Option<Kind> {
        let text = std::fs::read_to_string(self.dir.join(format!("{key}_b.json"))).ok()?;
        let json = parse(&text)?;
        Some(match json.get("stage").and_then(Json::as_str)? {
            "vs" => Kind::Vertex,
            "ps" => Kind::Fragment,
            "cs" => Kind::Compute,
            _ => Kind::Other,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_nvn::backend::ShaderKey;

    fn stage(index: u8, key: u8) -> StageKey {
        StageKey { index, key: Some(ShaderKey([key; 8])), code: 0, code_len: 0 }
    }

    #[test]
    fn a_stage_without_a_json_takes_its_kind_from_its_index() {
        let mut p = Programs::new(PathBuf::from("/nonexistent"));
        p.set(7, &[stage(0, 1), stage(1, 2), StageKey { index: 2, key: None, code: 0, code_len: 0 }]);
        assert_eq!(p.find(7, Kind::Vertex).map(|k| &**k), Some("0101010101010101"));
        assert_eq!(p.find(7, Kind::Fragment).map(|k| &**k), Some("0202020202020202"));
        assert_eq!(p.find(7, Kind::Compute), None);
        let epoch = p.epoch();
        p.set(7, &[stage(0, 1), stage(1, 2)]);
        assert_eq!(p.epoch(), epoch);
        p.release(7);
        assert_eq!(p.find(7, Kind::Vertex).map(|k| &**k), None);
        assert!(p.epoch() > epoch && p.generation(7) > 0);
    }
}
