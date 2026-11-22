//! The pipeline seeds of a title and how the build resolves them into pipeline list entries.
//!
//! A seed is the state of a pipeline as the game set it, in the terms of NVN: the shader keys, the colour targets with
//! their formats, NVN blend state and write masks, the sample count, alpha to coverage, and the NVN vertex attributes
//! and streams. It names no Metal entry function, library or vertex layout: those depend on the translation of the
//! shaders, which a build makes. [`resolve`] turns a seed into the entry that a draw of that state makes on the host,
//! with the rules of [`crate::draw`] and [`crate::nvn`] and the facts of the translated shaders
//! ([`crate::program`]). The build archives the resolved entries and ships them as the app's list
//! ([`crate::list::SHIPPED_LIST`]).
//!
//! A title module keeps its seeds as text (`pipelines.seeds`):
//!
//! ```text
//! nxkit-pipeline-seeds 1
//! # a comment
//! D <gl|zero>
//! S <shader key>
//! C <compute shader key>
//! R <vertex key> <fragment key|-> <samples> <a2c> <colors> <attributes> <streams>
//! M <task key|-> <mesh key> <fragment key|-> <samples> <a2c> <colors>
//! ```
//!
//! Fields are separated by a tab. `colors` is `-` or a comma separated list of slots, `_` for an empty slot and else
//! `<Format>:<mask in hex>:<blend>`, the format named as in the pipeline list, the blend being `-` or the six NVN values
//! `src_rgb.dst_rgb.op_rgb.src_alpha.dst_alpha.op_alpha` in decimal. `attributes` is `-` or a comma separated list of
//! `<location>:<NVN format in hex>:<offset>:<stream>`, `streams` is `-` or a comma separated list of
//! `<index>:<stride>:<divisor>`. An `S` line names a shader seen in play: the build translates it early. The `D` line says
//! the depth mode the game sets on its device: `zero` (0 to 1, the vertex functions that keep the clip depth) or `gl`
//! (-1 to 1). Without it the build resolves each seed for both. A line that does not parse is left out.

use std::collections::HashSet;

use crate::draw::{
    PASS_SAMPLES, color_targets, fragment_entry, mesh_entry, pass_colors, vertex_entry,
};
use crate::format::Format;
use crate::list::{ComputeRec, Entry, MeshRec, RenderRec};
use crate::nvn::{Attribute, Blend, LayoutError, Stream, vertex_descriptor};
use crate::program::{ProgramFacts, library_stem};
use crate::state::{TopologyClass, VertexDescriptor};

/// The first line of a seed file.
pub const HEADER: &str = "nxkit-pipeline-seeds 1";

/// The name of the seed file of a title module.
pub const FILE: &str = "pipelines.seeds";

/// One colour target of a seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColorSeed {
    /// The format of the target.
    pub format: Format,
    /// The write mask, bits R=1, G=2, B=4, A=8.
    pub mask: u8,
    /// The NVN blend state, none when blending is off.
    pub blend: Option<Blend>,
}

/// The state of a render pipeline.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RenderSeed {
    /// The key of the vertex shader.
    pub vertex: String,
    /// The key of the fragment shader.
    pub fragment: Option<String>,
    /// The sample count of the targets.
    pub samples: u32,
    /// Alpha to coverage.
    pub a2c: bool,
    /// The colour targets by slot.
    pub colors: Vec<Option<ColorSeed>>,
    /// The vertex attributes that the vertex shader reads.
    pub attributes: Vec<Attribute>,
    /// The vertex streams that they read.
    pub streams: Vec<Stream>,
}

/// The state of a mesh pipeline.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MeshSeed {
    /// The key of the task shader.
    pub task: Option<String>,
    /// The key of the mesh shader.
    pub mesh: String,
    /// The key of the fragment shader.
    pub fragment: Option<String>,
    /// The sample count of the targets.
    pub samples: u32,
    /// Alpha to coverage.
    pub a2c: bool,
    /// The colour targets by slot.
    pub colors: Vec<Option<ColorSeed>>,
}

/// One seed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Seed {
    /// A shader seen in play.
    Shader(String),
    /// A compute shader.
    Compute(String),
    /// A render pipeline.
    Render(RenderSeed),
    /// A mesh pipeline.
    Mesh(MeshSeed),
}

/// The depth mode a game sets on its device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthMode {
    /// -1 to 1, the default of NVN.
    Gl,
    /// 0 to 1.
    Zero,
}

/// The seeds of a title, without repeats, in their order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seeds {
    /// The depth mode of the game, when it is known.
    pub depth: Option<DepthMode>,
    seeds: Vec<Seed>,
    index: HashSet<Seed>,
}

impl Seeds {
    /// Adds a seed. False when it is there already.
    pub fn insert(&mut self, s: Seed) -> bool {
        if !self.index.insert(s.clone()) {
            return false;
        }
        self.seeds.push(s);
        true
    }

    /// The seeds in order.
    pub fn seeds(&self) -> &[Seed] {
        &self.seeds
    }

    /// The number of seeds.
    pub fn len(&self) -> usize {
        self.seeds.len()
    }

    /// True without seeds.
    pub fn is_empty(&self) -> bool {
        self.seeds.is_empty()
    }

    /// Adds the seeds of `other` that this list lacks, after its own. Returns how many were added.
    pub fn merge(&mut self, other: &Seeds) -> usize {
        other
            .seeds
            .iter()
            .filter(|s| self.insert((*s).clone()))
            .count()
    }

    /// The text of the file, with `comments` as `#` lines after the header.
    pub fn to_text(&self, comments: &[String]) -> String {
        let mut o = String::with_capacity(self.seeds.len() * 96 + 64);
        o.push_str(HEADER);
        o.push('\n');
        for c in comments {
            for l in c.lines() {
                o.push_str("# ");
                o.push_str(l);
                o.push('\n');
            }
        }
        match self.depth {
            Some(DepthMode::Gl) => o.push_str("D\tgl\n"),
            Some(DepthMode::Zero) => o.push_str("D\tzero\n"),
            None => {}
        }
        for s in &self.seeds {
            o.push_str(&line(s));
            o.push('\n');
        }
        o
    }

    /// Reads the text of a file. A line that does not parse is left out, `#` lines are comments; a text of another
    /// version is an error.
    pub fn parse(text: &str) -> Result<Seeds, String> {
        let mut lines = text.lines();
        match lines.next() {
            Some(HEADER) => {}
            Some(h) => return Err(format!("unknown header {h:?}")),
            None => return Err("empty".into()),
        }
        let mut s = Seeds::default();
        for l in lines.filter(|l| !l.starts_with('#')) {
            match l {
                "D\tgl" => s.depth = Some(DepthMode::Gl),
                "D\tzero" => s.depth = Some(DepthMode::Zero),
                _ => {}
            }
            if let Some(seed) = parse_line(l) {
                s.insert(seed);
            }
        }
        Ok(s)
    }
}

/// True for a shader key: 16 lower-case hex digits.
pub fn is_key(s: &str) -> bool {
    s.len() == 16
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn opt_text(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("-")
}

fn colors_text(c: &[Option<ColorSeed>]) -> String {
    if c.is_empty() {
        return "-".into();
    }
    c.iter()
        .map(|slot| match slot {
            None => "_".to_string(),
            Some(t) => {
                let blend = match &t.blend {
                    None => "-".to_string(),
                    Some(b) => format!(
                        "{}.{}.{}.{}.{}.{}",
                        b.src_rgb, b.dst_rgb, b.op_rgb, b.src_alpha, b.dst_alpha, b.op_alpha
                    ),
                };
                format!("{:?}:{:x}:{blend}", t.format, t.mask)
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn list_text<T>(items: &[T], f: impl Fn(&T) -> String) -> String {
    if items.is_empty() {
        "-".into()
    } else {
        items.iter().map(f).collect::<Vec<_>>().join(",")
    }
}

/// The line of a seed.
pub fn line(s: &Seed) -> String {
    match s {
        Seed::Shader(k) => format!("S\t{k}"),
        Seed::Compute(k) => format!("C\t{k}"),
        Seed::Render(r) => format!(
            "R\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.vertex,
            opt_text(&r.fragment),
            r.samples,
            u8::from(r.a2c),
            colors_text(&r.colors),
            list_text(&r.attributes, |a| format!(
                "{}:{:x}:{}:{}",
                a.location, a.format, a.offset, a.stream
            )),
            list_text(&r.streams, |s| format!(
                "{}:{}:{}",
                s.index, s.stride, s.divisor
            )),
        ),
        Seed::Mesh(m) => format!(
            "M\t{}\t{}\t{}\t{}\t{}\t{}",
            opt_text(&m.task),
            m.mesh,
            opt_text(&m.fragment),
            m.samples,
            u8::from(m.a2c),
            colors_text(&m.colors),
        ),
    }
}

fn key(s: &str) -> Option<String> {
    is_key(s).then(|| s.to_string())
}

fn opt_key(s: &str) -> Option<Option<String>> {
    if s == "-" {
        Some(None)
    } else {
        key(s).map(Some)
    }
}

fn flag(s: &str) -> Option<bool> {
    match s {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn color_of(slot: &str) -> Option<Option<ColorSeed>> {
    if slot == "_" {
        return Some(None);
    }
    let mut p = slot.splitn(3, ':');
    let name = p.next()?;
    let format = *Format::SIMPLE.iter().find(|f| format!("{f:?}") == name)?;
    let mask = u8::from_str_radix(p.next()?, 16).ok()? & 0xF;
    let blend = match p.next()? {
        "-" => None,
        b => {
            let n: Vec<u32> = b
                .split('.')
                .map(|v| v.parse().ok())
                .collect::<Option<_>>()?;
            let [src_rgb, dst_rgb, op_rgb, src_alpha, dst_alpha, op_alpha] = n[..] else {
                return None;
            };
            Some(Blend {
                enable: true,
                src_rgb,
                dst_rgb,
                op_rgb,
                src_alpha,
                dst_alpha,
                op_alpha,
            })
        }
    };
    Some(Some(ColorSeed {
        format,
        mask,
        blend,
    }))
}

fn colors_of(s: &str) -> Option<Vec<Option<ColorSeed>>> {
    if s == "-" {
        return Some(Vec::new());
    }
    s.split(',').map(color_of).collect()
}

fn items_of<T>(s: &str, f: impl Fn(&[&str]) -> Option<T>) -> Option<Vec<T>> {
    if s == "-" {
        return Some(Vec::new());
    }
    s.split(',')
        .map(|i| f(&i.split(':').collect::<Vec<_>>()))
        .collect()
}

/// One line of a seed file, `None` for a line that does not parse.
pub fn parse_line(l: &str) -> Option<Seed> {
    let f: Vec<&str> = l.split('\t').collect();
    match (*f.first()?, f.len()) {
        ("S", 2) => Some(Seed::Shader(key(f[1])?)),
        ("C", 2) => Some(Seed::Compute(key(f[1])?)),
        ("R", 8) => Some(Seed::Render(RenderSeed {
            vertex: key(f[1])?,
            fragment: opt_key(f[2])?,
            samples: f[3].parse().ok().filter(|n| *n > 0)?,
            a2c: flag(f[4])?,
            colors: colors_of(f[5])?,
            attributes: items_of(f[6], |p| match p {
                [l, fmt, off, st] => Some(Attribute {
                    location: l.parse().ok()?,
                    format: u32::from_str_radix(fmt, 16).ok()?,
                    offset: off.parse().ok()?,
                    stream: st.parse().ok()?,
                }),
                _ => None,
            })?,
            streams: items_of(f[7], |p| match p {
                [i, stride, div] => Some(Stream {
                    index: i.parse().ok()?,
                    stride: stride.parse().ok()?,
                    divisor: div.parse().ok()?,
                }),
                _ => None,
            })?,
        })),
        ("M", 7) => Some(Seed::Mesh(MeshSeed {
            task: opt_key(f[1])?,
            mesh: key(f[2])?,
            fragment: opt_key(f[3])?,
            samples: f[4].parse().ok().filter(|n| *n > 0)?,
            a2c: flag(f[5])?,
            colors: colors_of(f[6])?,
        })),
        _ => None,
    }
}

impl Seeds {
    /// The ways of drawing to resolve the seeds for: the known depth mode, or both.
    pub fn drawings(&self, pull: bool) -> Vec<Drawing> {
        let modes: &[bool] = match self.depth {
            Some(DepthMode::Gl) => &[false],
            Some(DepthMode::Zero) => &[true],
            None => &[false, true],
        };
        modes
            .iter()
            .map(|&zero_to_one| Drawing { zero_to_one, pull })
            .collect()
    }
}

/// How the app draws, which the resolution of a seed depends on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drawing {
    /// The device is in the depth mode 0 to 1: the vertex and mesh functions that keep the clip depth.
    pub zero_to_one: bool,
    /// Vertex functions pull their inputs (`_bp` libraries, no vertex layout).
    pub pull: bool,
}

/// Why a seed gives no entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unresolved {
    /// A shader of the seed is not in the build, or its facts cannot be read.
    Shader(String),
    /// A shader is of the wrong kind for its stage (a mesh shader as a vertex shader, or the other way).
    Stage(String),
    /// The vertex layout cannot be translated.
    Layout(LayoutError),
}

fn colors_and_targets(c: &[Option<ColorSeed>]) -> (Vec<Option<Format>>, Vec<Blend>, Vec<u8>) {
    let formats = pass_colors(&c.iter().map(|s| s.map(|s| s.format)).collect::<Vec<_>>());
    let blend = c
        .iter()
        .map(|s| s.and_then(|s| s.blend).unwrap_or_default())
        .collect();
    let masks = c.iter().map(|s| s.map_or(0xF, |s| s.mask)).collect();
    (formats, blend, masks)
}

/// The pipeline list entries that a draw in the state of `seed` makes on the host, without their keys (the host
/// computes them). `facts` gives the facts of a shader key (`true`: read as a vertex shader). A shader seed gives none.
///
/// A seed does not say whether the draw had a geometry shader that picks the layer (the host then runs the layer entry
/// of the vertex shader, `crate::draw::vertex_entry`). For a vertex shader with a layer entry both draws are given.
pub fn resolve<'a>(
    seed: &Seed,
    facts: &impl Fn(&str, bool) -> Option<&'a ProgramFacts>,
    drawing: Drawing,
) -> Result<Vec<Entry>, Unresolved> {
    let Seed::Render(r) = seed else {
        return Ok(resolve_one(seed, facts, drawing, false)?
            .into_iter()
            .collect());
    };
    let layered = facts(&r.vertex, true).is_some_and(|v| v.entry_layer.is_some())
        && !(drawing.pull && facts(&r.vertex, true).is_some_and(|v| v.pull.is_some()));
    let mut out: Vec<Entry> = resolve_one(seed, facts, drawing, false)?
        .into_iter()
        .collect();
    if layered {
        out.extend(resolve_one(seed, facts, drawing, true)?);
    }
    Ok(out)
}

fn resolve_one<'a>(
    seed: &Seed,
    facts: &impl Fn(&str, bool) -> Option<&'a ProgramFacts>,
    drawing: Drawing,
    layered: bool,
) -> Result<Option<Entry>, Unresolved> {
    let get = |k: &str, vertex: bool| facts(k, vertex).ok_or_else(|| Unresolved::Shader(k.into()));
    match seed {
        Seed::Shader(_) => Ok(None),
        Seed::Compute(k) => {
            let c = get(k, false)?;
            Ok(Some(Entry::Compute(ComputeRec {
                key: None,
                function: (library_stem(k, false), c.entry.clone()),
            })))
        }
        Seed::Render(r) => {
            let vs = get(&r.vertex, true)?;
            if vs.mesh.is_some() {
                return Err(Unresolved::Stage(r.vertex.clone()));
            }
            let fs = r
                .fragment
                .as_deref()
                .map(|k| get(k, false).map(|f| (k, f)))
                .transpose()?;
            let (formats, blend, masks) = colors_and_targets(&r.colors);
            let pull = drawing.pull.then_some(vs.pull.as_deref()).flatten();
            let (vertex, layout) = match pull {
                Some(entry) => (
                    (library_stem(&r.vertex, true), entry.to_string()),
                    VertexDescriptor::default(),
                ),
                None => {
                    let e = vertex_entry(
                        layered,
                        &vs.entry,
                        vs.entry_layer.as_deref(),
                        [vs.entry_z[0].as_deref(), vs.entry_z[1].as_deref()],
                        drawing.zero_to_one,
                    );
                    let layout = vertex_descriptor(&r.attributes, &r.streams, &vs.inputs)
                        .map_err(Unresolved::Layout)?;
                    (
                        (library_stem(&r.vertex, false), e.entry.to_string()),
                        layout,
                    )
                }
            };
            let fragment = fs.map(|(k, f)| {
                let entry = fragment_entry(
                    &f.entry,
                    f.entry_uint.as_deref(),
                    f.entry_mixed.as_ref().map(|(e, m)| (e.as_str(), *m)),
                    f.colors.as_deref(),
                    &formats,
                );
                (library_stem(k, false), entry.to_string())
            });
            let colors = color_targets(
                &blend,
                &masks,
                fs.map(|(_, f)| f.colors.as_deref()),
                &formats,
            );
            Ok(Some(Entry::Render(RenderRec {
                key: None,
                vertex_key: None,
                fallback: false,
                vertex,
                fragment,
                colors,
                layout,
                topology: TopologyClass::Triangle,
                samples: PASS_SAMPLES,
                a2c: r.a2c,
                icb: false,
            })))
        }
        Seed::Mesh(m) => {
            let ms = get(&m.mesh, false)?;
            let mi = ms.mesh.ok_or_else(|| Unresolved::Stage(m.mesh.clone()))?;
            let ts = m
                .task
                .as_deref()
                .map(|k| {
                    let t = get(k, false)?;
                    let ti = t.mesh.ok_or_else(|| Unresolved::Stage(k.into()))?;
                    Ok((k, t, ti))
                })
                .transpose()?;
            let fs = m
                .fragment
                .as_deref()
                .map(|k| get(k, false).map(|f| (k, f)))
                .transpose()?;
            let (formats, blend, masks) = colors_and_targets(&m.colors);
            let (mesh_fn, _) = mesh_entry(&ms.entry, ms.entry_z[0].as_deref(), drawing.zero_to_one);
            let fragment = fs.map(|(k, f)| {
                let entry = fragment_entry(
                    &f.entry,
                    f.entry_uint.as_deref(),
                    f.entry_mixed.as_ref().map(|(e, m)| (e.as_str(), *m)),
                    f.colors.as_deref(),
                    &formats,
                );
                (library_stem(k, false), entry.to_string())
            });
            Ok(Some(Entry::Mesh(MeshRec {
                key: None,
                object: ts.map(|(k, t, _)| (library_stem(k, false), t.entry.clone())),
                mesh: (library_stem(&m.mesh, false), mesh_fn.to_string()),
                fragment,
                colors: color_targets(
                    &blend,
                    &masks,
                    fs.map(|(_, f)| f.colors.as_deref()),
                    &formats,
                ),
                samples: PASS_SAMPLES,
                a2c: m.a2c,
                object_threads: ts.map_or(0, |(_, _, ti)| ti.threads),
                mesh_threads: mi.threads,
                payload: ts.map_or(0, |(_, _, ti)| ti.payload),
            })))
        }
    }
}

#[cfg(test)]
mod tests;
