//! Shaders of games that draw with Vulkan instead of NVN, translated to MSL for the host's Vulkan layer (D21).
//!
//! The game hands the words of a SPIR-V module to `vkCreateShaderModule`, so the host finds a shader again by the hash of
//! those bytes. [`translate`] reads one module with naga's SPIR-V reader and writes one Metal function `vk_<key>` with
//! naga's MSL writer, plus the row of `vk_shaders.tsv` ([`nxkit_abi::vk_table`]) that says where its descriptors go. The
//! slots and rows follow the old engine's `dm/gfx/tools/vk_shaders.py`.
//!
//! Before naga reads a module, the crate fixes up its words: array strides on struct members, combined image samplers
//! split into an image and a sampler, and 1D images made 2D (the host has no 1D textures). After it reads the module,
//! array varyings become one varying per element, as Metal has no arrays in a stage's inputs or outputs.
//!
//! The crate does no I/O: the caller finds the modules, writes the table with [`nxkit_abi::vk_table::write`], compiles
//! the MSL with Metal's tools and writes the files.

mod image1d;
mod io;
mod msl;
mod slots;
mod spirv;

pub use spirv::is_spirv;

use std::collections::BTreeSet;

use naga::valid::{Capabilities, ModuleInfo, ValidationFlags, Validator};
use nxkit_abi::vk_table::{self, Key, Row, Stage};

use crate::spirv::{DEC_BINDING, DEC_SET, Spv};

/// One translated shader stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shader {
    /// The table row. Its `source` is empty: the caller sets it to where it found the module.
    pub row: Row,
    /// The MSL source with one entry point, [`vk_table::function`] of the key.
    pub msl: String,
    /// The dual-source variant [`vk_table::dual_function`] of a fragment shader with an output at location 1.
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

/// The naga version and the revision of this crate's passes, for the table's second line and for cache keys. A change to the
/// passes that changes the output raises the revision.
pub fn translator_id() -> String {
    "nxb-spirv 1, naga 30.0.1".to_string()
}

/// The table key of a module: the first 8 bytes of the SHA-256 of the game's bytes, which the host hashes as well.
pub fn key_of(bytes: &[u8]) -> Key {
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

/// Translates one SPIR-V module of a Vulkan game. The vertex shader's clip-space Y is negated, as Vulkan's points down and
/// Metal's points up. Its depth range stays as it is. An error is a module the host cannot use.
pub fn translate(bytes: &[u8]) -> Result<Outcome, String> {
    let key = key_of(bytes);
    let mut s = Spv::parse(bytes)?;
    let stage = match stage_name(s.execution_model()?) {
        "vert" => Stage::Vert,
        "frag" => Stage::Frag,
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
    let name = vk_table::function(&key);
    let (m, info) = to_ir(&s.words(), stage == Stage::Vert, &name)?;
    let slots = slots::assign(&m, &combined)?;
    let resources = slots.used(&info, 0);
    // the host refuses a row outside the table's limits (a set above 7 or a binding above 127)
    if let Some(r) = resources.iter().find(|r| !r.is_valid()) {
        return Err(format!(
            "the resource {r} is outside the limits of {}",
            vk_table::FILE
        ));
    }
    let src = msl::write(&m, &info, &slots, &name)?;
    let msl_dual = match stage {
        Stage::Frag => msl::dual_source(&src, &name),
        _ => None,
    };
    Ok(Outcome::Shader(Shader {
        row: Row {
            key,
            stage,
            resources,
            source: String::new(),
        },
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
