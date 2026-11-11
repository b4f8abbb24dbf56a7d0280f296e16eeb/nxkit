//! Pipelines the host recorded while a title played (`pipelines.seen`, `seen-<scheme>.list`), as archive bases.
//!
//! The archive holds one unspecialized pipeline for each pair of shaders, and the host makes the pipeline of a draw by
//! specializing it to the colour state (`nxkit-metal` `cache::specialize`). That works only for the state that the
//! base has: no vertex layout, one sample, no alpha to coverage, no ICB support. A draw whose pipeline has a vertex
//! layout, several samples, alpha to coverage or ICB support finds no base, so the host compiled it from the
//! functions while the title played (seconds each, and the draw was skipped until it was done).
//!
//! The list says which of these states the title used. This module reads it and makes a base for each such state:
//! the pair with the vertex layout, primitive class, sample count, alpha to coverage and ICB support of the state, and
//! all colour attachments unspecialized. The script gets these bases beside the plain ones ([`crate::plan`]), the
//! archive holds them compiled for the GPU, and the host finds a base of this state in the archive and specializes it
//! to the colour state, as it does for a plain base. The colour state of the list is not needed: it is what the host
//! specializes.
//!
//! The same lines also give the fully specialized pipelines of the recorded states, which the host loads without
//! specializing ([`crate::exact`]).
//!
//! The list is text, one line per pipeline, tab separated. Only the lines of render pipelines (`R`) are used:
//!
//! ```text
//! nxkit-pipelines-seen 2
//! R <key> <vertex key> <fallback> <vstem> <ventry> <fstem|-> <fentry|-> <icb> <topology> <samples> <a2c> <colors> <layout>
//! ```
//!
//! `layout` is `-` or a comma separated list of `a<index>.<Format>.<offset>.<buffer>` attributes and
//! `l<buffer>.<stride>.<Step>.<rate>` layouts (the format of `nxkit-gpu` `pipelines/record.rs`). A line that does not
//! parse, or that names a format the script has no name for, is left out. Mesh and compute pipelines are not used.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use nxb_pyjson::Value;

use crate::plan::{PlannedBase, plan_with};

/// The first line of a list.
pub const HEADER: &str = "nxkit-pipelines-seen 2";

/// A library file stem and an entry name.
pub type Func = (String, String);

/// A vertex attribute: index, script format name, byte offset, buffer index.
pub type Attribute = (u32, String, u32, u32);

/// A vertex buffer layout: buffer index, stride, step function name, step rate.
pub type Layout = (u32, u32, String, u32);

/// The state of a base that the host cannot reach by specializing the plain one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StateBase {
    /// The vertex function (library stem, entry).
    pub vertex: Func,
    /// The fragment function.
    pub fragment: Func,
    /// The vertex attributes, in the order of the list.
    pub attributes: Vec<Attribute>,
    /// The vertex buffer layouts, in the order of the list.
    pub layouts: Vec<Layout>,
    /// `Unspecified`, `Point`, `Line` or `Triangle`.
    pub topology: String,
    /// The sample count.
    pub samples: u32,
    /// Alpha to coverage.
    pub alpha_to_coverage: bool,
    /// ICB support.
    pub icb: bool,
}

/// The names of the vertex formats of the script (`ATTRIBUTE FORMAT` of `metal4-pipelines-script(5)`).
const FORMATS: &[&str] = &[
    "Char",
    "Char2",
    "Char3",
    "Char4",
    "UChar",
    "UChar2",
    "UChar3",
    "UChar4",
    "UCharNormalized",
    "UChar2Normalized",
    "UChar3Normalized",
    "UChar4Normalized",
    "CharNormalized",
    "Char2Normalized",
    "Char3Normalized",
    "Char4Normalized",
    "Short",
    "Short2",
    "Short3",
    "Short4",
    "UShort",
    "UShort2",
    "UShort3",
    "UShort4",
    "ShortNormalized",
    "Short2Normalized",
    "Short3Normalized",
    "Short4Normalized",
    "UShortNormalized",
    "UShort2Normalized",
    "UShort3Normalized",
    "UShort4Normalized",
    "Int",
    "Int2",
    "Int3",
    "Int4",
    "UInt",
    "UInt2",
    "UInt3",
    "UInt4",
    "Int1010102Normalized",
    "UInt1010102Normalized",
    "UChar4Normalized_BGRA",
    "Half",
    "Half2",
    "Half3",
    "Half4",
    "Float",
    "Float2",
    "Float3",
    "Float4",
];

/// The script's name of a vertex format of the host (`nxkit-metal` `VertexFormat`, `UChar4Norm` for
/// `UChar4Normalized`), none for a format the script does not name.
fn format_name(host: &str) -> Option<String> {
    let name = if let Some(base) = host.strip_suffix("NormBgra") {
        format!("{base}Normalized_BGRA")
    } else if let Some(base) = host.strip_suffix("Norm") {
        format!("{base}Normalized")
    } else {
        host.to_string()
    };
    FORMATS.contains(&name.as_str()).then_some(name)
}

pub(crate) fn layout_of(s: &str) -> Option<(Vec<Attribute>, Vec<Layout>)> {
    let (mut attributes, mut layouts) = (Vec::new(), Vec::new());
    if s == "-" {
        return Some((attributes, layouts));
    }
    for item in s.split(',') {
        let (kind, rest) = item.split_at_checked(1)?;
        let p: Vec<&str> = rest.split('.').collect();
        match (kind, &p[..]) {
            ("a", [i, format, offset, buffer]) => attributes.push((
                i.parse().ok()?,
                format_name(format)?,
                offset.parse().ok()?,
                buffer.parse().ok()?,
            )),
            ("l", [buffer, stride, step, rate]) => {
                if !["Constant", "PerVertex", "PerInstance"].contains(step) {
                    return None;
                }
                layouts.push((
                    buffer.parse().ok()?,
                    stride.parse().ok()?,
                    (*step).to_string(),
                    rate.parse().ok()?,
                ));
            }
            _ => return None,
        }
    }
    Some((attributes, layouts))
}

pub(crate) fn flag(s: &str) -> Option<bool> {
    match s {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

pub(crate) fn stage(a: &str, b: &str) -> Option<Option<Func>> {
    let ok = |s: &str| !s.is_empty() && !s.contains(|c: char| c.is_control() || c.is_whitespace());
    match (a, b) {
        ("-", "-") => Some(None),
        _ if ok(a) && ok(b) => Some(Some((a.to_string(), b.to_string()))),
        _ => None,
    }
}

/// The bases of the state of one `R` line: none for a pipeline the plain base serves, one for a state, two for a state
/// that the host's point size fallback made (it compiles such a pipeline without a primitive class).
fn bases_of_line(l: &str) -> Vec<StateBase> {
    let f: Vec<&str> = l.split('\t').collect();
    if f.len() != 14 || f[0] != "R" {
        return Vec::new();
    }
    let parsed = (|| {
        let fallback = flag(f[3])?;
        let vertex = stage(f[4], f[5])??;
        let fragment = stage(f[6], f[7])??;
        let icb = flag(f[8])?;
        let topology = f[9];
        if !["Unspecified", "Point", "Line", "Triangle"].contains(&topology) {
            return None;
        }
        let samples: u32 = f[10].parse().ok()?;
        let alpha_to_coverage = flag(f[11])?;
        let (attributes, layouts) = layout_of(f[13])?;
        Some((
            fallback,
            StateBase {
                vertex,
                fragment,
                attributes,
                layouts,
                topology: topology.to_string(),
                samples,
                alpha_to_coverage,
                icb,
            },
        ))
    })();
    let Some((fallback, base)) = parsed else {
        return Vec::new();
    };
    let plain = base.attributes.is_empty()
        && base.layouts.is_empty()
        && base.samples == 1
        && !base.alpha_to_coverage
        && !base.icb;
    if plain {
        return Vec::new();
    }
    let mut out = vec![base.clone()];
    if fallback && base.topology != "Unspecified" {
        out.push(StateBase {
            topology: "Unspecified".into(),
            ..base
        });
    }
    out
}

/// The bases of the states in a list text, without repeats, in the order of the list. A text of another version gives
/// none.
pub fn read_list(text: &str) -> Vec<StateBase> {
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for l in lines {
        for b in bases_of_line(l) {
            if seen.insert(b.clone()) {
                out.push(b);
            }
        }
    }
    out
}

/// The bases of several list files, without repeats. A file that cannot be read is left out.
pub fn read_lists(paths: &[PathBuf]) -> Vec<StateBase> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        for b in read_list(&text) {
            if seen.insert(b.clone()) {
                out.push(b);
            }
        }
    }
    out
}

/// The size in bytes of a vertex format of the script (`Float3` 12, `UChar4Normalized` 4, `Half2` 4).
fn format_size(name: &str) -> Option<u32> {
    if name.starts_with("Int1010102") || name.starts_with("UInt1010102") {
        return Some(4);
    }
    let base = name.split("Normalized").next()?;
    let digits = base.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let kind = &base[..base.len() - digits.len()];
    let count: u32 = if digits.is_empty() {
        1
    } else {
        digits.parse().ok()?
    };
    let each = match kind {
        "Char" | "UChar" => 1,
        "Short" | "UShort" | "Half" => 2,
        "Int" | "UInt" | "Float" => 4,
        _ => return None,
    };
    (1..=4).contains(&count).then_some(each * count)
}

/// Why the compiler would refuse a vertex descriptor, or none. The host's descriptors come from the game's vertex
/// state, and a game may declare an attribute beyond the stride of its stream (the host sets it as it is and Metal
/// compiles it at run time), but `applegpu-nt` checks the descriptor and fails the whole batch. The rules, from
/// Metal's: a buffer index and an attribute index below 31, every attribute's buffer has a layout, the attribute lies
/// inside the stride (`offset + size <= stride`), a per vertex or per instance layout has a stride. With `used` (the
/// attribute locations that the vertex function reads) only those attributes are checked.
pub(crate) fn layout_invalid(
    attributes: &[Attribute],
    layouts: &[Layout],
    used: Option<&HashSet<u32>>,
) -> Option<String> {
    for (buffer, stride, step, _) in layouts {
        if *buffer >= 31 {
            return Some(format!("layout of buffer {buffer}: buffer index over 30"));
        }
        if *stride == 0 && step != "Constant" {
            return Some(format!(
                "layout of buffer {buffer}: stride 0 with step {step}"
            ));
        }
    }
    for (i, format, offset, buffer) in attributes {
        // the compiler checks the attributes that the vertex function reads; the others pass as they are
        if used.is_some_and(|u| !u.contains(i)) {
            continue;
        }
        if *i >= 31 {
            return Some(format!("attribute {i}: index over 30"));
        }
        let Some(size) = format_size(format) else {
            return Some(format!("attribute {i}: unknown format {format}"));
        };
        let Some(layout) = layouts.iter().find(|l| l.0 == *buffer) else {
            return Some(format!("attribute {i}: buffer {buffer} has no layout"));
        };
        if layout.2 != "Constant" && offset + size > layout.1 {
            return Some(format!(
                "attribute {i} ({format} at {offset}) does not fit the stride {} of buffer {buffer}",
                layout.1
            ));
        }
    }
    None
}

/// The vertex descriptor: attributes by index and layouts by buffer, a slot nobody sets at its default. The host
/// gives a constant step no rate (0) and any other step at least 1.
pub(crate) fn vertex_descriptor(attributes: &[Attribute], layouts: &[Layout]) -> Value {
    let n = |v: u32| Value::Int(i64::from(v));
    let mut attrs = Vec::new();
    if let Some(top) = attributes.iter().map(|a| a.0).max() {
        attrs =
            vec![Value::Object(vec![("format".into(), Value::str("Invalid"))]); top as usize + 1];
        for (i, format, offset, buffer) in attributes {
            attrs[*i as usize] = Value::Object(vec![
                ("buffer_index".into(), n(*buffer)),
                ("offset".into(), n(*offset)),
                ("format".into(), Value::str(format.clone())),
            ]);
        }
    }
    let mut lays = Vec::new();
    if let Some(top) = layouts.iter().map(|l| l.0).max() {
        let unset = Value::Object(vec![
            ("stride".into(), n(0)),
            ("step_function".into(), Value::str("PerVertex")),
            ("step_rate".into(), n(1)),
        ]);
        lays = vec![unset; top as usize + 1];
        for (buffer, stride, step, rate) in layouts {
            let rate = if step == "Constant" {
                0
            } else {
                (*rate).max(1)
            };
            lays[*buffer as usize] = Value::Object(vec![
                ("stride".into(), n(*stride)),
                ("step_function".into(), Value::str(step.clone())),
                ("step_rate".into(), n(rate)),
            ]);
        }
    }
    let mut o = Vec::new();
    if !attrs.is_empty() {
        o.push(("attributes".to_string(), Value::List(attrs)));
    }
    if !lays.is_empty() {
        o.push(("layouts".to_string(), Value::List(lays)));
    }
    Value::Object(o)
}

impl StateBase {
    /// Why the compiler would refuse the vertex descriptor of this state, or none ([`layout_invalid`]).
    pub fn invalid(&self, used: Option<&HashSet<u32>>) -> Option<String> {
        layout_invalid(&self.attributes, &self.layouts, used)
    }

    /// The shader keys of the pair: the stems without `_b` or `_bp`. None for a stem the build does not make.
    pub fn pair_keys(&self) -> Option<(String, String)> {
        let v = self
            .vertex
            .0
            .strip_suffix("_bp")
            .or_else(|| self.vertex.0.strip_suffix("_b"))?;
        let f = self.fragment.0.strip_suffix("_b")?;
        Some((v.to_string(), f.to_string()))
    }

    /// The base as [`PlannedBase`] (what `plan` gives for the pair).
    pub fn planned(&self) -> PlannedBase {
        (
            self.vertex.0.clone(),
            self.vertex.1.clone(),
            self.fragment.0.clone(),
            self.fragment.1.clone(),
        )
    }

    /// The render pipeline descriptor of the script: the two function references, the state, and eight unspecialized
    /// colour attachments. Members at their default are left out.
    pub fn descriptor(&self, vertex: Value, fragment: Value, colour: &Value) -> Value {
        let mut m = vec![
            ("vertex_function_descriptor".to_string(), vertex),
            ("fragment_function_descriptor".to_string(), fragment),
        ];
        if !self.attributes.is_empty() || !self.layouts.is_empty() {
            m.push(("vertex_descriptor".into(), self.vertex_descriptor()));
        }
        if self.topology != "Unspecified" {
            m.push((
                "input_primitive_topology".into(),
                Value::str(self.topology.clone()),
            ));
        }
        if self.alpha_to_coverage {
            m.push(("alpha_to_coverage_state".into(), Value::str("Enabled")));
        }
        if self.samples != 1 {
            m.push((
                "raster_sample_count".into(),
                Value::Int(i64::from(self.samples)),
            ));
        }
        if self.icb {
            m.push((
                "support_indirect_command_buffers".into(),
                Value::str("Enabled"),
            ));
        }
        m.push((
            "color_attachments".into(),
            Value::List(vec![colour.clone(); 8]),
        ));
        Value::Object(m)
    }

    fn vertex_descriptor(&self) -> Value {
        vertex_descriptor(&self.attributes, &self.layouts)
    }
}

/// The attribute locations that the vertex function of the library reads, from its reflection (`vertex_inputs`). None
/// when the reflection cannot be read.
pub(crate) fn vertex_inputs(lib_dir: &Path, stem: &str) -> Option<HashSet<u32>> {
    let text = std::fs::read(lib_dir.join(format!("{stem}.json"))).ok()?;
    let v = nxb_pyjson::parse(&String::from_utf8_lossy(&text)).ok()?;
    let list = v.get("vertex_inputs")?.as_list()?;
    Some(
        list.iter()
            .filter_map(|i| match i.get("location") {
                Some(Value::Int(n)) => u32::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
    )
}

/// The pairs of shaders that the archive can hold at all, and the functions of their libraries: the check that a
/// recorded state passes before it enters the script ([`usable`], `crate::exact::usable`).
///
/// A pair is accepted when the plan accepts it under the rules of a plain base (reflection of this chain, no mesh
/// stage, no function constants, no layer output, no primitive ID, the libraries have their functions), as the host
/// draws it with stage inputs (the `_b` library) or pulling (the `_bp` library). The state's own functions must be in
/// the libraries without function constants; they need not be the entries the plan chose: the host draws a pair with
/// the integer entry (`main0_u`) when its targets are integer formats.
pub(crate) struct Acceptor<'a> {
    lib_dir: &'a Path,
    /// The (vertex library stem, fragment library stem) of every accepted pair.
    stems: HashSet<(String, String)>,
    functions: std::collections::HashMap<String, Option<Vec<(String, bool)>>>,
}

impl<'a> Acceptor<'a> {
    pub(crate) fn new(
        lib_dir: &'a Path,
        chain: &str,
        pairs: &[(String, String)],
        zero: bool,
    ) -> Self {
        let mut stems = HashSet::new();
        for with_inputs in [false, true] {
            for b in plan_with(lib_dir, chain, pairs, zero, with_inputs).bases {
                stems.insert((b.0, b.2));
            }
        }
        Self {
            lib_dir,
            stems,
            functions: Default::default(),
        }
    }

    /// Whether the library `stem` has `entry`, and it has no function constants.
    fn entry_ok(&mut self, stem: &str, entry: &str) -> bool {
        let dir = self.lib_dir;
        let fns = self
            .functions
            .entry(stem.to_string())
            .or_insert_with(|| crate::plan::functions(&dir.join(format!("{stem}.metallib"))));
        fns.as_ref()
            .and_then(|f| f.iter().rev().find(|(n, _)| n == entry))
            .is_some_and(|(_, constants)| !constants)
    }

    /// Whether a state with these functions can be archived.
    pub(crate) fn accepts(&mut self, vertex: &Func, fragment: &Func) -> bool {
        self.stems.contains(&(vertex.0.clone(), fragment.0.clone()))
            && self.entry_ok(&vertex.0, &vertex.1)
            && self.entry_ok(&fragment.0, &fragment.1)
    }
}

/// The bases of `states` that the archive can hold: those whose pair the plan accepts under the same rules as a plain
/// base (see [`Acceptor`]) and that name functions of the libraries. The libraries of the pairs must be in
/// `lib_dir`. A vertex shader with stage inputs is accepted here, which the plain plan leaves out. A state whose vertex
/// descriptor the compiler would refuse ([`StateBase::invalid`]) is left out and comes back in the second list.
pub fn usable(
    lib_dir: &Path,
    chain: &str,
    states: &[StateBase],
    zero: bool,
) -> (Vec<StateBase>, Vec<crate::Dropped>) {
    let mut pairs = Vec::new();
    let mut known = HashSet::new();
    for s in states {
        if let Some(p) = s.pair_keys()
            && known.insert(p.clone())
        {
            pairs.push(p);
        }
    }
    let mut ok = Acceptor::new(lib_dir, chain, &pairs, zero);
    let (mut kept, mut dropped) = (Vec::new(), Vec::new());
    let mut inputs: std::collections::HashMap<String, Option<HashSet<u32>>> = Default::default();
    for s in states {
        if s.pair_keys().is_none() || !ok.accepts(&s.vertex, &s.fragment) {
            continue;
        }
        let used = inputs
            .entry(s.vertex.0.clone())
            .or_insert_with(|| vertex_inputs(lib_dir, &s.vertex.0));
        // `NXB_SEEN_NO_CHECK=1` skips the check, to see what the compiler does with a state ([`crate::compile_resilient`])
        let check = std::env::var_os("NXB_SEEN_NO_CHECK").is_none();
        match s.invalid(used.as_ref()).filter(|_| check) {
            None => kept.push(s.clone()),
            Some(reason) => dropped.push(crate::Dropped {
                what: format!(
                    "{}.{} + {}.{}",
                    s.vertex.0, s.vertex.1, s.fragment.0, s.fragment.1
                ),
                reason,
            }),
        }
    }
    (kept, dropped)
}

#[cfg(test)]
mod tests;
