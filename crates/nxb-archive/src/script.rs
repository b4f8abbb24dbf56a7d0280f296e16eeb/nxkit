//! The Metal 4 pipelines script (`metal4-pipelines-script(5)`, the `.mtl4-json` that `pso_offline.py` writes and
//! `metal-tt` reads): parsed, hashed per base, and cut into per-batch scripts written as `json.dump(indent=1)` does.

use std::collections::{BTreeMap, HashMap};

use nxb_model::{Digest, Sha256};
use nxb_pyjson::Value;

use crate::cut::Base;

/// A parsed script.
pub struct Script {
    /// `libraries`: label → entry (label, path).
    pub libraries: Vec<Value>,
    /// `library_function_descriptors`.
    pub functions: Vec<Value>,
    /// `render_pipeline_descriptors`.
    pub pipelines: Vec<Value>,
    /// `compute_pipeline_descriptors` (none in a script of render bases alone).
    pub computes: Vec<Value>,
}

fn list<'a>(v: &'a Value, path: &[&str]) -> Result<&'a [Value], String> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p).ok_or_else(|| format!("script: no {p}"))?;
    }
    cur.as_list()
        .ok_or_else(|| format!("script: {} is not a list", path.join(".")))
}

fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("script: no {k}"))
}

impl Script {
    /// Parses the script text.
    pub fn parse(s: &str) -> Result<Script, String> {
        let v = nxb_pyjson::parse(s).map_err(|e| format!("script: {e:?}"))?;
        Ok(Script {
            libraries: list(&v, &["libraries"])?.to_vec(),
            functions: list(
                &v,
                &["function_descriptors", "library_function_descriptors"],
            )?
            .to_vec(),
            pipelines: list(&v, &["pipeline_descriptors", "render_pipeline_descriptors"])?.to_vec(),
            computes: match v
                .get("pipeline_descriptors")
                .and_then(|d| d.get("compute_pipeline_descriptors"))
            {
                Some(_) => list(
                    &v,
                    &["pipeline_descriptors", "compute_pipeline_descriptors"],
                )?
                .to_vec(),
                None => Vec::new(),
            },
        })
    }

    /// Library paths by label.
    pub fn library_paths(&self) -> Result<BTreeMap<String, String>, String> {
        self.libraries
            .iter()
            .map(|l| Ok((text(l, "label")?.to_string(), text(l, "path")?.to_string())))
            .collect()
    }

    /// The content hash of every base: its two functions (library bytes by digest, entry name) and the descriptor
    /// text (colour state), not the library paths or labels.
    pub fn bases(&self, lib_digest: &HashMap<String, Digest>) -> Result<Vec<Base>, String> {
        let fns: HashMap<&str, &Value> = self
            .functions
            .iter()
            .filter_map(|f| Some((text(f, "label").ok()?, f)))
            .collect();
        let mut out = Vec::with_capacity(self.pipelines.len());
        for (index, p) in self.pipelines.iter().enumerate() {
            let mut h = Sha256::new();
            let mut group = [0u8; 32];
            for k in ["vertex_function_descriptor", "fragment_function_descriptor"] {
                let r = text(p, k)?;
                let f = fns
                    .get(r.trim_start_matches("fnd:"))
                    .ok_or_else(|| format!("script: no function {r}"))?;
                let lib = text(f, "library")?.trim_start_matches("lib:");
                let d = lib_digest
                    .get(lib)
                    .ok_or_else(|| format!("script: no digest of library {lib}"))?;
                h.update(&d.0);
                h.update(text(f, "name")?.as_bytes());
                h.update(&[0]);
                if k == "vertex_function_descriptor" {
                    let mut g = Sha256::new();
                    g.update(&d.0);
                    g.update(text(f, "name")?.as_bytes());
                    group = g.finalize().0;
                }
            }
            let mut rest = p.clone();
            if let Value::Object(m) = &mut rest {
                m.retain(|(k, _)| !k.ends_with("_function_descriptor"));
            }
            h.update(nxb_pyjson::dumps(&rest).as_bytes());
            out.push(Base {
                hash: h.finalize().0,
                group,
                index,
                compute: false,
            });
        }
        Ok(out)
    }

    /// The content hash of every compute base: its function (library bytes by digest, entry name) and the rest of
    /// the descriptor text. A compute base never hashes as a render base.
    pub fn compute_bases(&self, lib_digest: &HashMap<String, Digest>) -> Result<Vec<Base>, String> {
        let fns: HashMap<&str, &Value> = self
            .functions
            .iter()
            .filter_map(|f| Some((text(f, "label").ok()?, f)))
            .collect();
        let mut out = Vec::with_capacity(self.computes.len());
        for (index, p) in self.computes.iter().enumerate() {
            let r = text(p, "compute_function_descriptor")?;
            let f = fns
                .get(r.trim_start_matches("fnd:"))
                .ok_or_else(|| format!("script: no function {r}"))?;
            let lib = text(f, "library")?.trim_start_matches("lib:");
            let d = lib_digest
                .get(lib)
                .ok_or_else(|| format!("script: no digest of library {lib}"))?;
            let mut h = Sha256::new();
            h.update(b"compute\0");
            h.update(&d.0);
            h.update(text(f, "name")?.as_bytes());
            h.update(&[0]);
            let mut rest = p.clone();
            if let Value::Object(m) = &mut rest {
                m.retain(|(k, _)| !k.ends_with("_function_descriptor"));
            }
            h.update(nxb_pyjson::dumps(&rest).as_bytes());
            let hash = h.finalize().0;
            out.push(Base {
                hash,
                group: hash,
                index,
                compute: true,
            });
        }
        Ok(out)
    }

    /// Whether the base is a recorded state ([`crate::seen`], [`crate::exact`]): a render pipeline with a member besides
    /// the two functions and the colour attachments, or with colour attachments that are not the eight unspecialized
    /// ones, which a plain base never has.
    pub fn is_extra(&self, b: &Base) -> bool {
        if b.compute {
            return false;
        }
        match self.pipelines.get(b.index) {
            Some(Value::Object(m)) => {
                let other = m.iter().any(|(k, _)| {
                    !matches!(
                        k.as_str(),
                        "vertex_function_descriptor"
                            | "fragment_function_descriptor"
                            | "color_attachments"
                    )
                });
                let unspecialized = m
                    .iter()
                    .find(|(k, _)| k == "color_attachments")
                    .and_then(|(_, v)| v.as_list())
                    .is_some_and(|l| {
                        l.len() == 8
                            && l.iter().all(|a| {
                                a.get("pixel_format").and_then(Value::as_str)
                                    == Some("Unspecialized")
                            })
                    });
                other || !unspecialized
            }
            _ => false,
        }
    }

    /// The functions of a base for a message: `<vertex lib>.<entry> + <fragment lib>.<entry>`.
    pub fn describe(&self, b: &Base) -> String {
        let p = if b.compute {
            self.computes.get(b.index)
        } else {
            self.pipelines.get(b.index)
        };
        let f = |k: &str| {
            p.and_then(|p| p.get(k))
                .and_then(Value::as_str)
                .map_or("?", |s| s.trim_start_matches("fnd:"))
                .to_string()
        };
        if b.compute {
            f("compute_function_descriptor")
        } else {
            format!(
                "{} + {}",
                f("vertex_function_descriptor"),
                f("fragment_function_descriptor")
            )
        }
    }

    /// The script of one batch, in `pso_offline.py`'s layout (libraries and functions in first-use order).
    pub fn batch_text(&self, batch: &[Base]) -> Result<String, String> {
        let fns: HashMap<&str, &Value> = self
            .functions
            .iter()
            .filter_map(|f| Some((text(f, "label").ok()?, f)))
            .collect();
        let libs: HashMap<&str, &Value> = self
            .libraries
            .iter()
            .filter_map(|l| Some((text(l, "label").ok()?, l)))
            .collect();
        let (mut lo, mut fo, mut po, mut co) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut seen_l, mut seen_f) = (
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        );
        for b in batch {
            let p = if b.compute {
                &self.computes[b.index]
            } else {
                &self.pipelines[b.index]
            };
            let keys: &[&str] = if b.compute {
                &["compute_function_descriptor"]
            } else {
                &["vertex_function_descriptor", "fragment_function_descriptor"]
            };
            for k in keys.iter().copied() {
                let label = text(p, k)?.trim_start_matches("fnd:");
                let f = *fns
                    .get(label)
                    .ok_or_else(|| format!("script: no function {label}"))?;
                let lib = text(f, "library")?.trim_start_matches("lib:");
                if seen_l.insert(lib.to_string()) {
                    lo.push(
                        (*libs
                            .get(lib)
                            .ok_or_else(|| format!("script: no library {lib}"))?)
                        .clone(),
                    );
                }
                if seen_f.insert(label.to_string()) {
                    fo.push(f.clone());
                }
            }
            if b.compute {
                co.push(p.clone());
            } else {
                po.push(p.clone());
            }
        }
        let mut descriptors = vec![("render_pipeline_descriptors".into(), Value::List(po))];
        if !co.is_empty() {
            descriptors.push(("compute_pipeline_descriptors".into(), Value::List(co)));
        }
        let v = Value::Object(vec![
            ("libraries".into(), Value::List(lo)),
            (
                "function_descriptors".into(),
                Value::Object(vec![(
                    "library_function_descriptors".into(),
                    Value::List(fo),
                )]),
            ),
            ("pipeline_descriptors".into(), Value::Object(descriptors)),
        ]);
        Ok(nxb_pyjson::dumps_indent(&v, 1))
    }
}
