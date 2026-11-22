//! SPIR-V at the word level: the parse into instructions and the fix-ups that run before naga reads a module.
//!
//! The fix-ups are `fix_strides` (an `ArrayStride` on a struct member) and `split_combined` (naga reads no combined
//! image-sampler variables, so each one becomes an image variable and a sampler variable with the same set and binding).
//! `crate::image1d` has the third one, 1D images as 2D.

use std::collections::{BTreeMap, BTreeSet};

/// The magic number in the first word of a SPIR-V module.
pub(crate) const MAGIC: u32 = 0x0723_0203;

pub(crate) const OP_CAPABILITY: u16 = 17;
pub(crate) const OP_ENTRY_POINT: u16 = 15;
pub(crate) const OP_TYPE_INT: u16 = 21;
pub(crate) const OP_TYPE_FLOAT: u16 = 22;
pub(crate) const OP_TYPE_VECTOR: u16 = 23;
pub(crate) const OP_TYPE_IMAGE: u16 = 25;
pub(crate) const OP_TYPE_SAMPLER: u16 = 26;
pub(crate) const OP_TYPE_SAMPLED_IMAGE: u16 = 27;
pub(crate) const OP_TYPE_ARRAY: u16 = 28;
pub(crate) const OP_TYPE_RUNTIME_ARRAY: u16 = 29;
pub(crate) const OP_TYPE_STRUCT: u16 = 30;
pub(crate) const OP_TYPE_POINTER: u16 = 32;
pub(crate) const OP_TYPE_FUNCTION: u16 = 33;
pub(crate) const OP_CONSTANT: u16 = 43;
pub(crate) const OP_FUNCTION: u16 = 54;
pub(crate) const OP_FUNCTION_PARAMETER: u16 = 55;
pub(crate) const OP_FUNCTION_CALL: u16 = 57;
pub(crate) const OP_VARIABLE: u16 = 59;
pub(crate) const OP_LOAD: u16 = 61;
pub(crate) const OP_DECORATE: u16 = 71;
pub(crate) const OP_MEMBER_DECORATE: u16 = 72;
pub(crate) const OP_COMPOSITE_CONSTRUCT: u16 = 80;
pub(crate) const OP_COMPOSITE_EXTRACT: u16 = 81;
pub(crate) const OP_SAMPLED_IMAGE: u16 = 86;

pub(crate) const DEC_ARRAY_STRIDE: u32 = 6;
pub(crate) const DEC_BINDING: u32 = 33;
pub(crate) const DEC_SET: u32 = 34;

pub(crate) const SC_UNIFORM_CONSTANT: u32 = 0;

/// Set in the binding of the sampler that `split_combined` adds. The image and the sampler then have bindings of their
/// own, so naga's MSL binding map can give the one a texture slot and the other a sampler slot.
pub(crate) const TWIN_BINDING: u32 = 1 << 31;

/// One instruction: the opcode and the words after the first one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Inst {
    pub op: u16,
    pub w: Vec<u32>,
}

impl Inst {
    pub(crate) fn new(op: u16, w: Vec<u32>) -> Inst {
        Inst { op, w }
    }
}

/// A module as a header and a list of instructions.
#[derive(Clone, Debug)]
pub(crate) struct Spv {
    pub header: [u32; 5],
    pub insts: Vec<Inst>,
}

/// Whether `bytes` start with the SPIR-V magic number, little-endian.
pub fn is_spirv(bytes: &[u8]) -> bool {
    bytes.get(..4) == Some(&MAGIC.to_le_bytes()[..])
}

impl Spv {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Spv, String> {
        if !is_spirv(bytes) || bytes.len() < 20 || !bytes.len().is_multiple_of(4) {
            return Err("not a SPIR-V module".into());
        }
        let words: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        let mut header = [0u32; 5];
        header.copy_from_slice(&words[..5]);
        let mut insts = Vec::new();
        let mut i = 5;
        while i < words.len() {
            let n = (words[i] >> 16) as usize;
            if n == 0 || i + n > words.len() {
                return Err(format!("a truncated instruction at word {i}"));
            }
            insts.push(Inst::new(
                (words[i] & 0xffff) as u16,
                words[i + 1..i + n].to_vec(),
            ));
            i += n;
        }
        Ok(Spv { header, insts })
    }

    pub(crate) fn words(&self) -> Vec<u32> {
        let mut out = self.header.to_vec();
        for i in &self.insts {
            out.push(((i.w.len() as u32 + 1) << 16) | u32::from(i.op));
            out.extend_from_slice(&i.w);
        }
        out
    }

    pub(crate) fn fresh(&mut self) -> u32 {
        let id = self.header[3];
        self.header[3] += 1;
        id
    }

    /// The execution model of the single entry point, or an error when the module has none or several.
    pub(crate) fn execution_model(&self) -> Result<u32, String> {
        let eps: Vec<&Inst> = self
            .insts
            .iter()
            .filter(|i| i.op == OP_ENTRY_POINT)
            .collect();
        match eps[..] {
            [ep] => {
                ep.w.first()
                    .copied()
                    .ok_or_else(|| "an empty OpEntryPoint".into())
            }
            _ => Err(format!("{} entry points (one expected)", eps.len())),
        }
    }

    pub(crate) fn def(&self, id: u32) -> Option<&Inst> {
        self.insts.iter().find(|i| result_id(i) == Some(id))
    }

    fn decorations(&self, id: u32) -> impl Iterator<Item = &Inst> {
        self.insts
            .iter()
            .filter(move |i| i.op == OP_DECORATE && i.w.first() == Some(&id))
    }

    pub(crate) fn decoration(&self, id: u32, dec: u32) -> Option<u32> {
        self.decorations(id)
            .find(|i| i.w.get(1) == Some(&dec))
            .map(|i| i.w.get(2).copied().unwrap_or(0))
    }

    /// The position of the first function, where new types and constants can go.
    pub(crate) fn first_function(&self) -> usize {
        self.insts
            .iter()
            .position(|i| i.op == OP_FUNCTION)
            .unwrap_or(self.insts.len())
    }
}

/// The result id of an instruction, for the opcodes this crate reads.
fn result_id(i: &Inst) -> Option<u32> {
    match i.op {
        19..=38 => i.w.first().copied(),
        41..=46 => i.w.get(1).copied(),
        OP_FUNCTION | OP_FUNCTION_PARAMETER | OP_FUNCTION_CALL | OP_VARIABLE | OP_LOAD => {
            i.w.get(1).copied()
        }
        _ => None,
    }
}

/// Copies an `ArrayStride` decoration that sits on a struct member onto the member's array type when that type has no stride
/// of its own. The new decorations go right after the last member decoration that carries a stride.
pub(crate) fn fix_strides(s: &mut Spv) {
    let mut add = Vec::new();
    let mut last = None;
    let mut done = BTreeSet::new();
    for (k, i) in s.insts.iter().enumerate() {
        if i.op != OP_MEMBER_DECORATE || i.w.get(2) != Some(&DEC_ARRAY_STRIDE) || i.w.len() < 4 {
            continue;
        }
        last = Some(k);
        let Some(st) = s.def(i.w[0]).filter(|d| d.op == OP_TYPE_STRUCT) else {
            continue;
        };
        let Some(&ty) = st.w.get(1 + i.w[1] as usize) else {
            continue;
        };
        let is_array = s
            .def(ty)
            .is_some_and(|d| d.op == OP_TYPE_ARRAY || d.op == OP_TYPE_RUNTIME_ARRAY);
        if is_array && s.decoration(ty, DEC_ARRAY_STRIDE).is_none() && done.insert(ty) {
            add.push(Inst::new(OP_DECORATE, vec![ty, DEC_ARRAY_STRIDE, i.w[3]]));
        }
    }
    if let Some(k) = last {
        s.insts.splice(k + 1..k + 1, add);
    }
}

/// The variable and parameter ids of former combined image samplers, each with its new sampler twin.
pub(crate) type Twins = BTreeMap<u32, u32>;

/// The id of the type `op` with the operands `tail` after its result id. A missing type is added at `*at`, which then
/// moves past it.
pub(crate) fn find_or_add(s: &mut Spv, at: &mut usize, op: u16, tail: &[u32]) -> u32 {
    if let Some(i) = s
        .insts
        .iter()
        .find(|i| i.op == op && i.w.get(1..) == Some(tail))
    {
        return i.w[0];
    }
    let id = s.fresh();
    let mut w = vec![id];
    w.extend_from_slice(tail);
    s.insts.insert(*at, Inst::new(op, w));
    *at += 1;
    id
}

/// Splits every combined image-sampler variable and function parameter into an image and a sampler. The sampler variable gets
/// the same set and the binding with [`TWIN_BINDING`] set. Each load of a combined value becomes two loads and an `OpSampledImage`. Arrays of combined image
/// samplers are an error.
pub(crate) fn split_combined(s: &mut Spv) -> Result<Twins, String> {
    let mut twins = Twins::new();
    let sampled: BTreeMap<u32, u32> = s
        .insts
        .iter()
        .filter(|i| i.op == OP_TYPE_SAMPLED_IMAGE && i.w.len() >= 2)
        .map(|i| (i.w[0], i.w[1]))
        .collect();
    if sampled.is_empty() {
        return Ok(twins);
    }
    for i in &s.insts {
        if i.op == OP_TYPE_ARRAY && i.w.len() >= 2 && sampled.contains_key(&i.w[1]) {
            return Err("an array of combined image samplers".into());
        }
    }
    // pointer to sampled image → (pointer to image, pointer to sampler)
    let mut ptrs: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    let ptr_list: Vec<(u32, u32)> = s
        .insts
        .iter()
        .filter(|i| i.op == OP_TYPE_POINTER && i.w.len() >= 3 && sampled.contains_key(&i.w[2]))
        .map(|i| (i.w[0], i.w[2]))
        .collect();
    for (ptr, si) in ptr_list {
        let mut at = s
            .insts
            .iter()
            .position(|i| i.op == OP_TYPE_POINTER && i.w[0] == ptr)
            .map_or(0, |k| k + 1);
        let img = sampled[&si];
        let sampler = find_or_add(s, &mut at, OP_TYPE_SAMPLER, &[]);
        let p_img = find_or_add(s, &mut at, OP_TYPE_POINTER, &[SC_UNIFORM_CONSTANT, img]);
        let p_smp = find_or_add(s, &mut at, OP_TYPE_POINTER, &[SC_UNIFORM_CONSTANT, sampler]);
        ptrs.insert(ptr, (p_img, p_smp));
    }
    let mut out = Vec::with_capacity(s.insts.len());
    let insts = std::mem::take(&mut s.insts);
    let mut fn_types: BTreeMap<u32, u32> = BTreeMap::new();
    let mut new_types = Vec::new();
    // the function types first, as their new ids are needed by OpFunction
    for i in &insts {
        if i.op == OP_TYPE_FUNCTION && i.w[2..].iter().any(|p| ptrs.contains_key(p)) {
            let id = s.fresh();
            let mut w = vec![id, i.w[1]];
            for p in &i.w[2..] {
                match ptrs.get(p) {
                    Some(&(a, b)) => w.extend([a, b]),
                    None => w.push(*p),
                }
            }
            fn_types.insert(i.w[0], id);
            new_types.push((i.w[0], Inst::new(OP_TYPE_FUNCTION, w)));
        }
    }
    let mut extra_decs = Vec::new();
    for i in insts {
        let ptr_twin = |ty: u32| ptrs.get(&ty).copied();
        match i.op {
            OP_TYPE_FUNCTION => {
                let id = i.w[0];
                out.push(i);
                if let Some((_, t)) = new_types.iter().find(|(old, _)| *old == id) {
                    out.push(t.clone());
                }
            }
            OP_VARIABLE | OP_FUNCTION_PARAMETER if ptr_twin(i.w[0]).is_some() => {
                let Some((p_img, p_smp)) = ptr_twin(i.w[0]) else {
                    continue;
                };
                let twin = s.fresh();
                twins.insert(i.w[1], twin);
                let mut v = i.clone();
                v.w[0] = p_img;
                let mut t = i;
                t.w[0] = p_smp;
                t.w[1] = twin;
                out.push(v);
                out.push(t);
            }
            OP_FUNCTION if fn_types.contains_key(&i.w[3]) => {
                let mut f = i;
                f.w[3] = fn_types[&f.w[3]];
                out.push(f);
            }
            OP_LOAD if i.w.len() >= 3 && twins.contains_key(&i.w[2]) => {
                let (img, smp) = (s.fresh(), s.fresh());
                let si = i.w[0];
                let image_ty = sampled
                    .get(&si)
                    .copied()
                    .ok_or("a load of a combined image sampler without its type")?;
                let sampler_ty = sampler_of(&out).ok_or("no sampler type")?;
                out.push(Inst::new(OP_LOAD, vec![image_ty, img, i.w[2]]));
                out.push(Inst::new(OP_LOAD, vec![sampler_ty, smp, twins[&i.w[2]]]));
                out.push(Inst::new(OP_SAMPLED_IMAGE, vec![si, i.w[1], img, smp]));
            }
            OP_FUNCTION_CALL => {
                let mut w = i.w[..3].to_vec();
                for a in &i.w[3..] {
                    w.push(*a);
                    if let Some(&t) = twins.get(a) {
                        w.push(t);
                    }
                }
                out.push(Inst::new(OP_FUNCTION_CALL, w));
            }
            _ => out.push(i),
        }
    }
    // the twins get the set and the marked binding of their image, and the entry point lists them where it lists the
    // image
    for i in &out {
        if i.op == OP_DECORATE
            && (i.w.get(1) == Some(&DEC_BINDING) || i.w.get(1) == Some(&DEC_SET))
            && let Some(&t) = twins.get(&i.w[0])
        {
            let v = if i.w[1] == DEC_BINDING {
                i.w[2] | TWIN_BINDING
            } else {
                i.w[2]
            };
            extra_decs.push(Inst::new(OP_DECORATE, vec![t, i.w[1], v]));
        }
    }
    for i in &mut out {
        if i.op == OP_ENTRY_POINT {
            let name_end = 2 + i.w[2..]
                .iter()
                .position(|w| w.to_le_bytes().contains(&0))
                .map_or(0, |p| p + 1);
            let mut w = i.w[..name_end].to_vec();
            for v in &i.w[name_end..] {
                w.push(*v);
                if let Some(&t) = twins.get(v) {
                    w.push(t);
                }
            }
            i.w = w;
        }
    }
    if let Some(k) = out
        .iter()
        .rposition(|i| i.op == OP_DECORATE || i.op == OP_MEMBER_DECORATE)
    {
        out.splice(k + 1..k + 1, extra_decs);
    }
    s.insts = out;
    Ok(twins)
}

fn sampler_of(insts: &[Inst]) -> Option<u32> {
    insts
        .iter()
        .find(|i| i.op == OP_TYPE_SAMPLER)
        .map(|i| i.w[0])
}

#[cfg(test)]
#[path = "spirv_tests.rs"]
mod tests;
