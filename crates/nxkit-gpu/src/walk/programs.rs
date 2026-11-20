//! The programs that the game gave to the backend, and what each of their stages is.
//!
//! `Backend::program_set` gives the keys of the stages of a program. The type of a stage (vertex, fragment, compute,
//! task, mesh) is in the `stage` field of the `_b.json` of the shader step. The walker reads it once for each key.

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
    /// An NVN2 task shader: the object function of a mesh pipeline.
    Task,
    /// An NVN2 mesh shader.
    Mesh,
    TessControl,
    TessEvaluation,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stage {
    key: Arc<str>,
    kind: Kind,
    reflected: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct Program {
    stages: Vec<Stage>,
    /// A shader without a key may still be bound. Patch replay cannot assume it is absent.
    has_unresolved_stage: bool,
}

/// The stages of each program.
pub(super) struct Programs {
    dir: PathBuf,
    kinds: FastMap<String, Option<Kind>>,
    by_program: FastMap<u64, Program>,
    /// Counts the changes of each program. A template that looked a program up is stale when the count moved.
    generations: FastMap<u64, u64>,
    /// Counts every change of any program, so that a template checks its programs only after a change.
    epoch: u64,
}

impl Programs {
    pub(super) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            kinds: FastMap::default(),
            by_program: FastMap::default(),
            generations: FastMap::default(),
            epoch: 0,
        }
    }

    /// The number of changes of any program so far.
    pub(super) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The number of changes of `program` so far.
    pub(super) fn generation(&self, program: u64) -> u64 {
        self.generations.get(&program).copied().unwrap_or(0)
    }

    fn changed(&mut self, program: u64) {
        self.epoch += 1;
        *self.generations.entry(program).or_default() += 1;
    }

    /// Replaces the stages of `program`. Ordinary shader lookup omits stages without keys.
    /// Their presence still affects patch admission and invalidates recordings when it changes.
    pub(super) fn set(&mut self, program: u64, keys: &[StageKey]) {
        let stages = keys
            .iter()
            .filter_map(|s| {
                let key: Arc<str> = s.key?.to_hex().into();
                let (kind, reflected) = self.kind(&key, s.index);
                Some(Stage {
                    key,
                    kind,
                    reflected,
                })
            })
            .collect::<Vec<Stage>>();
        let entry = Program {
            stages,
            has_unresolved_stage: keys.iter().any(|s| s.key.is_none()),
        };
        if self.by_program.get(&program) == Some(&entry) {
            return;
        }
        self.by_program.insert(program, entry);
        self.changed(program);
    }

    pub(super) fn release(&mut self, program: u64) {
        if self.by_program.remove(&program).is_some() {
            self.changed(program);
        }
    }

    /// The stages of `program` as text, for diagnostics.
    pub(super) fn describe(&self, program: u64) -> String {
        match self.by_program.get(&program) {
            None => format!("unknown ({} programs known)", self.by_program.len()),
            Some(entry) => format!(
                "{:?}{}",
                entry
                    .stages
                    .iter()
                    .map(|s| (&*s.key, s.kind))
                    .collect::<Vec<_>>(),
                if entry.has_unresolved_stage {
                    " (unresolved stage)"
                } else {
                    ""
                }
            ),
        }
    }

    /// The key of the stage of `kind` in `program`.
    pub(super) fn find(&self, program: u64, kind: Kind) -> Option<&Arc<str>> {
        self.by_program
            .get(&program)?
            .stages
            .iter()
            .find(|s| s.kind == kind)
            .map(|s| &s.key)
    }

    /// A patch replay needs actual stage metadata, not the index-based draw fallback.
    pub(super) fn complete_patch_program(&self, program: u64) -> bool {
        program == 0
            || self.by_program.get(&program).is_some_and(|entry| {
                !entry.has_unresolved_stage
                    && !entry.stages.is_empty()
                    && entry.stages.iter().all(|s| {
                        s.reflected
                            && matches!(
                                s.kind,
                                Kind::Vertex
                                    | Kind::Fragment
                                    | Kind::TessControl
                                    | Kind::TessEvaluation
                            )
                    })
            })
    }

    fn kind(&mut self, key: &str, index: u8) -> (Kind, bool) {
        let (reflection, first) = if let Some(k) = self.kinds.get(key) {
            (*k, false)
        } else {
            let r = self.read_kind(key);
            self.kinds.insert(key.to_owned(), r);
            (r, true)
        };
        // The fallback is specific to this program binding, not to the shader key.
        let kind = reflection.unwrap_or(match index {
            0 => Kind::Vertex,
            1 => Kind::Fragment,
            _ => Kind::Other,
        });
        if first && kind == Kind::Other {
            // A stage without encoder support.
            nxkit_nvn::diag::note(
                nxkit_nvn::diag::Reason::StageIgnored,
                format_args!("program stage {key} (slot {index})"),
            );
        }
        (kind, reflection.is_some())
    }

    fn read_kind(&self, key: &str) -> Option<Kind> {
        let text = std::fs::read_to_string(self.dir.join(format!("{key}_b.json"))).ok()?;
        let json = parse(&text)?;
        // A task or mesh shader is translated as a compute job with a `mesh_stage`. Its kind names the stage.
        let mesh = json
            .get("mesh_stage")
            .and_then(|m| m.get("kind"))
            .and_then(Json::as_str);
        Some(match mesh.or(json.get("stage").and_then(Json::as_str))? {
            "vs" => Kind::Vertex,
            "ps" => Kind::Fragment,
            "cs" => Kind::Compute,
            "ts" => Kind::Task,
            "ms" => Kind::Mesh,
            "hs" => Kind::TessControl,
            "ds" => Kind::TessEvaluation,
            _ => Kind::Other,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_nvn::backend::ShaderKey;

    fn stage(index: u8, key: u8) -> StageKey {
        StageKey {
            index,
            key: Some(ShaderKey([key; 8])),
            code: 0,
            code_len: 0,
        }
    }

    #[test]
    fn a_stage_without_a_json_takes_its_kind_from_its_index() {
        let mut p = Programs::new(PathBuf::from("/nonexistent"));
        p.set(
            7,
            &[
                stage(0, 1),
                stage(1, 2),
                StageKey {
                    index: 2,
                    key: None,
                    code: 0,
                    code_len: 0,
                },
            ],
        );
        assert_eq!(
            p.find(7, Kind::Vertex).map(|k| &**k),
            Some("0101010101010101")
        );
        assert_eq!(
            p.find(7, Kind::Fragment).map(|k| &**k),
            Some("0202020202020202")
        );
        assert_eq!(p.find(7, Kind::Compute), None);
        let epoch = p.epoch();
        p.set(7, &[stage(0, 1), stage(1, 2)]);
        assert!(p.epoch() > epoch);
        let epoch = p.epoch();
        p.set(7, &[stage(0, 1), stage(1, 2)]);
        assert_eq!(p.epoch(), epoch);
        p.release(7);
        assert_eq!(p.find(7, Kind::Vertex).map(|k| &**k), None);
        assert!(p.epoch() > epoch && p.generation(7) > 0);
    }

    #[test]
    fn patch_admission_does_not_mistake_an_unresolved_stage_for_an_absent_one() {
        let mut p = Programs::new(PathBuf::from("/nonexistent"));
        // Seed the exact reflected types; no JSON or shader binaries in this test.
        for (i, kind) in [
            (1, Kind::Vertex),
            (2, Kind::TessEvaluation),
            (3, Kind::TessControl),
        ] {
            p.kinds.insert(ShaderKey([i; 8]).to_hex(), Some(kind));
        }
        let (v, d, h) = (stage(0, 1), stage(1, 2), stage(2, 3));
        let unresolved = StageKey { key: None, ..h };

        p.set(7, &[v, d]);
        assert!(p.complete_patch_program(7));
        assert!(p.find(7, Kind::TessControl).is_none());
        let generation = p.generation(7);

        p.set(7, &[v, d, unresolved]);
        assert!(!p.complete_patch_program(7));
        assert!(p.find(7, Kind::TessEvaluation).is_some());
        assert!(p.find(7, Kind::TessControl).is_none());
        assert!(p.generation(7) > generation);
        let generation = p.generation(7);
        p.set(7, &[v, d, unresolved]);
        assert_eq!(p.generation(7), generation);

        p.set(7, &[v, d, h]);
        assert!(p.complete_patch_program(7));
        assert!(
            p.find(7, Kind::TessControl).is_some(),
            "the caller must reject the reflected control shader"
        );
        assert!(p.generation(7) > generation);

        p.set(7, &[v, d, unresolved]);
        let generation = p.generation(7);
        p.set(7, &[v, d]);
        assert!(p.complete_patch_program(7));
        assert!(p.find(7, Kind::TessControl).is_none());
        assert!(p.generation(7) > generation);
    }
}
