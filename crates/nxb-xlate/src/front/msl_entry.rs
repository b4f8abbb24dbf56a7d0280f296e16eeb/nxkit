//! The entry of the MSL back end into the translation: a job and its machine code to the structured program, with the
//! retry loop of the driver (guessed texture and image types, 64 bit float lowering).

use super::core::structure_context;
use super::geometry::{structure_geometry_compute, structure_geometry_reader};
use super::pipeline::options_of;
use crate::accessor::Accessor;
use crate::driver::{Program, retry};
use crate::job::Job;
use crate::props::Props;
use crate::structured::StructInfo;
use crate::structured::walk::uses_float64;
use crate::translation::context::TranslatorContext;
use std::collections::BTreeSet;

/// The result of the front end for the MSL emitter.
pub struct Structured {
    /// The job that produced it (`softfloat64` set after the float lowering retry).
    pub job: Job,
    /// The structured program.
    pub info: StructInfo,
    /// The definitions, host capabilities and resource properties.
    pub props: Props,
    /// The resource lists.
    pub program: Program,
    /// The accessor's notes.
    pub notes: Vec<String>,
    /// The texture slots read by a 1D texel fetch that became a wrapped 2D read.
    pub fetch1d: BTreeSet<i32>,
    /// The vertex shader that draws the vertices of a geometry job.
    pub geometry_reader: bool,
}

/// Run the front end of one job to the structured program.
pub fn structure_job(job: &Job, code: &[u8]) -> Result<Structured, String> {
    let geometry = job.stage == "gs";
    if geometry && job.geometry_topology.is_none() {
        return Err("geometry job without an input topology".into());
    }
    let r = retry(job, &|j: &Job, acc| {
        let tc = TranslatorContext::create(code, acc, options_of(j))?;
        let debug = tc.options.debug_mode;
        let (info, props, program) = if geometry {
            structure_geometry_compute(&tc, acc, debug)?
        } else {
            structure_context(&tc, acc, debug)?
        };
        let fp64 = uses_float64(&info);
        Ok(((info, props), program, fp64))
    })?;
    let (info, props) = r.value;
    Ok(Structured {
        job: r.job,
        info,
        props,
        program: r.program,
        notes: r.notes,
        fetch1d: r.fetch1d,
        geometry_reader: false,
    })
}

/// Run the front end of the vertex shader that draws the vertices of a geometry compute job
/// (`GenerateVertexPassthroughForCompute`). The reader has no textures, so it needs no retry.
pub fn structure_geometry_reader_job(job: &Job, code: &[u8]) -> Result<Structured, String> {
    let mut acc = Accessor::new(job);
    let tc = TranslatorContext::create(code, &mut acc, options_of(job))?;
    let debug = tc.options.debug_mode;
    let (info, props) = structure_geometry_reader(&tc, &mut acc, debug)?;
    let mut reader = job.clone();
    reader.stage = "vs".into();
    Ok(Structured {
        job: reader,
        info,
        props,
        program: Program::default(),
        notes: acc.notes.clone(),
        fetch1d: BTreeSet::new(),
        geometry_reader: true,
    })
}
