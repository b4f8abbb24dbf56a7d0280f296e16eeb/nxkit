//! Attribute arrays that a register indexes, the viewport mask and the draw index.
//!
//! A shader that indexes its attributes with a register (`LDA`, `IPA` and `AST` with a register address) reads one
//! array of sixteen `float4`. MSL keeps each location as its own member, so every location has its own interpolation
//! and its own attribute slot. A register index then picks the member with a chain of comparisons.

use super::emit::{Em, R, Shader};
use super::inst_mem::{Place, Space, swizzle_or_index};
use super::io::{Plan, varying};
use super::types::scalar;
use crate::ir::IoVariable;
use crate::ir::agg::AggType;
use crate::props::Definitions;
use crate::structured::ast::OpData;

/// The length of the attribute array the shader sees.
pub(super) const ATTRIBUTES: i32 = 16;

/// The members of `main0_in` for an indexed input.
///
/// A pixel shader gets the locations its interpolation map uses. A vertex shader gets the float attributes.
pub(super) fn input_members(p: &mut Plan, d: &Definitions, shader: Shader) -> R<()> {
    for l in 0..ATTRIBUTES {
        let used = match shader {
            Shader::Fragment => d.imap_types.get(l as usize).is_some_and(|m| *m != [0; 4]),
            _ => d
                .attribute_types
                .get(l as usize)
                .is_some_and(|t| t & !0xc0 == 0),
        };
        if !used {
            continue;
        }
        if shader == Shader::Fragment {
            varying(p, &d.imap_types[l as usize], l);
        } else {
            p.in_members
                .push(format!("float4 loc{l} [[attribute({l})]];"));
            p.attributes.push((l as u32, "float4".into()));
        }
        p.ia.push(l);
    }
    if shader != Shader::Fragment && p.ia.len() < d.attribute_types.len().min(ATTRIBUTES as usize) {
        p.notes.push(
            "indexed vertex attributes read only the float attributes, the others read zero".into(),
        );
    }
    Ok(())
}

/// The user outputs of a vertex shader that writes its outputs through a register index.
pub(super) fn output_members(p: &mut Plan, users: &mut Vec<i32>) {
    users.extend(0..ATTRIBUTES);
    p.oa = (0..ATTRIBUTES).collect();
}

/// A vertex shader writes a viewport mask. Metal has a viewport index and no mask.
pub(super) fn viewport_mask(p: &mut Plan) {
    p.viewport_mask = true;
}

/// The members that depend on what the whole program touches.
pub(super) fn finish(p: &mut Plan) {
    if p.viewport_mask {
        if !p.viewport {
            p.viewport = true;
            p.out_members
                .push("uint gl_ViewportIndex [[viewport_array_index]];".into());
        }
        p.notes.push(
            "the viewport mask becomes the viewport index of its lowest set bit, Metal has no mask"
                .into(),
        );
    }
}

/// A vertex shader reads a draw index. Metal has none, so every draw is draw zero.
pub(super) fn draw_index(p: &mut Plan) {
    if !p.notes.iter().any(|n| n.starts_with("draw index")) {
        p.notes
            .push("draw index is zero, Metal has no draw index".into());
    }
}

impl Plan {
    /// The member of the location in `idx`, a chain over the declared locations.
    ///
    /// A load of a location that is not declared reads zero. A store goes to the last member then.
    fn indexed(&self, out: bool, idx: &str, store: bool) -> String {
        let (locs, base) = if out {
            (&self.oa, "out")
        } else {
            (&self.ia, "in")
        };
        let Some((last, rest)) = locs.split_last() else {
            return "float4(0.0f)".into();
        };
        let mut s = String::new();
        for l in rest {
            s.push_str(&format!("{idx} == {l}u ? {base}.loc{l} : "));
        }
        if store || rest.is_empty() {
            s.push_str(&format!("{base}.loc{last}"));
        } else {
            s.push_str(&format!(
                "{idx} == {last}u ? {base}.loc{last} : float4(0.0f)"
            ));
        }
        format!("({s})")
    }
}

impl Em<'_> {
    /// The place of an access to an indexed attribute: the vector index, then the element.
    pub(super) fn indexed_place(&mut self, o: &OpData, out: bool, at: usize) -> R<Place> {
        let a = self.src(o, at)?;
        let (base, skip) = match self.constant(a) {
            Some(c) => {
                let locs = if out { &self.plan.oa } else { &self.plan.ia };
                if locs.contains(&c) {
                    (format!("{}.loc{c}", if out { "out" } else { "in" }), false)
                } else {
                    ("float4(0.0f)".to_string(), true)
                }
            }
            None => {
                let i = self.get(AggType::U32, a)?;
                let i = self.once(i)?;
                (self.plan.indexed(out, &i.s, out), false)
            }
        };
        let mut s = base;
        if o.sources.len() > at + 1 {
            let e = self.src(o, at + 1)?;
            let c = self.constant(e);
            let i = self.get(AggType::S32, e)?.s;
            s = swizzle_or_index(s, true, &i, c);
        }
        let ty = AggType::FP32;
        Ok(Place {
            s,
            ty: scalar(ty),
            mem: ty,
            space: Space::Thread,
            skip,
            mask: false,
        })
    }

    /// The place of the viewport mask, which is the viewport index.
    pub(super) fn viewport_mask_place(&self) -> Place {
        Place {
            s: "out.gl_ViewportIndex".into(),
            ty: AggType::S32,
            mem: AggType::U32,
            space: Space::Thread,
            skip: false,
            mask: true,
        }
    }

    /// The viewport index a mask names.
    pub(super) fn mask_index(v: &str) -> String {
        format!("min(ctz(uint({v})), 15u)")
    }
}

/// Whether the variable is the viewport mask.
pub(super) fn is_mask(v: IoVariable) -> bool {
    v == IoVariable::ViewportMask
}
