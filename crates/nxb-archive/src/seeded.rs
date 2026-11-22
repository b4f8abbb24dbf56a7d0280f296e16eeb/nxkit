//! The pipeline seeds of a title (`nxkit_pipestate::seeds`) resolved against the shaders of a build.
//!
//! A seed names shader keys and the game's state. What a draw of that state makes on the host also depends on the
//! translated shaders: the entry functions and the vertex inputs, in `<key>_b.json` of the library folder. [`Resolver`]
//! reads those once per key and resolves seeds with `nxkit_pipestate::seeds::resolve`, so an entry here is the entry
//! that the host's draw makes. The streamed archive resolves the seeds of a group when its shaders are done
//! ([`crate::stream`]) and compiles them as exact pipelines; the build writes all of them as the app's shipped list.
//!
//! The seeds are resolved for the depth mode the seed file names, or for both modes when it names none, and for drawing
//! without vertex pulling (the host's default).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nxb_pyjson::Value;
use nxkit_pipestate::list::Entry;
use nxkit_pipestate::program::{JsonView, ProgramFacts, library_stem, read};
use nxkit_pipestate::seeds::{Seed, Seeds, resolve};

/// A `nxb_pyjson` value as the view that `nxkit_pipestate::program` reads.
#[derive(Clone, Copy)]
struct View<'a>(&'a Value);

impl<'a> JsonView<'a> for View<'a> {
    fn get(self, key: &str) -> Option<Self> {
        self.0.get(key).map(View)
    }
    fn as_str(self) -> Option<&'a str> {
        self.0.as_str()
    }
    fn as_i64(self) -> Option<i64> {
        self.0.as_i64()
    }
    fn as_bool(self) -> Option<bool> {
        match self.0 {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    fn items(self) -> Vec<Self> {
        self.0.as_list().unwrap_or(&[]).iter().map(View).collect()
    }
}

/// Resolves seeds against the shaders of a library folder.
pub struct Resolver {
    lib_dir: PathBuf,
}

/// How many seeds gave an entry and how many did not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Entries made.
    pub resolved: usize,
    /// Seeds whose shaders are not in the build or have no facts.
    pub no_shader: usize,
    /// Seeds whose vertex layout or stages do not fit the shaders.
    pub refused: usize,
}

impl Resolver {
    /// A resolver over the library folder of the shader step.
    pub fn new(lib_dir: &Path) -> Resolver {
        Resolver {
            lib_dir: lib_dir.to_path_buf(),
        }
    }

    fn read(&self, key: &str, vertex: bool) -> Option<ProgramFacts> {
        let path = self
            .lib_dir
            .join(format!("{}.json", library_stem(key, false)));
        let text = std::fs::read(path).ok()?;
        let j = nxb_pyjson::parse(&String::from_utf8_lossy(&text)).ok()?;
        read(View(&j), vertex).ok()
    }

    /// The entries of the seeds that `keep` accepts, in seed order, without repeats.
    pub fn resolve(&self, seeds: &Seeds, keep: impl Fn(&Seed) -> bool) -> (Vec<Entry>, Counts) {
        let chosen: Vec<&Seed> = seeds.seeds().iter().filter(|s| keep(s)).collect();
        let mut facts: HashMap<(String, bool), Option<ProgramFacts>> = HashMap::new();
        for s in &chosen {
            let wanted: Vec<(&str, bool)> = match s {
                Seed::Shader(_) => Vec::new(),
                Seed::Compute(k) => vec![(k, false)],
                Seed::Render(r) => std::iter::once((r.vertex.as_str(), true))
                    .chain(r.fragment.as_deref().map(|f| (f, false)))
                    .collect(),
                Seed::Mesh(m) => m
                    .task
                    .as_deref()
                    .into_iter()
                    .chain([m.mesh.as_str()])
                    .chain(m.fragment.as_deref())
                    .map(|k| (k, false))
                    .collect(),
            };
            for (k, v) in wanted {
                facts
                    .entry((k.to_string(), v))
                    .or_insert_with(|| self.read(k, v));
            }
        }
        let get = |k: &str, vertex: bool| facts.get(&(k.to_string(), vertex))?.as_ref();
        let mut out = nxkit_pipestate::list::Seen::default();
        let mut n = Counts::default();
        for drawing in seeds.drawings(false) {
            for s in &chosen {
                match resolve(s, &get, drawing) {
                    Ok(entries) => {
                        for e in entries {
                            out.insert(e);
                        }
                    }
                    Err(nxkit_pipestate::seeds::Unresolved::Shader(_)) => n.no_shader += 1,
                    Err(_) => n.refused += 1,
                }
            }
        }
        n.resolved = out.len();
        (out.entries().to_vec(), n)
    }
}

/// The seeds of the seed texts of a title (family first), merged. A text that does not parse is reported in the
/// second value.
pub fn read_seeds(texts: &[&str]) -> (Seeds, Vec<String>) {
    let mut all = Seeds::default();
    let mut errors = Vec::new();
    for t in texts {
        match Seeds::parse(t) {
            Ok(s) => {
                if all.depth.is_none() {
                    all.depth = s.depth;
                }
                all.merge(&s);
            }
            Err(e) => errors.push(e),
        }
    }
    (all, errors)
}

/// The shader pairs of the render seeds, for the archive's pair list.
pub fn pairs(seeds: &Seeds) -> Vec<(String, String)> {
    seeds
        .seeds()
        .iter()
        .filter_map(|s| match s {
            Seed::Render(r) => Some((r.vertex.clone(), r.fragment.clone()?)),
            _ => None,
        })
        .collect()
}

/// The compute shaders of the seeds as library stems (`<key>_b`), for the archive's compute list.
pub fn computes(seeds: &Seeds) -> Vec<String> {
    seeds
        .seeds()
        .iter()
        .filter_map(|s| match s {
            Seed::Compute(k) => Some(library_stem(k, false)),
            _ => None,
        })
        .collect()
}

/// The shader keys of the seeds, for the shader step's order: what a pipeline names first, then the shaders seen.
pub fn shader_order(seeds: &Seeds) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut add = |k: &String| {
        if seen.insert(k.clone()) {
            out.push(k.clone());
        }
    };
    for s in seeds.seeds() {
        match s {
            Seed::Render(r) => {
                add(&r.vertex);
                r.fragment.iter().for_each(&mut add);
            }
            Seed::Mesh(m) => {
                m.task.iter().for_each(&mut add);
                add(&m.mesh);
                m.fragment.iter().for_each(&mut add);
            }
            Seed::Compute(k) => add(k),
            Seed::Shader(_) => {}
        }
    }
    for s in seeds.seeds() {
        if let Seed::Shader(k) = s {
            add(k);
        }
    }
    out
}

#[cfg(test)]
mod tests;
