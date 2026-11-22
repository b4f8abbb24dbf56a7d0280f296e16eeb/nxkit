//! The rows of the old engine's lists as seeds.
//!
//! `pso_manifest.tsv` has `S <key>_b <stage>`, `C <key>_b`, `R <pso_skey hex>` and `M <task key_b|-> <pso_skey hex>`
//! rows. `shader_seen.tsv` has a shader key in the first column. The engine wrote its own render state into the keys:
//! the colour formats are the Metal formats of its targets, the blend state, masks and vertex state are NVN's. That is
//! the state a seed holds. What only the engine has is left out and counted: a merged pass (its slots are the engine's,
//! not the game's) and a target format that the host has no name for. The alpha test, logic operation, tile and border
//! flags of a key select variants of the engine's own shaders and are not part of a seed.

use nxkit_pipestate::Format;
use nxkit_pipestate::nvn::{Attribute, Blend, Stream};
use nxkit_pipestate::seeds::{ColorSeed, MeshSeed, RenderSeed, Seed, Seeds, is_key};

use crate::skey::{Skey, decode};

/// What an import left out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Left {
    /// Rows that do not parse.
    pub damaged: usize,
    /// Rows of a merged pass.
    pub merged: usize,
    /// Rows with a target format the host has no name for.
    pub format: usize,
}

impl Left {
    /// The sum.
    pub fn total(&self) -> usize {
        self.damaged + self.merged + self.format
    }
}

/// The seeds of a title and what was left out.
#[derive(Clone, Debug, Default)]
pub struct Imported {
    /// The seeds.
    pub seeds: Seeds,
    /// What was left out.
    pub left: Left,
}

fn key_b(s: &str) -> Option<String> {
    let k = s.strip_suffix("_b")?;
    is_key(k).then(|| k.to_string())
}

fn colors(k: &Skey) -> Result<Vec<Option<ColorSeed>>, ()> {
    let mut out: Vec<Option<ColorSeed>> = k
        .slots
        .iter()
        .map(|s| {
            if s.format == 0 {
                return Ok(None);
            }
            let format = Format::from_metal_value(s.format).ok_or(())?;
            let b = s.blend;
            Ok(Some(ColorSeed {
                format,
                mask: s.mask,
                blend: s.blend_on.then_some(Blend {
                    enable: true,
                    src_rgb: b[0],
                    dst_rgb: b[1],
                    op_rgb: b[4],
                    src_alpha: b[2],
                    dst_alpha: b[3],
                    op_alpha: b[5],
                }),
            }))
        })
        .collect::<Result<_, ()>>()?;
    while out.last() == Some(&None) {
        out.pop();
    }
    Ok(out)
}

fn render(k: &Skey) -> RenderSeed {
    let attributes: Vec<Attribute> = k
        .attrs
        .iter()
        .map(|a| Attribute {
            location: a.index,
            format: a.format,
            offset: a.offset,
            stream: a.stream,
        })
        .collect();
    let mut used: Vec<u32> = attributes
        .iter()
        .map(|a| a.stream)
        .filter(|s| *s < 16)
        .collect();
    used.sort_unstable();
    used.dedup();
    let streams = used
        .into_iter()
        .map(|i| Stream {
            index: i,
            stride: k.streams[i as usize].0,
            divisor: k.streams[i as usize].1,
        })
        .collect();
    RenderSeed {
        vertex: k.vs.clone(),
        fragment: (!k.fs.is_empty()).then(|| k.fs.clone()),
        samples: k.samples.max(1),
        a2c: k.a2c,
        colors: Vec::new(),
        attributes,
        streams,
    }
}

impl Imported {
    fn key_row(&mut self, hex: &str, task: Option<Option<String>>) {
        let Some(k) = decode(hex).filter(|k| !k.vs.is_empty()) else {
            self.left.damaged += 1;
            return;
        };
        if k.merged {
            self.left.merged += 1;
            return;
        }
        let Ok(colors) = colors(&k) else {
            self.left.format += 1;
            return;
        };
        let seed = match task {
            None => Seed::Render(RenderSeed {
                colors,
                ..render(&k)
            }),
            Some(task) => Seed::Mesh(MeshSeed {
                task,
                mesh: k.vs.clone(),
                fragment: (!k.fs.is_empty()).then(|| k.fs.clone()),
                samples: k.samples.max(1),
                a2c: k.a2c,
                colors,
            }),
        };
        self.seeds.insert(seed);
    }

    /// Adds the rows of a `pso_manifest.tsv`.
    pub fn manifest(&mut self, text: &str) {
        for l in text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let f: Vec<&str> = l.split('\t').collect();
            match f[..] {
                ["S", k, ..] | ["C", k] => match key_b(k) {
                    Some(k) => {
                        self.seeds.insert(if f[0] == "S" {
                            Seed::Shader(k)
                        } else {
                            Seed::Compute(k)
                        });
                    }
                    None => self.left.damaged += 1,
                },
                ["R", hex] => self.key_row(hex, None),
                ["M", task, hex] => {
                    let task = match task {
                        "-" => Some(None),
                        t => key_b(t).map(Some),
                    };
                    match task {
                        Some(t) => self.key_row(hex, Some(t)),
                        None => self.left.damaged += 1,
                    }
                }
                _ => self.left.damaged += 1,
            }
        }
    }

    /// The seeds without the shader seeds whose shader a pipeline seed names: the build translates those anyway.
    pub fn without_named_shaders(&self) -> Seeds {
        let mut named = std::collections::HashSet::new();
        for s in self.seeds.seeds() {
            match s {
                Seed::Shader(_) => {}
                Seed::Compute(k) => {
                    named.insert(k.as_str());
                }
                Seed::Render(r) => {
                    named.insert(r.vertex.as_str());
                    named.extend(r.fragment.as_deref());
                }
                Seed::Mesh(m) => {
                    named.insert(m.mesh.as_str());
                    named.extend(m.task.as_deref());
                    named.extend(m.fragment.as_deref());
                }
            }
        }
        let mut out = Seeds::default();
        for s in self.seeds.seeds() {
            if !matches!(s, Seed::Shader(k) if named.contains(k.as_str())) {
                out.insert(s.clone());
            }
        }
        out
    }

    /// Adds the shader keys of a `shader_seen.tsv`.
    pub fn shader_seen(&mut self, text: &str) {
        for l in text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let k = l.split('\t').next().unwrap_or("");
            let k = k.strip_suffix("_b").unwrap_or(k);
            if is_key(k) {
                self.seeds.insert(Seed::Shader(k.to_string()));
            } else {
                self.left.damaged += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests;
