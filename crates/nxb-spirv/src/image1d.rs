//! 1D images as 2D images of one row, at the word level before naga reads the module.
//!
//! The host makes a 1D image of the game a 2D texture of one row, because Metal cannot render into a 1D texture, mipmap
//! it or give it depth. Hades draws a shadow map into a 512x1 target and samples it as a 1D image in
//! `LightAndShadow.frag`. So each 1D image type of the module becomes 2D. A sample takes the row's centre `(x, 0.5)`, a
//! fetch, read or write the texel `(x, 0)`, and a size query the width, as `spirv-cross --msl-texture-1d-as-2d` does.
//! Arrays of 1D images, projective samples, gradients, offsets and texel pointers of a 1D image are an error.

use std::collections::{BTreeMap, BTreeSet};

use crate::spirv::{
    Inst, OP_CAPABILITY, OP_COMPOSITE_CONSTRUCT, OP_COMPOSITE_EXTRACT, OP_CONSTANT, OP_FUNCTION,
    OP_TYPE_FLOAT, OP_TYPE_IMAGE, OP_TYPE_INT, OP_TYPE_POINTER, OP_TYPE_SAMPLED_IMAGE,
    OP_TYPE_VECTOR, Spv,
};

const DIM_1D: u32 = 0;
const DIM_2D: u32 = 1;
const CAP_SAMPLED_1D: u32 = 43;
const CAP_IMAGE_1D: u32 = 44;

const OP_IMAGE_TEXEL_POINTER: u16 = 60;
const OP_SAMPLE_IMPLICIT: u16 = 87;
const OP_SAMPLE_EXPLICIT: u16 = 88;
const OP_SAMPLE_DREF_IMPLICIT: u16 = 89;
const OP_SAMPLE_DREF_EXPLICIT: u16 = 90;
const OP_SAMPLE_PROJ_FIRST: u16 = 91;
const OP_SAMPLE_PROJ_LAST: u16 = 94;
const OP_FETCH: u16 = 95;
const OP_READ: u16 = 98;
const OP_WRITE: u16 = 99;
const OP_QUERY_SIZE_LOD: u16 = 103;
const OP_QUERY_SIZE: u16 = 104;
const OP_QUERY_LOD: u16 = 105;

/// The image operands that would need a second component: Grad, ConstOffset, Offset and ConstOffsets.
const TWO_COMPONENT_OPERANDS: u32 = 0x4 | 0x8 | 0x10 | 0x20;

/// The result type of each value id: the constants, the variables and every instruction in a function that has a result
/// type. An instruction whose second word is the use of an earlier value does not count: the first entry of an id wins,
/// and a value is defined before its uses.
fn value_types(s: &Spv) -> BTreeMap<u32, u32> {
    let mut out = BTreeMap::new();
    let start = s.first_function();
    for (k, i) in s.insts.iter().enumerate() {
        let typed = k >= start || matches!(i.op, 1 | 41..=46 | 48..=52 | 59);
        if typed && i.w.len() >= 2 {
            out.entry(i.w[1]).or_insert(i.w[0]);
        }
    }
    out
}

/// The new types and constants, which go before the first function.
struct Decls {
    add: Vec<Inst>,
}

impl Decls {
    fn find(&self, s: &[Inst], f: impl Fn(&Inst) -> bool) -> Option<u32> {
        s.iter().chain(&self.add).find(|i| f(i)).map(|i| i.w[0])
    }

    /// The type `vec2` of `scalar`.
    fn vec2(&mut self, s: &mut Spv, old: &[Inst], scalar: u32) -> u32 {
        let found = self.find(old, |i| {
            i.op == OP_TYPE_VECTOR && i.w.get(1..) == Some(&[scalar, 2])
        });
        found.unwrap_or_else(|| {
            let id = s.fresh();
            self.add
                .push(Inst::new(OP_TYPE_VECTOR, vec![id, scalar, 2]));
            id
        })
    }

    /// The 32-bit constant `bits` of type `ty`.
    fn constant(&mut self, s: &mut Spv, old: &[Inst], ty: u32, bits: u32) -> u32 {
        let is =
            |i: &&Inst| i.op == OP_CONSTANT && i.w.len() == 3 && i.w[0] == ty && i.w[2] == bits;
        if let Some(i) = old.iter().chain(&self.add).find(is) {
            return i.w[1];
        }
        let id = s.fresh();
        self.add.push(Inst::new(OP_CONSTANT, vec![ty, id, bits]));
        id
    }
}

/// Makes the 1D images of `s` 2D. Returns whether the module had one.
pub(crate) fn as_2d(s: &mut Spv) -> Result<bool, String> {
    let mut images = BTreeSet::new();
    for i in &s.insts {
        if i.op == OP_TYPE_IMAGE && i.w.len() >= 8 && i.w[2] == DIM_1D {
            if i.w[4] != 0 {
                return Err("an array of 1D images".into());
            }
            images.insert(i.w[0]);
        }
    }
    if images.is_empty() {
        return Ok(false);
    }
    let sampled: BTreeSet<u32> = s
        .insts
        .iter()
        .filter(|i| i.op == OP_TYPE_SAMPLED_IMAGE && i.w.get(1).is_some_and(|t| images.contains(t)))
        .map(|i| i.w[0])
        .collect();
    let pointers: BTreeSet<u32> = s
        .insts
        .iter()
        .filter(|i| i.op == OP_TYPE_POINTER && i.w.get(2).is_some_and(|t| images.contains(t)))
        .map(|i| i.w[0])
        .collect();
    let ints: BTreeSet<u32> = s
        .insts
        .iter()
        .filter(|i| i.op == OP_TYPE_INT && i.w.get(1) == Some(&32))
        .map(|i| i.w[0])
        .collect();
    let float = s
        .insts
        .iter()
        .find(|i| i.op == OP_TYPE_FLOAT && i.w.len() == 2 && i.w[1] == 32)
        .map(|i| i.w[0]);
    let ty = value_types(s);
    let of = |v: u32, set: &BTreeSet<u32>| ty.get(&v).is_some_and(|t| set.contains(t));
    let old = std::mem::take(&mut s.insts);
    let mut d = Decls { add: Vec::new() };
    let mut out = Vec::with_capacity(old.len() + 16);
    for mut i in old.iter().cloned() {
        match i.op {
            OP_CAPABILITY if matches!(i.w.first(), Some(&(CAP_SAMPLED_1D | CAP_IMAGE_1D))) => {
                continue;
            }
            OP_TYPE_IMAGE if images.contains(&i.w[0]) => i.w[2] = DIM_2D,
            OP_IMAGE_TEXEL_POINTER if of(i.w[2], &pointers) => {
                return Err("a texel pointer into a 1D image".into());
            }
            OP_SAMPLE_PROJ_FIRST..=OP_SAMPLE_PROJ_LAST if of(i.w[2], &sampled) => {
                return Err("a projective sample of a 1D image".into());
            }
            OP_SAMPLE_IMPLICIT
            | OP_SAMPLE_EXPLICIT
            | OP_SAMPLE_DREF_IMPLICIT
            | OP_SAMPLE_DREF_EXPLICIT
            | OP_QUERY_LOD
                if i.w.len() >= 4 && of(i.w[2], &sampled) =>
            {
                let dref = matches!(i.op, OP_SAMPLE_DREF_IMPLICIT | OP_SAMPLE_DREF_EXPLICIT);
                let mask = i.w.get(if dref { 5 } else { 4 }).copied().unwrap_or(0);
                if i.op != OP_QUERY_LOD && mask & TWO_COMPONENT_OPERANDS != 0 {
                    return Err("a gradient or offset in a sample of a 1D image".into());
                }
                let f = float.ok_or("a 1D sample without a 32-bit float type")?;
                let v2 = d.vec2(s, &old, f);
                let half = d.constant(s, &old, f, 0.5f32.to_bits());
                let c = s.fresh();
                out.push(Inst::new(OP_COMPOSITE_CONSTRUCT, vec![v2, c, i.w[3], half]));
                i.w[3] = c;
            }
            OP_FETCH | OP_READ if i.w.len() >= 4 && of(i.w[2], &images) => {
                if i.w.get(4).is_some_and(|m| m & TWO_COMPONENT_OPERANDS != 0) {
                    return Err("an offset in a read of a 1D image".into());
                }
                i.w[3] = texel(s, &old, &mut d, &mut out, &ty, &ints, i.w[3])?;
            }
            OP_WRITE if i.w.len() >= 3 && of(i.w[0], &images) => {
                i.w[1] = texel(s, &old, &mut d, &mut out, &ty, &ints, i.w[1])?;
            }
            OP_QUERY_SIZE | OP_QUERY_SIZE_LOD if i.w.len() >= 3 && of(i.w[2], &images) => {
                // the 2D query gives (width, 1): the shader gets the width
                let scalar = i.w[0];
                let size = s.fresh();
                let id = i.w[1];
                i.w[0] = d.vec2(s, &old, scalar);
                i.w[1] = size;
                out.push(i);
                out.push(Inst::new(OP_COMPOSITE_EXTRACT, vec![scalar, id, size, 0]));
                continue;
            }
            _ => {}
        }
        out.push(i);
    }
    let at = out
        .iter()
        .position(|i| i.op == OP_FUNCTION)
        .unwrap_or(out.len());
    out.splice(at..at, d.add);
    s.insts = out;
    Ok(true)
}

/// The texel coordinate `(x, 0)` of the integer coordinate `x`, built in `out`.
fn texel(
    s: &mut Spv,
    old: &[Inst],
    d: &mut Decls,
    out: &mut Vec<Inst>,
    ty: &BTreeMap<u32, u32>,
    ints: &BTreeSet<u32>,
    x: u32,
) -> Result<u32, String> {
    let t = ty
        .get(&x)
        .copied()
        .filter(|t| ints.contains(t))
        .ok_or("a 1D image coordinate that is not a 32-bit integer")?;
    let v2 = d.vec2(s, old, t);
    let zero = d.constant(s, old, t, 0);
    let c = s.fresh();
    out.push(Inst::new(OP_COMPOSITE_CONSTRUCT, vec![v2, c, x, zero]));
    Ok(c)
}

#[cfg(test)]
#[path = "image1d_tests.rs"]
mod tests;
