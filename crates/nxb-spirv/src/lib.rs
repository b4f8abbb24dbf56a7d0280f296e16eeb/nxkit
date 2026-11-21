//! Shaders of games that draw with Vulkan instead of NVN, translated to MSL for the host's Vulkan layer (D21).
//!
//! The game hands the words of a SPIR-V module to `vkCreateShaderModule`, so the host finds a shader again by the hash of
//! those bytes ([`key_of`]). [`translate`] reads one module with naga's SPIR-V reader and writes one Metal function with
//! naga's MSL writer, plus the Metal slot of each descriptor it takes. The slots follow the old engine's
//! `dm/gfx/tools/vk_shaders.py`.
//!
//! Before naga reads a module, the crate fixes up its words: array strides on struct members, combined image samplers
//! split into an image and a sampler, and 1D images made 2D (the host has no 1D textures). After it reads the module,
//! array varyings become one varying per element, as Metal has no arrays in a stage's inputs or outputs.
//!
//! The crate is pure and knows no file format of the host: the caller names the functions, writes the host's table,
//! compiles the MSL with Metal's tools and writes the files.

mod image1d;
mod io;
mod msl;
mod slots;
mod spirv;

pub use spirv::is_spirv;

use std::collections::BTreeSet;

use naga::valid::{Capabilities, ModuleInfo, ValidationFlags, Validator};

use crate::spirv::{DEC_BINDING, DEC_SET, Spv};

/// The stage of a translated module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// A vertex shader.
    Vertex,
    /// A fragment shader.
    Fragment,
}

/// What a descriptor is, and which Metal slots it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// A uniform buffer, at a buffer slot.
    Uniform,
    /// A storage buffer, at a buffer slot.
    Storage,
    /// A sampled or storage image, at a texture slot.
    Texture,
    /// A sampler, at a sampler slot.
    Sampler,
    /// A combined image sampler, at the texture slot and the sampler slot of the same number.
    Combined,
    /// The push-constant block, at a buffer slot. Its set and binding are 0.
    Push,
}

impl Kind {
    /// Whether the kind takes a buffer slot.
    pub const fn is_buffer(self) -> bool {
        matches!(self, Kind::Uniform | Kind::Storage | Kind::Push)
    }

    /// Whether the kind takes a texture slot.
    pub const fn is_texture(self) -> bool {
        matches!(self, Kind::Texture | Kind::Combined)
    }

    /// Whether the kind takes a sampler slot.
    pub const fn is_sampler(self) -> bool {
        matches!(self, Kind::Sampler | Kind::Combined)
    }
}

/// One descriptor binding of the function and its first Metal slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Resource {
    /// What the descriptor is.
    pub kind: Kind,
    /// The descriptor set in the SPIR-V.
    pub set: u32,
    /// The binding in the SPIR-V.
    pub binding: u32,
    /// The first Metal slot.
    pub slot: u32,
    /// The number of descriptors at the slots from `slot` on. It is 1 for now: an array of descriptors is an error.
    pub count: u32,
}

/// One translated shader stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shader {
    /// The stage.
    pub stage: Stage,
    /// The resources the function takes, in (set, binding) order and the push constants last.
    pub resources: Vec<Resource>,
    /// The MSL source with one entry point, the function named `entry` in [`translate`].
    pub msl: String,
    /// For a fragment shader with an output at location 1: its dual-source variant, the function named `dual`, with that
    /// output as the second blend source of attachment 0.
    pub msl_dual: Option<String>,
}

/// What became of one SPIR-V module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A vertex or fragment shader in MSL.
    Shader(Shader),
    /// A stage the host does not take, by its short name (`comp`, `geom`, `tesc`, `tese` and so on).
    Skipped(&'static str),
}

/// The naga version and the revision of this crate's passes, for the second line of the host's table and for cache keys. A change to the
/// passes that changes the output raises the revision.
pub fn translator_id() -> String {
    "nxb-spirv 2, naga 30.0.1".to_string()
}

/// The key of a module: the first 8 bytes of the SHA-256 of the game's bytes, which the host hashes as well.
pub fn key_of(bytes: &[u8]) -> [u8; 8] {
    let mut key = [0u8; 8];
    key.copy_from_slice(&nxb_model::sha256(bytes).as_bytes()[..8]);
    key
}

/// The short stage name of a SPIR-V execution model.
fn stage_name(model: u32) -> &'static str {
    match model {
        0 => "vert",
        1 => "tesc",
        2 => "tese",
        3 => "geom",
        4 => "frag",
        5 => "comp",
        5267 | 5364 => "task",
        5268 | 5365 => "mesh",
        5313..=5318 => "rt",
        _ => "other",
    }
}

/// Translates one SPIR-V module of a Vulkan game into the Metal function `entry`, and a fragment shader with an output at
/// location 1 also into its dual-source variant `dual`. The vertex shader's clip-space Y is negated, as Vulkan's points
/// down and Metal's points up. Its depth range stays as it is. An error is a module the host cannot use.
pub fn translate(bytes: &[u8], entry: &str, dual: &str) -> Result<Outcome, String> {
    let mut s = Spv::parse(bytes)?;
    let stage = match stage_name(s.execution_model()?) {
        "vert" => Stage::Vertex,
        "frag" => Stage::Fragment,
        other => return Ok(Outcome::Skipped(other)),
    };
    spirv::fix_strides(&mut s);
    image1d::as_2d(&mut s)?;
    let twins = spirv::split_combined(&mut s)?;
    let combined: BTreeSet<(u32, u32)> = twins
        .keys()
        .filter_map(|&v| {
            Some((
                s.decoration(v, DEC_SET).unwrap_or(0),
                s.decoration(v, DEC_BINDING)?,
            ))
        })
        .collect();
    let (m, info) = to_ir(&s.words(), stage == Stage::Vertex, entry)?;
    let slots = slots::assign(&m, &combined)?;
    let src = msl::write(&m, &info, &slots, entry)?;
    let msl_dual = match stage {
        Stage::Fragment => msl::dual_source(&src, entry, dual),
        Stage::Vertex => None,
    };
    Ok(Outcome::Shader(Shader {
        stage,
        resources: slots.used(&info, 0),
        msl: src,
        msl_dual,
    }))
}

/// Reads SPIR-V `words` into naga's IR, splits array varyings, names the entry point `name` and validates the module.
/// `flip_y` negates the clip-space Y of a vertex shader.
pub(crate) fn to_ir(
    words: &[u32],
    flip_y: bool,
    name: &str,
) -> Result<(naga::Module, ModuleInfo), String> {
    let o = naga::front::spv::Options {
        adjust_coordinate_space: flip_y,
        strict_capabilities: false,
        block_ctx_dump_prefix: None,
    };
    let mut m = naga::front::spv::Frontend::new(words.iter().copied(), &o)
        .parse()
        .map_err(|e| format!("SPIR-V reader: {e}"))?;
    if m.entry_points.len() != 1 {
        return Err(format!(
            "{} entry points (one expected)",
            m.entry_points.len()
        ));
    }
    io::flatten_varyings(&mut m)?;
    if let Some(ep) = m.entry_points.first_mut() {
        ep.name = name.to_string();
    }
    // a game may give two variables one binding (Vulkan allows aliased descriptors), so the binding collision check is off
    let info = Validator::new(
        ValidationFlags::all() - ValidationFlags::BINDINGS,
        Capabilities::all(),
    )
    .validate(&m)
    .map_err(|e| {
        let e = e.into_inner();
        let mut t = format!("validation: {e}");
        let mut src = std::error::Error::source(&e);
        while let Some(c) = src {
            t += &format!(": {c}");
            src = c.source();
        }
        t
    })?;
    Ok((m, info))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
