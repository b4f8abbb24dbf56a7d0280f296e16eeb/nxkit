//! The MSL back end: the structured program to Metal Shading Language, with the typed reflection of each library.
//!
//! This replaces the SPIR-V generator, `spirv-cross` and the MSL text passes. The output keeps the contract of the
//! engine host: the argument buffer at `[[buffer(0)]]` with `[[id(n)]]` members, the entry `main0` and the extra
//! entries, the library files `<key>_b`, `_bf`, `_bp`, `_bv`, `_bx`, `_bg*`, `_bl*`, `_bgv`, and `_b.json` as
//! [`refl::Reflection`] writes it. Refer to `docs/translator.md`.

/// The version of the MSL back end. The stage step keys its translation store on it besides the chain version.
///
/// Bump the number whenever a change alters the MSL text or the reflection for an unchanged input: a new emitter
/// choice, a new helper, a new reflection field or a changed ABI constant. A change that keeps the output equal needs no bump.
/// A change that alters the output only of the shaders that use one feature of [`revision::FEATURES`] bumps that
/// feature's revision instead, so the other shaders keep their translations (`docs/early-cutoff.md`).
pub const TRANSLATOR_ID: &str = "msl-2.20";

pub mod abi;
#[cfg(test)]
mod alu_tests;
pub mod args;
#[cfg(test)]
mod bind_tests;
pub mod buffers;
pub mod control;
mod decls;
pub mod emit;
pub mod entry;
pub mod facts;
pub mod facts_ps;
pub mod facts_vs;
pub mod facts_vs_screen;
pub mod facts_vs_sym;
pub mod first_store;
#[cfg(test)]
mod gaps_tests;
pub mod geom;
pub mod geom_lean;
#[cfg(test)]
mod geom_lean_tests;
pub mod geom_reader;
#[cfg(test)]
mod geom_tests;
#[cfg(test)]
mod gmem_tests;
pub mod handles;
pub mod hmma;
pub mod inst_alu;
pub mod inst_img;
pub mod inst_mem;
pub mod inst_misc;
pub mod inst_query;
pub mod inst_tex;
pub mod io;
pub mod ioidx;
#[cfg(test)]
mod ioidx_tests;
pub mod library;
#[cfg(test)]
mod loop_tests;
pub mod mesh;
#[cfg(test)]
mod mesh_pad_tests;
mod mesh_tests;
pub mod pool;
#[cfg(test)]
mod pool_tests;
pub mod prelude;
#[cfg(test)]
mod raw_last_tests;
pub mod refl;
pub mod revision;
pub mod shared_store;
pub mod spill;
#[cfg(test)]
mod tensor_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_fetch;
#[cfg(test)]
mod tests_ps;
#[cfg(test)]
mod tests_ps_facts;
pub mod texoff;
pub mod types;
pub mod usage;
pub mod variant;
#[cfg(test)]
mod verify_tests;
pub mod walk;
pub mod wide;
#[cfg(test)]
mod wide_tests;

use crate::job::Job;
use crate::translation::msl_entry::structure_job;
pub use emit::Options;
pub use refl::Reflection;

/// What the stage step knows about one shader besides its job and code.
#[derive(Clone, Debug, Default)]
pub struct Context<'a> {
    /// The chain version V.
    pub chain: &'a str,
    /// `<package>#<index>`.
    pub source: String,
    /// The paired shader of the game's program.
    pub pc_paired: &'a str,
    /// Vertex shaders: the inputs of the paired pixel shaders (`vs_pad`).
    pub partners: &'a [StageInputs],
    /// The key is not in the answer table, so the queries use defaults.
    pub no_facts: bool,
    /// Pixel shaders: the profile's `dual_want.tsv` lists the key, so the library gets `main0_d`.
    pub dual_source: bool,
    /// Pixel shaders: the profile's `bf_want.tsv` lists the key, so a shader without a rewritable sample gets `_bf`.
    pub fetch_outputs_only: bool,
}

/// One user varying a pixel shader reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Varying {
    /// The location.
    pub location: u32,
    /// The MSL type (`float4`, `int2`, ...).
    pub msl_type: String,
    /// The interpolation qualifier (`""`, `flat`, `center_no_perspective`, ...).
    pub interpolation: String,
    /// The components the pixel shader reads, one bit each (bit 3 is w). All four when its map does not name them.
    pub read: u8,
}

/// The user varyings a pixel shader reads, in location order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StageInputs {
    /// The varyings.
    pub varyings: Vec<Varying>,
}

/// The entries of a pixel shader that the side library (`_bx`) takes: the alpha test and the logic op functions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Side {
    /// Alpha test entries.
    pub alpha: Vec<String>,
    /// Logic op entries.
    pub logic: Vec<String>,
}

/// One library of a shader: one `.metal` source, compiled to one `.metallib`.
#[derive(Clone, Debug, PartialEq)]
pub struct Library {
    /// The file suffix: `_b`, `_bf`, `_bp`, `_bv`, `_bg0`, `_bgv`, `_bl0`, ...
    pub suffix: String,
    /// The MSL source.
    pub msl: String,
    /// The library's own JSON file. `None` when the library has none (its facts are a field of `_b.json`).
    pub reflection: Option<Reflection>,
}

/// The translation of one shader.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Translated {
    /// The shader key.
    pub key: String,
    /// `vs`, `ps`, `cs`, `gs`, `ts`, `ms`.
    pub stage: String,
    /// The libraries. The first one is `_b`.
    pub libraries: Vec<Library>,
    /// Pixel shaders: what the shader reads, for its paired vertex shaders.
    pub inputs: StageInputs,
    /// Pixel shaders: the side library units, when the shader has any.
    pub side: Option<Side>,
}

/// Why a shader has no translation. `reason` is one of the fixed reason strings of `docs/shader-step.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The reason string (`translation failed`, `not ported`, `MSL failed`, ...).
    pub reason: String,
    /// The detail.
    pub detail: String,
}

impl Failure {
    /// A failure with a reason and a detail.
    pub fn new(reason: &str, detail: impl Into<String>) -> Failure {
        Failure {
            reason: reason.to_string(),
            detail: detail.into(),
        }
    }
}

/// The code hash of the vertex shader that counts the visible vertices of an occlusion query.
const VISIBILITY_COUNTER: &str = "f5f4d23149565acd852f6b7091f64cc8cb3aa019ccb33ba3f88ced62c6d1ff7b";

fn failure(e: String) -> Failure {
    if e.starts_with("unported:") {
        Failure::new("not ported", e)
    } else if let Some(d) = e.strip_prefix("mesh stage failed: ") {
        Failure::new("mesh stage failed", d)
    } else {
        Failure::new("translation failed", e)
    }
}

/// The options of one translation: what the shader is and what the stage step knows about it.
fn options(stage: &str, code: &[u8], ctx: &Context<'_>) -> Options {
    Options {
        visibility_counter: stage == "vs" && nxb_model::sha256(code).to_hex() == VISIBILITY_COUNTER,
        dual_source: ctx.dual_source,
        fetch_outputs_only: ctx.fetch_outputs_only,
        plain_decls: false,
    }
}

/// Translate one shader from its job and machine code to its libraries.
pub fn translate(job: &Job, code: &[u8], ctx: &Context<'_>) -> Result<Translated, Failure> {
    let stage = job.stage.as_str();
    if matches!(stage, "hs" | "ds") {
        let kind = if stage == "hs" {
            "control"
        } else {
            "evaluation"
        };
        return Err(Failure::new(
            "not ported",
            format!(
                "unported: tessellation {kind} shader ({stage} stage), the front end has no tessellation"
            ),
        ));
    }
    if matches!(stage, "ts" | "ms") {
        return Err(Failure::new(
            "translation failed",
            format!("a {stage} job is written as stage cs with a mesh_stage"),
        ));
    }
    if !matches!(stage, "vs" | "ps" | "cs" | "gs") {
        return Err(Failure::new(
            "not ported",
            format!("unported: {stage} stage"),
        ));
    }
    if stage == "cs" {
        mesh::check(job, code).map_err(failure)?;
    }
    let opt = options(stage, code, ctx);
    if stage == "gs" {
        return geom::translate(job, code, ctx, &opt).map_err(failure);
    }
    let structured = structure_job(job, code).map_err(failure)?;
    library::build(&structured, ctx, &opt).map_err(failure)
}
