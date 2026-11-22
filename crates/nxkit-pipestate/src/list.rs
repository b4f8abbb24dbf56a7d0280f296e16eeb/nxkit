//! The text of a pipeline list (`pipelines.seen`, `seen-<scheme>.list`, the shipped list of an app).
//!
//! The host notes every render, mesh and compute pipeline that it makes with the description that made it
//! (`nxkit-gpu` `pipelines/record.rs`): the library file stems and entry names of the stages (the names the shard index
//! uses), the colour attachments with their formats, blend state and write masks, the vertex layout, the topology
//! class, the sample count, alpha to coverage and ICB support. The build archives the pipelines of a list
//! (`nxb-archive`) and writes the list that it resolved from the seeds of a title into the app (`nxb-build`). The
//! start makes all of them before the first frame.
//!
//! The text holds no game data: the stems are shader keys (hashes), the rest are Metal state names and numbers. A list
//! of the host also holds the key of the pipeline cache, the hash that the encoder computes for the draw, so that a
//! pipeline made at the start is found by the first draw that asks. A list that the build wrote has `-` there: the
//! keys follow the host's key scheme, which the build does not know, and the host computes them when it reads the
//! list.
//!
//! ```text
//! nxkit-pipelines-seen 2
//! R <key|-> <vertex key|-> <fallback> <vstem> <ventry> <fstem|-> <fentry|-> <icb> <topology> <samples> <a2c> <colors> <layout>
//! M <key|-> <ostem|-> <oentry|-> <mstem> <mentry> <fstem|-> <fentry|-> <samples> <a2c> <othreads> <mthreads> <payload> <colors>
//! C <key|-> <stem> <entry>
//! ```
//!
//! Fields are separated by a tab. `colors` is a comma separated list of slots, `_` for an empty slot and else
//! `<Format>:<mask in hex>:<blend>`, the blend being `-` or the six names `src_rgb/dst_rgb/op_rgb/src_alpha/dst_alpha/
//! op_alpha`. `layout` is `-` or a comma separated list of `a<index>.<Format>.<offset>.<buffer>` attributes and
//! `l<buffer>.<stride>.<Step>.<rate>` layouts. A line that does not parse is left out.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::format::Format;
use crate::state::{
    BlendFactor, BlendOp, BlendState, ColorTarget, StepFunction, TopologyClass, VertexAttribute,
    VertexDescriptor, VertexFormat, VertexLayout,
};

/// Where the build writes the list it resolved from the seeds of a title, relative to the run folder of the app. The
/// host reads it at every start and never writes it.
pub const SHIPPED_LIST: &str = "pipelines/shipped.list";

/// The first line of the file.
pub const HEADER: &str = "nxkit-pipelines-seen 2";

/// The most entries a list keeps. A title with more shaders than this is cut, the rest compiles when asked for.
pub const MAX_ENTRIES: usize = 200_000;

/// A library file stem and an entry name.
pub type Stage = (String, String);

/// A render pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderRec {
    /// The key of the pipeline cache. `None` in a list that the build wrote: the host computes it.
    pub key: Option<u64>,
    /// The `point_size_key` of the vertex function, which the fallback remembers. `None` like `key`.
    pub vertex_key: Option<u64>,
    /// The key is one of a pipeline that the topology fallback made (the class is `Unspecified`).
    pub fallback: bool,
    /// The vertex function.
    pub vertex: Stage,
    /// The fragment function, none for a draw that writes depth only.
    pub fragment: Option<Stage>,
    /// The colour attachments by slot.
    pub colors: Vec<Option<ColorTarget>>,
    /// The vertex layout, empty for a vertex function that pulls.
    pub layout: VertexDescriptor,
    /// The topology class.
    pub topology: TopologyClass,
    /// The sample count.
    pub samples: u32,
    /// Alpha to coverage.
    pub a2c: bool,
    /// ICB support.
    pub icb: bool,
}

/// A mesh pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshRec {
    /// The key of the pipeline cache, `None` in a list that the build wrote.
    pub key: Option<u64>,
    /// The object (task) function.
    pub object: Option<Stage>,
    /// The mesh function.
    pub mesh: Stage,
    /// The fragment function, none for a draw that writes depth only.
    pub fragment: Option<Stage>,
    /// The colour attachments by slot.
    pub colors: Vec<Option<ColorTarget>>,
    /// The sample count.
    pub samples: u32,
    /// Alpha to coverage.
    pub a2c: bool,
    /// The threads of an object threadgroup.
    pub object_threads: u32,
    /// The threads of a mesh threadgroup.
    pub mesh_threads: u32,
    /// The bytes of the payload.
    pub payload: u32,
}

/// A compute pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputeRec {
    /// The key of the pipeline cache, `None` in a list that the build wrote.
    pub key: Option<u64>,
    /// The compute function.
    pub function: Stage,
}

/// One pipeline of the list.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// A render pipeline.
    Render(RenderRec),
    /// A mesh pipeline.
    Mesh(MeshRec),
    /// A compute pipeline.
    Compute(ComputeRec),
}

/// What tells two entries apart: the kind and the key, or the line without the keys for an entry without one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Id {
    Key(u8, u64),
    Text(String),
}

impl Entry {
    /// The key of the pipeline cache.
    pub fn key(&self) -> Option<u64> {
        match self {
            Self::Render(r) => r.key,
            Self::Mesh(m) => m.key,
            Self::Compute(c) => c.key,
        }
    }

    fn kind(&self) -> u8 {
        match self {
            Self::Render(_) => b'R',
            Self::Mesh(_) => b'M',
            Self::Compute(_) => b'C',
        }
    }

    fn id(&self) -> Id {
        match self.key() {
            Some(k) => Id::Key(self.kind(), k),
            None => Id::Text(line(self).unwrap_or_default()),
        }
    }
}

/// The pipelines of a run (or of several runs), without repeats, in the order they were first asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    entries: Vec<Entry>,
    index: HashMap<Id, usize>,
}

impl Seen {
    /// Adds an entry. False when the list has it already (the first one stays) or is full.
    pub fn insert(&mut self, e: Entry) -> bool {
        if self.entries.len() >= MAX_ENTRIES || self.index.contains_key(&e.id()) {
            return false;
        }
        self.index.insert(e.id(), self.entries.len());
        self.entries.push(e);
        true
    }

    /// Adds what `other` has and this list lacks, after its own entries. Returns how many were added.
    pub fn merge(&mut self, other: &Seen) -> usize {
        other
            .entries
            .iter()
            .filter(|e| self.insert((*e).clone()))
            .count()
    }

    /// True when the list has an entry that tells apart as `e` (the same kind and key, or the same description for an
    /// entry without a key).
    pub fn contains(&self, e: &Entry) -> bool {
        self.index.contains_key(&e.id())
    }

    /// The entries in order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The list with the keys that `keys` gives the entries without one, and without the entries it gives none.
    /// `keys` returns the key and, for a render entry, the `point_size_key` of its vertex function.
    pub fn with_keys(&self, keys: impl Fn(&Entry) -> Option<(u64, Option<u64>)>) -> Seen {
        let mut out = Seen::default();
        for e in &self.entries {
            let mut e = e.clone();
            if e.key().is_none() {
                let Some((key, vertex)) = keys(&e) else {
                    continue;
                };
                match &mut e {
                    Entry::Render(r) => {
                        r.key = Some(key);
                        r.vertex_key = vertex;
                    }
                    Entry::Mesh(m) => m.key = Some(key),
                    Entry::Compute(c) => c.key = Some(key),
                }
            }
            out.insert(e);
        }
        out
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True without entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The text of the file.
    pub fn to_text(&self) -> String {
        let mut o = String::with_capacity(self.entries.len() * 160 + 32);
        o.push_str(HEADER);
        o.push('\n');
        for e in &self.entries {
            if let Some(l) = line(e) {
                o.push_str(&l);
                o.push('\n');
            }
        }
        o
    }

    /// Reads the text of a file. A line that does not parse is left out. A text of another version is an error.
    pub fn parse(text: &str) -> Result<Seen, String> {
        let mut lines = text.lines();
        match lines.next() {
            Some(HEADER) => {}
            Some(h) => return Err(format!("unknown header {h:?}")),
            None => return Err("empty".into()),
        }
        let mut s = Seen::default();
        for l in lines {
            if let Some(e) = parse_line(l) {
                s.insert(e);
            }
        }
        Ok(s)
    }
}

/// Looks a name up in the table of an enum. The table is made once per enum from `Debug`. The static is process-wide
/// (W26) because the names never change and a list of tens of thousands of lines reads one per field.
macro_rules! by_name {
    ($t:ty, $all:expr, $s:expr) => {{
        static T: OnceLock<HashMap<String, $t>> = OnceLock::new();
        T.get_or_init(|| $all.iter().map(|v| (format!("{v:?}"), *v)).collect())
            .get($s)
            .copied()
    }};
}

fn format_of(s: &str) -> Option<Format> {
    by_name!(Format, Format::SIMPLE, s)
}

/// The name of a format, or none for one that a render target does not have (block compressed).
fn format_name(f: Format) -> Option<String> {
    format_of(&format!("{f:?}")).map(|f| format!("{f:?}"))
}

/// True for a library stem or an entry name that a list can hold.
pub fn name_ok(s: &str) -> bool {
    !s.is_empty() && !s.contains(|c: char| c.is_control() || c.is_whitespace())
}

/// The text of colour attachments, `None` for a block compressed format.
pub fn colors_text(c: &[Option<ColorTarget>]) -> Option<String> {
    if c.is_empty() {
        return Some("-".into());
    }
    let mut parts = Vec::with_capacity(c.len());
    for slot in c {
        parts.push(match slot {
            None => "_".to_string(),
            Some(t) => {
                let blend = match &t.blend {
                    None => "-".to_string(),
                    Some(b) => format!(
                        "{:?}/{:?}/{:?}/{:?}/{:?}/{:?}",
                        b.src_rgb, b.dst_rgb, b.op_rgb, b.src_alpha, b.dst_alpha, b.op_alpha
                    ),
                };
                format!("{}:{:x}:{blend}", format_name(t.format)?, t.write_mask)
            }
        });
    }
    Some(parts.join(","))
}

fn blend_of(s: &str) -> Option<Option<BlendState>> {
    if s == "-" {
        return Some(None);
    }
    let p: Vec<&str> = s.split('/').collect();
    let [a, b, c, d, e, f] = p[..] else {
        return None;
    };
    let factor = |s: &str| by_name!(BlendFactor, BlendFactor::ALL, s);
    let op = |s: &str| by_name!(BlendOp, BlendOp::ALL, s);
    Some(Some(BlendState {
        src_rgb: factor(a)?,
        dst_rgb: factor(b)?,
        op_rgb: op(c)?,
        src_alpha: factor(d)?,
        dst_alpha: factor(e)?,
        op_alpha: op(f)?,
    }))
}

fn colors_of(s: &str) -> Option<Vec<Option<ColorTarget>>> {
    if s == "-" {
        return Some(Vec::new());
    }
    s.split(',')
        .map(|slot| {
            if slot == "_" {
                return Some(None);
            }
            let mut p = slot.splitn(3, ':');
            let format = format_of(p.next()?)?;
            let write_mask = u8::from_str_radix(p.next()?, 16).ok()?;
            let blend = blend_of(p.next()?)?;
            Some(Some(ColorTarget {
                format,
                blend,
                write_mask,
            }))
        })
        .collect()
}

/// The text of a vertex layout.
pub fn layout_text(d: &VertexDescriptor) -> String {
    if d.attributes.is_empty() && d.layouts.is_empty() {
        return "-".into();
    }
    let a = d
        .attributes
        .iter()
        .map(|(i, a)| format!("a{i}.{:?}.{}.{}", a.format, a.offset, a.buffer));
    let l = d
        .layouts
        .iter()
        .map(|l| format!("l{}.{}.{:?}.{}", l.buffer, l.stride, l.step, l.step_rate));
    a.chain(l).collect::<Vec<_>>().join(",")
}

fn layout_of(s: &str) -> Option<VertexDescriptor> {
    let mut d = VertexDescriptor::default();
    if s == "-" {
        return Some(d);
    }
    for item in s.split(',') {
        let (kind, rest) = item.split_at_checked(1)?;
        let p: Vec<&str> = rest.split('.').collect();
        match (kind, &p[..]) {
            ("a", [i, f, off, buf]) => d.attributes.push((
                i.parse().ok()?,
                VertexAttribute {
                    format: by_name!(VertexFormat, VertexFormat::ALL, *f)?,
                    offset: off.parse().ok()?,
                    buffer: buf.parse().ok()?,
                },
            )),
            ("l", [buf, stride, step, rate]) => d.layouts.push(VertexLayout {
                buffer: buf.parse().ok()?,
                stride: stride.parse().ok()?,
                step: by_name!(StepFunction, StepFunction::ALL, *step)?,
                step_rate: rate.parse().ok()?,
            }),
            _ => return None,
        }
    }
    Some(d)
}

fn stage_text(s: &Option<Stage>) -> String {
    match s {
        Some((a, b)) => format!("{a}\t{b}"),
        None => "-\t-".into(),
    }
}

/// The line of an entry, `None` for one with a format that a list cannot name (block compressed).
pub fn line(e: &Entry) -> Option<String> {
    Some(match e {
        Entry::Render(r) => format!(
            "R\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:?}\t{}\t{}\t{}\t{}",
            key_text(r.key),
            key_text(r.vertex_key),
            u8::from(r.fallback),
            r.vertex.0,
            r.vertex.1,
            stage_text(&r.fragment),
            u8::from(r.icb),
            r.topology,
            r.samples,
            u8::from(r.a2c),
            colors_text(&r.colors)?,
            layout_text(&r.layout),
        ),
        Entry::Mesh(m) => format!(
            "M\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            key_text(m.key),
            stage_text(&m.object),
            m.mesh.0,
            m.mesh.1,
            stage_text(&m.fragment),
            m.samples,
            u8::from(m.a2c),
            m.object_threads,
            m.mesh_threads,
            m.payload,
            colors_text(&m.colors)?,
        ),
        Entry::Compute(c) => format!("C\t{}\t{}\t{}", key_text(c.key), c.function.0, c.function.1),
    })
}

/// A stage from its two fields: `-` `-` for none.
pub fn stage_of(a: &str, b: &str) -> Option<Option<Stage>> {
    match (a, b) {
        ("-", "-") => Some(None),
        _ if name_ok(a) && name_ok(b) => Some(Some((a.to_string(), b.to_string()))),
        _ => None,
    }
}

fn flag(s: &str) -> Option<bool> {
    match s {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn key_text(k: Option<u64>) -> String {
    k.map_or_else(|| "-".to_string(), |k| format!("{k:016x}"))
}

fn key_of(s: &str) -> Option<Option<u64>> {
    if s == "-" {
        return Some(None);
    }
    u64::from_str_radix(s, 16).ok().map(Some)
}

/// One line of a list, `None` for a line that does not parse.
pub fn parse_line(l: &str) -> Option<Entry> {
    let f: Vec<&str> = l.split('\t').collect();
    let key = key_of(f.get(1)?)?;
    match (*f.first()?, f.len()) {
        ("R", 14) => Some(Entry::Render(RenderRec {
            key,
            vertex_key: key_of(f[2])?,
            fallback: flag(f[3])?,
            vertex: stage_of(f[4], f[5])??,
            fragment: stage_of(f[6], f[7])?,
            icb: flag(f[8])?,
            topology: by_name!(TopologyClass, TopologyClass::ALL, f[9])?,
            samples: f[10].parse().ok()?,
            a2c: flag(f[11])?,
            colors: colors_of(f[12])?,
            layout: layout_of(f[13])?,
        })),
        ("M", 14) => Some(Entry::Mesh(MeshRec {
            key,
            object: stage_of(f[2], f[3])?,
            mesh: stage_of(f[4], f[5])??,
            fragment: stage_of(f[6], f[7])?,
            samples: f[8].parse().ok()?,
            a2c: flag(f[9])?,
            object_threads: f[10].parse().ok()?,
            mesh_threads: f[11].parse().ok()?,
            payload: f[12].parse().ok()?,
            colors: colors_of(f[13])?,
        })),
        ("C", 4) => Some(Entry::Compute(ComputeRec {
            key,
            function: stage_of(f[2], f[3])??,
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
