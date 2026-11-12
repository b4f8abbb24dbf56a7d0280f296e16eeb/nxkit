//! Recorded states as fully specialized pipelines, the "exact" pipelines of the archive.
//!
//! [`crate::seen`] adds the unspecialized base of each recorded state, and the host specializes it to the colour state
//! of the draw. That is a compiler call for every pipeline of the list, about 40 ms each (1600 of them took 70 s at
//! the start of Ocarina of Time). This module goes one step further: for each recorded render pipeline it writes the
//! descriptor that the host asks for, colour formats, blend factors and operations, write masks, vertex layout, class,
//! samples, alpha to coverage and ICB support all set, so that the archive holds the pipeline itself and the host's
//! lookup of the exact descriptor (`nxkit-metal` `PipelineCache::render_traced`) is an archive load: no compile, no
//! specialization.
//!
//! The descriptor is the one the host builds from its `RenderPipelineDesc` (`cache.rs`, `render_descriptor`): a colour
//! slot the list gives as `_` or beyond its length stays at the descriptor's default (format invalid, blending off,
//! all channels written), a slot without a blend sets the state to `Disabled` and leaves the factors at their defaults,
//! one with a blend sets `Enabled` and all six factors and operations. A member at its default is left out.
//!
//! The pipelines are compiled in batches of their own, one per stream group, so that a change of the list recompiles
//! those batches and no batch of a base (`stream.rs`), and are shards of their own in the index
//! (`nxkit_abi::shards`, version 2). The pair of functions is the key of the index entry: a pair that the plan would
//! not accept as a base (function constants, mesh stage, layer output, primitive ID) has no exact pipeline either,
//! and a state whose descriptor the compiler would refuse is left out ([`ExactState::invalid`]).

use std::collections::HashSet;
use std::path::Path;

use nxb_pyjson::Value;

use crate::seen::{
    Acceptor, Attribute, Func, Layout, flag, layout_invalid, layout_of, stage, vertex_descriptor,
};

/// The pixel formats a render target of the host has: its name (`Debug` of `nxkit-metal` `Format`) and the script's.
const PIXEL_FORMATS: &[(&str, &str)] = &[
    ("R8Uint", "R8Uint"),
    ("R8Sint", "R8Sint"),
    ("R8Unorm", "R8Unorm"),
    ("R8Snorm", "R8Snorm"),
    ("R16Uint", "R16Uint"),
    ("R16Sint", "R16Sint"),
    ("R16Unorm", "R16Unorm"),
    ("R16Snorm", "R16Snorm"),
    ("R16Float", "R16Float"),
    ("R32Uint", "R32Uint"),
    ("R32Sint", "R32Sint"),
    ("R32Float", "R32Float"),
    ("Rg8Uint", "RG8Uint"),
    ("Rg8Sint", "RG8Sint"),
    ("Rg8Unorm", "RG8Unorm"),
    ("Rg8Snorm", "RG8Snorm"),
    ("Rg16Uint", "RG16Uint"),
    ("Rg16Sint", "RG16Sint"),
    ("Rg16Unorm", "RG16Unorm"),
    ("Rg16Snorm", "RG16Snorm"),
    ("Rg16Float", "RG16Float"),
    ("Rg32Uint", "RG32Uint"),
    ("Rg32Sint", "RG32Sint"),
    ("Rg32Float", "RG32Float"),
    ("Rgba8Uint", "RGBA8Uint"),
    ("Rgba8Sint", "RGBA8Sint"),
    ("Rgba8Unorm", "RGBA8Unorm"),
    ("Rgba8UnormSrgb", "RGBA8Unorm_sRGB"),
    ("Rgba8Snorm", "RGBA8Snorm"),
    ("Bgra8Unorm", "BGRA8Unorm"),
    ("Bgra8UnormSrgb", "BGRA8Unorm_sRGB"),
    ("Rgba16Uint", "RGBA16Uint"),
    ("Rgba16Sint", "RGBA16Sint"),
    ("Rgba16Unorm", "RGBA16Unorm"),
    ("Rgba16Snorm", "RGBA16Snorm"),
    ("Rgba16Float", "RGBA16Float"),
    ("Rgba32Uint", "RGBA32Uint"),
    ("Rgba32Sint", "RGBA32Sint"),
    ("Rgba32Float", "RGBA32Float"),
    ("Rgb10A2Unorm", "RGB10A2Unorm"),
    ("Rgb10A2Uint", "RGB10A2Uint"),
    ("Rg11B10Float", "RG11B10Float"),
    ("Rgb9E5Float", "RGB9E5Float"),
    ("B5G6R5Unorm", "B5G6R5Unorm"),
    ("Bgr5A1Unorm", "BGR5A1Unorm"),
    ("A1Bgr5Unorm", "A1BGR5Unorm"),
    ("Abgr4Unorm", "ABGR4Unorm"),
];

/// The script's name of a host format.
fn pixel_format(host: &str) -> Option<&'static str> {
    PIXEL_FORMATS
        .iter()
        .find(|(h, _)| *h == host)
        .map(|(_, s)| *s)
}

/// The script's name of a blend factor of the host (`Debug` of `BlendFactor`). The second source alpha factors have no
/// name in the script.
fn blend_factor(host: &str) -> Option<&'static str> {
    Some(match host {
        "Zero" => "Zero",
        "One" => "One",
        "SrcColor" => "SourceColor",
        "OneMinusSrcColor" => "OneMinusSourceColor",
        "SrcAlpha" => "SourceAlpha",
        "OneMinusSrcAlpha" => "OneMinusSourceAlpha",
        "DstColor" => "DestinationColor",
        "OneMinusDstColor" => "OneMinusDestinationColor",
        "DstAlpha" => "DestinationAlpha",
        "OneMinusDstAlpha" => "OneMinusDestinationAlpha",
        "SrcAlphaSaturated" => "SourceAlphaSaturated",
        "BlendColor" => "BlendColor",
        "OneMinusBlendColor" => "OneMinusBlendColor",
        "BlendAlpha" => "BlendAlpha",
        "OneMinusBlendAlpha" => "OneMinusBlendAlpha",
        "Src1Color" => "Source1Color",
        "OneMinusSrc1Color" => "OneMinusSource1Color",
        _ => return None,
    })
}

/// The script's name of a blend operation (the host's `Debug` names are the same).
fn blend_op(host: &str) -> Option<&'static str> {
    ["Add", "Subtract", "ReverseSubtract", "Min", "Max"]
        .into_iter()
        .find(|n| *n == host)
}

/// The blend of a colour slot: source and destination RGB factor, RGB operation, source and destination alpha factor,
/// alpha operation, as the script names them.
pub type Blend = [&'static str; 6];

/// One colour attachment of a recorded state.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ColorSlot {
    /// The script's pixel format name.
    pub format: &'static str,
    /// The channels written, bits R=1, G=2, B=4, A=8.
    pub mask: u8,
    /// The blend, or none for plain replacement.
    pub blend: Option<Blend>,
}

/// A recorded render pipeline with everything the host sets on its descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExactState {
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
    /// The colour slots; `None` is an empty one.
    pub colors: Vec<Option<ColorSlot>>,
}

fn color_of(slot: &str) -> Option<Option<ColorSlot>> {
    if slot == "_" {
        return Some(None);
    }
    let mut p = slot.splitn(3, ':');
    let format = pixel_format(p.next()?)?;
    let mask = u8::from_str_radix(p.next()?, 16).ok()?;
    let blend = match p.next()? {
        "-" => None,
        b => {
            let n: Vec<&str> = b.split('/').collect();
            let [a, b, c, d, e, f] = n[..] else {
                return None;
            };
            Some([
                blend_factor(a)?,
                blend_factor(b)?,
                blend_op(c)?,
                blend_factor(d)?,
                blend_factor(e)?,
                blend_op(f)?,
            ])
        }
    };
    Some(Some(ColorSlot {
        format,
        mask: mask & 0xF,
        blend,
    }))
}

fn colors_of(s: &str) -> Option<Vec<Option<ColorSlot>>> {
    if s == "-" {
        return Some(Vec::new());
    }
    s.split(',').map(color_of).collect()
}

/// The state of one `R` line with a fragment function, or none for a line that does not parse, names a format or a
/// factor the script has no name for, or has no fragment stage.
fn state_of_line(l: &str) -> Option<ExactState> {
    let f: Vec<&str> = l.split('\t').collect();
    if f.len() != 14 || f[0] != "R" {
        return None;
    }
    let topology = f[9];
    if !["Unspecified", "Point", "Line", "Triangle"].contains(&topology) {
        return None;
    }
    let (attributes, layouts) = layout_of(f[13])?;
    Some(ExactState {
        vertex: stage(f[4], f[5])??,
        fragment: stage(f[6], f[7])??,
        attributes,
        layouts,
        topology: topology.to_string(),
        samples: f[10].parse().ok()?,
        alpha_to_coverage: flag(f[11])?,
        icb: flag(f[8])?,
        colors: colors_of(f[12])?,
    })
}

/// The exact states of a list text, without repeats, in the order of the list. A text of another version gives none.
pub fn read_list(text: &str) -> Vec<ExactState> {
    let mut lines = text.lines();
    if lines.next() != Some(crate::seen::HEADER) {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    lines
        .filter_map(state_of_line)
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

/// The exact states of several list files, without repeats. A file that cannot be read is left out.
pub fn read_lists(paths: &[std::path::PathBuf]) -> Vec<ExactState> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        for s in read_list(&text) {
            if seen.insert(s.clone()) {
                out.push(s);
            }
        }
    }
    out
}

fn mask_text(mask: u8) -> String {
    match mask & 0xF {
        0xF => "All".into(),
        0 => "None".into(),
        m => [(1, "Red"), (2, "Green"), (4, "Blue"), (8, "Alpha")]
            .into_iter()
            .filter(|(bit, _)| m & bit != 0)
            .map(|(_, n)| n)
            .collect::<Vec<_>>()
            .join(" "),
    }
}

impl ColorSlot {
    /// The attachment object of the script. Members at their default are left out.
    fn descriptor(&self) -> Value {
        let s = |v: &str| Value::str(v.to_string());
        let mut m = vec![("pixel_format".to_string(), s(self.format))];
        if self.mask != 0xF {
            m.push(("write_mask".into(), s(&mask_text(self.mask))));
        }
        if let Some([src_rgb, dst_rgb, op_rgb, src_alpha, dst_alpha, op_alpha]) = self.blend {
            m.push(("blending_state".into(), s("Enabled")));
            m.push(("alpha_blend_operation".into(), s(op_alpha)));
            m.push(("rgb_blend_operation".into(), s(op_rgb)));
            m.push(("destination_alpha_blend_factor".into(), s(dst_alpha)));
            m.push(("destination_rgb_blend_factor".into(), s(dst_rgb)));
            m.push(("source_alpha_blend_factor".into(), s(src_alpha)));
            m.push(("source_rgb_blend_factor".into(), s(src_rgb)));
        }
        Value::Object(m)
    }
}

impl ExactState {
    /// The shader keys of the pair: the stems without `_b` or `_bp`.
    pub fn pair_keys(&self) -> Option<(String, String)> {
        let v = self
            .vertex
            .0
            .strip_suffix("_bp")
            .or_else(|| self.vertex.0.strip_suffix("_b"))?;
        let f = self.fragment.0.strip_suffix("_b")?;
        Some((v.to_string(), f.to_string()))
    }

    /// Why the compiler would refuse this state, or none: the vertex descriptor (`layout_invalid`) and a sample count
    /// that Metal does not take.
    pub fn invalid(&self, used: Option<&HashSet<u32>>) -> Option<String> {
        if ![1, 2, 4, 8].contains(&self.samples) {
            return Some(format!("sample count {}", self.samples));
        }
        layout_invalid(&self.attributes, &self.layouts, used)
    }

    /// The render pipeline descriptor of the script: the two function references and every member the host sets. Members
    /// at their default are left out.
    pub fn descriptor(&self, vertex: Value, fragment: Value) -> Value {
        let mut m = vec![
            ("vertex_function_descriptor".to_string(), vertex),
            ("fragment_function_descriptor".to_string(), fragment),
        ];
        if !self.attributes.is_empty() || !self.layouts.is_empty() {
            m.push((
                "vertex_descriptor".into(),
                vertex_descriptor(&self.attributes, &self.layouts),
            ));
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
        if self.colors.iter().any(Option::is_some) {
            let list = self
                .colors
                .iter()
                .map(|c| {
                    c.as_ref()
                        .map_or_else(|| Value::Object(Vec::new()), ColorSlot::descriptor)
                })
                .collect();
            m.push(("color_attachments".into(), Value::List(list)));
        }
        Value::Object(m)
    }
}

/// The states of `states` that the archive can hold: their pair passes the plan under the rules of a plain base (see
/// [`crate::seen::usable`]), and the compiler would not refuse them. The second list has the refused ones.
pub fn usable(
    lib_dir: &Path,
    chain: &str,
    states: &[ExactState],
    zero: bool,
) -> (Vec<ExactState>, Vec<crate::Dropped>) {
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
            .or_insert_with(|| crate::seen::vertex_inputs(lib_dir, &s.vertex.0));
        let check = std::env::var_os("NXB_SEEN_NO_CHECK").is_none();
        match s.invalid(used.as_ref()).filter(|_| check) {
            None => kept.push(s.clone()),
            Some(reason) => dropped.push(crate::Dropped {
                what: format!(
                    "exact {}.{} + {}.{}",
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
