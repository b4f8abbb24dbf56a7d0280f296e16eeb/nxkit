//! The list of pipelines that a run asked for (`pipelines.seen`).
//!
//! Every render, mesh and compute pipeline that the host makes is noted here with the description that made it: the
//! library file stems and entry names of the stages (the names the shard index uses), the colour attachments with
//! their formats, blend state and write masks, the vertex layout, the topology class, the sample count, alpha to
//! coverage and ICB support. The next start makes all of them from the runtime archive before the first frame (see
//! `runtime.rs`).
//!
//! The text holds no game data: the stems are shader keys (hashes), the rest are Metal state names and numbers. It also
//! holds the key of the pipeline cache, the hash that the encoder computes for the draw, so that a pipeline made at the
//! start is found by the first draw that asks. The keys follow the key scheme
//! (`KEY_SCHEME` in `keys.rs`), and the folder of the cache is named by it, so a list is only read by a host with the
//! same scheme.
//!
//! ```text
//! nxkit-pipelines-seen 2
//! R <key> <vertex key> <fallback> <vstem> <ventry> <fstem|-> <fentry|-> <icb> <topology> <samples> <a2c> <colors> <layout>
//! M <key> <ostem|-> <oentry|-> <mstem> <mentry> <fstem|-> <fentry|-> <samples> <a2c> <othreads> <mthreads> <payload> <colors>
//! C <key> <stem> <entry>
//! ```
//!
//! Fields are separated by a tab. `colors` is a comma separated list of slots, `_` for an empty slot and else
//! `<Format>:<mask in hex>:<blend>`, the blend being `-` or the six names `src_rgb/dst_rgb/op_rgb/src_alpha/dst_alpha/
//! op_alpha`. `layout` is `-` or a comma separated list of `a<index>.<Format>.<offset>.<buffer>` attributes and
//! `l<buffer>.<stride>.<Step>.<rate>` layouts.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use nxkit_metal::{
    BlendFactor, BlendOp, BlendState, ColorTarget, Format, MeshPipelineDesc, RenderPipelineDesc,
    ShaderLibrary, Source, StepFunction, TopologyClass, VertexAttribute, VertexDescriptor,
    VertexFormat, VertexLayout,
};

#[cfg(test)]
mod tests;

/// The first line of the file.
pub(super) const HEADER: &str = "nxkit-pipelines-seen 2";

/// The most entries a list keeps. A title with more shaders than this is cut, the rest compiles when asked for.
const MAX_ENTRIES: usize = 200_000;

/// A library file stem and an entry name.
pub(super) type Stage = (String, String);

/// A render pipeline.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RenderRec {
    /// The key of the pipeline cache.
    pub(super) key: u64,
    /// The `point_size_key` of the vertex function, which the fallback remembers.
    pub(super) vertex_key: u64,
    /// The key is one of a pipeline that the topology fallback made (the class is `Unspecified`).
    pub(super) fallback: bool,
    pub(super) vertex: Stage,
    pub(super) fragment: Option<Stage>,
    pub(super) colors: Vec<Option<ColorTarget>>,
    pub(super) layout: VertexDescriptor,
    pub(super) topology: TopologyClass,
    pub(super) samples: u32,
    pub(super) a2c: bool,
    pub(super) icb: bool,
}

/// A mesh pipeline.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct MeshRec {
    pub(super) key: u64,
    pub(super) object: Option<Stage>,
    pub(super) mesh: Stage,
    pub(super) fragment: Option<Stage>,
    pub(super) colors: Vec<Option<ColorTarget>>,
    pub(super) samples: u32,
    pub(super) a2c: bool,
    pub(super) object_threads: u32,
    pub(super) mesh_threads: u32,
    pub(super) payload: u32,
}

/// A compute pipeline.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ComputeRec {
    pub(super) key: u64,
    pub(super) function: Stage,
}

/// One pipeline of the list.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Entry {
    Render(RenderRec),
    Mesh(MeshRec),
    Compute(ComputeRec),
}

impl Entry {
    /// The kind and the key, which tell two entries apart.
    fn id(&self) -> (u8, u64) {
        match self {
            Self::Render(r) => (b'R', r.key),
            Self::Mesh(m) => (b'M', m.key),
            Self::Compute(c) => (b'C', c.key),
        }
    }
}

/// The pipelines of a run (or of several runs), without repeats, in the order they were first asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Seen {
    entries: Vec<Entry>,
    index: HashMap<(u8, u64), usize>,
}

impl Seen {
    /// Adds an entry. False when the list has it already (the first one stays) or is full.
    pub(super) fn insert(&mut self, e: Entry) -> bool {
        if self.entries.len() >= MAX_ENTRIES || self.index.contains_key(&e.id()) {
            return false;
        }
        self.index.insert(e.id(), self.entries.len());
        self.entries.push(e);
        true
    }

    /// Adds what `other` has and this list lacks, after its own entries. Returns how many were added.
    pub(super) fn merge(&mut self, other: &Seen) -> usize {
        other
            .entries
            .iter()
            .filter(|e| self.insert((*e).clone()))
            .count()
    }

    /// The entries in order.
    pub(super) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The text of the file.
    pub(super) fn to_text(&self) -> String {
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

    /// Reads the text of a file. A line that does not parse is left out; a text of another version is an error.
    pub(super) fn parse(text: &str) -> Result<Seen, String> {
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

/// Looks a name up in the table of an enum. The table is made once per enum from `Debug`.
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

fn name_ok(s: &str) -> bool {
    !s.is_empty() && !s.contains(|c: char| c.is_control() || c.is_whitespace())
}

fn colors_text(c: &[Option<ColorTarget>]) -> Option<String> {
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

fn layout_text(d: &VertexDescriptor) -> String {
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

fn line(e: &Entry) -> Option<String> {
    Some(match e {
        Entry::Render(r) => format!(
            "R\t{:016x}\t{:016x}\t{}\t{}\t{}\t{}\t{}\t{:?}\t{}\t{}\t{}\t{}",
            r.key,
            r.vertex_key,
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
            "M\t{:016x}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            m.key,
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
        Entry::Compute(c) => format!("C\t{:016x}\t{}\t{}", c.key, c.function.0, c.function.1),
    })
}

fn stage_of(a: &str, b: &str) -> Option<Option<Stage>> {
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

fn parse_line(l: &str) -> Option<Entry> {
    let f: Vec<&str> = l.split('\t').collect();
    let key = u64::from_str_radix(f.get(1)?, 16).ok()?;
    match (*f.first()?, f.len()) {
        ("R", 14) => Some(Entry::Render(RenderRec {
            key,
            vertex_key: u64::from_str_radix(f[2], 16).ok()?,
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

/// The names that the shard index uses for a stage, when the library came from a file.
fn stage(l: &ShaderLibrary, entry: &str) -> Option<Stage> {
    let stem = l.stem()?;
    (name_ok(stem) && name_ok(entry)).then(|| (stem.to_string(), entry.to_string()))
}

/// The record of a render description, when every part of it can be written.
pub(super) fn render_rec(
    key: u64,
    vertex_key: u64,
    fallback: bool,
    d: &RenderPipelineDesc<'_>,
) -> Option<RenderRec> {
    let fragment = match d.fragment {
        Some((l, e)) => Some(stage(l, e)?),
        None => None,
    };
    colors_text(&d.colors)?;
    Some(RenderRec {
        key,
        vertex_key,
        fallback,
        vertex: stage(d.vertex.0, d.vertex.1)?,
        fragment,
        colors: d.colors.clone(),
        layout: d.vertex_layout.clone(),
        topology: d.topology,
        samples: d.sample_count,
        a2c: d.alpha_to_coverage,
        icb: d.support_icb,
    })
}

/// The record of a mesh description.
pub(super) fn mesh_rec(key: u64, d: &MeshPipelineDesc<'_>) -> Option<MeshRec> {
    let opt = |s: Option<(&ShaderLibrary, &str)>| match s {
        Some((l, e)) => stage(l, e).map(Some),
        None => Some(None),
    };
    colors_text(&d.colors)?;
    Some(MeshRec {
        key,
        object: opt(d.object)?,
        mesh: stage(d.mesh.0, d.mesh.1)?,
        fragment: opt(d.fragment)?,
        colors: d.colors.clone(),
        samples: d.sample_count,
        a2c: d.alpha_to_coverage,
        object_threads: d.object_threads,
        mesh_threads: d.mesh_threads,
        payload: d.payload_bytes,
    })
}

/// What the run has asked for. Set once by [`arm`].
static ARMED: AtomicBool = AtomicBool::new(false);
static LIST: Mutex<Option<Seen>> = Mutex::new(None);
/// Entries added since the list was last written.
static DIRTY: AtomicBool = AtomicBool::new(false);
/// Pipelines made since the runtime archive was last written: the ones that Metal compiled or specialized.
static FRESH: AtomicU64 = AtomicU64::new(0);

/// Whether `NXKIT_PIPELINE_RECORD` allows the list: on unless it is `0`.
pub(super) fn record_on() -> bool {
    std::env::var("NXKIT_PIPELINE_RECORD").map_or(true, |v| v != "0")
}

/// Starts the list from what earlier runs wrote.
pub(super) fn arm(initial: Seen) {
    *LIST.lock().unwrap_or_else(PoisonError::into_inner) = Some(initial);
    ARMED.store(true, Ordering::Release);
}

/// True once [`arm`] ran: the hooks of the encoder do nothing before.
pub(super) fn armed() -> bool {
    ARMED.load(Ordering::Acquire)
}

fn add(e: Option<Entry>, fresh: bool) {
    if fresh {
        FRESH.fetch_add(1, Ordering::Relaxed);
    }
    let Some(e) = e else { return };
    let mut g = LIST.lock().unwrap_or_else(PoisonError::into_inner);
    if g.as_mut().is_some_and(|l| l.insert(e)) {
        DIRTY.store(true, Ordering::Release);
    }
}

fn made(s: Source) -> bool {
    matches!(s, Source::Compiled(_) | Source::Specialized(_))
}

/// Notes a render pipeline that the cache made or found.
pub(super) fn render(
    key: u64,
    vertex_key: u64,
    fallback: bool,
    d: &RenderPipelineDesc<'_>,
    s: Source,
) {
    if armed() && s != Source::Cached {
        add(
            render_rec(key, vertex_key, fallback, d).map(Entry::Render),
            made(s),
        );
    }
}

/// Notes a mesh pipeline.
pub(super) fn mesh(key: u64, d: &MeshPipelineDesc<'_>, s: Source) {
    if armed() && s != Source::Cached {
        add(mesh_rec(key, d).map(Entry::Mesh), made(s));
    }
}

/// Notes a compute pipeline of a library from a file.
pub(super) fn compute(key: u64, lib: &ShaderLibrary, entry: &str) {
    if armed() {
        add(
            stage(lib, entry).map(|function| Entry::Compute(ComputeRec { key, function })),
            false,
        );
    }
}

/// The list as it is now, when it changed since the last call.
pub(super) fn take_dirty() -> Option<Seen> {
    if !DIRTY.swap(false, Ordering::AcqRel) {
        return None;
    }
    LIST.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Puts back the mark that [`take_dirty`] cleared, after a write that failed.
pub(super) fn mark_dirty() {
    DIRTY.store(true, Ordering::Release);
}

/// The pipelines compiled or specialized since the last call.
pub(super) fn take_fresh() -> u64 {
    FRESH.swap(0, Ordering::AcqRel)
}

/// Puts back what [`take_fresh`] took, after a write that failed.
pub(super) fn restore_fresh(n: u64) {
    FRESH.fetch_add(n, Ordering::Relaxed);
}
