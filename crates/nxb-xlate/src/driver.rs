//! The retry loop of a job and the resource lists a translation returns.

use crate::accessor::{Accessor, Binding, image_types_of};
use crate::job::Job;
use crate::types::SamplerType;
use std::collections::BTreeMap;

/// A buffer binding of the program info.
#[derive(Clone, Debug, Default)]
pub struct BufferInfo {
    pub binding: i64,
    pub slot: i64,
    pub cb_slot: i64,
    pub cb_offset: i64,
    pub flags: i64,
}

/// A texture or image of the program info.
#[derive(Clone, Debug)]
pub struct TexInfo {
    pub binding: i64,
    pub ty: u32,
    pub format: usize,
    pub cbuf_slot: i64,
    pub handle: i64,
    pub array_length: i64,
    pub separate: bool,
    pub flags: i64,
}

/// What the front end returns: the resource lists of a program.
#[derive(Clone, Debug, Default)]
pub struct Program {
    pub cbufs: Vec<BufferInfo>,
    pub sbufs: Vec<BufferInfo>,
    pub textures: Vec<TexInfo>,
    pub images: Vec<TexInfo>,
    pub uses_frag_coord: bool,
    /// The textures and images the render scale changes (see `xform::texture`).
    pub scaled: Vec<crate::xform::helper::ScaleUse>,
    pub frag_out_map: i64,
    /// `GetResourceReservations().OutputSizePerInvocation` (geometry)
    pub geometry_stride: i64,
}

fn bindings(l: &[TexInfo]) -> Vec<Binding> {
    l.iter()
        .map(|t| Binding {
            ty: crate::types::SamplerType(t.ty),
            cbuf_slot: t.cbuf_slot as i32,
            handle: t.handle as i32,
            array_length: t.array_length as i32,
        })
        .collect()
}

/// The result of the retry loop of one job.
pub struct Retried<T> {
    /// The job that produced the result (`softfloat64` set after the FP64 retry).
    pub job: Job,
    /// The accessor's notes.
    pub notes: Vec<String>,
    /// The texel fetches of 1D textures the accessor recorded.
    pub fetch1d: std::collections::BTreeSet<i32>,
    /// What the step returned.
    pub value: T,
    /// The program info of the step.
    pub program: Program,
    /// The guessed image types of the second pass.
    pub image_types: Option<BTreeMap<i32, SamplerType>>,
    /// The guessed texture types of the second pass.
    pub texture_types: Option<BTreeMap<i32, SamplerType>>,
}

/// One translation of a job: the value, its program info and whether it uses 64 bit floats.
pub type Step<'a, T> = dyn Fn(&Job, &mut Accessor) -> Result<(T, Program, bool), String> + 'a;

/// The retry loop of `Main`: translate, translate again with the guessed image and texture types, and translate
/// once more with `softfloat64` when the result still uses 64 bit floats.
pub fn retry<T>(job: &Job, step: &Step<'_, T>) -> Result<Retried<T>, String> {
    let mut acc = Accessor::new(job);
    let (mut v, mut p, mut fp64) = step(job, &mut acc)?;
    let mut image_types = None;
    let mut texture_types = None;
    if acc.image_query_guessed {
        let t = image_types_of(&bindings(&p.images));
        if !t.is_empty() {
            image_types = Some(t);
        }
    }
    let tt = acc.texture_types_of(&bindings(&p.textures));
    if !tt.is_empty() {
        texture_types = Some(tt);
    }
    if image_types.is_some() || texture_types.is_some() {
        acc = Accessor::with_types(job, image_types.clone(), texture_types.clone());
        (v, p, fp64) = step(job, &mut acc)?;
    }
    if fp64 {
        let mut j2 = job.clone();
        j2.softfloat64 = true;
        let mut acc2 = Accessor::with_types(&j2, image_types.clone(), texture_types.clone());
        let (v2, p2, _) = step(&j2, &mut acc2)?;
        acc2.notes
            .push("FP64 lowered to FP32 (DoubleToFloat)".into());
        let (notes, fetch1d) = (std::mem::take(&mut acc2.notes), acc2.fetch1d.clone());
        drop(acc2);
        return Ok(Retried {
            job: j2,
            notes,
            fetch1d,
            value: v2,
            program: p2,
            image_types,
            texture_types,
        });
    }
    Ok(Retried {
        job: job.clone(),
        notes: acc.notes,
        fetch1d: acc.fetch1d,
        value: v,
        program: p,
        image_types,
        texture_types,
    })
}
