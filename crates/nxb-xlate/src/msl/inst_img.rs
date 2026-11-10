//! Image loads, stores and atomics.

use super::args::SET_NAME;
use super::emit::{Em, R, Val};
use super::inst_tex::{dimensions, result_type, swizzle, vec_of};
use super::types::{literal, msl_type};
use crate::ir::Inst;
use crate::structured::ast::{AstTex, OpData};
use crate::structured::types::{AggType, format_component_type};
use crate::types::SamplerType;

const ATOMIC_MASK: u32 = 15 << 16;
const BOUNDS_MASK: u32 = 3 << 20;
const PREDICATE_RESULT: u32 = 1 << 24;
const FORMAT_R32_UINT: usize = 11;
const FORMAT_R32_SINT: usize = 12;

impl Em<'_> {
    /// The image object of an operation, reading its index source.
    fn image_ref(&mut self, o: &OpData, i: &mut usize, t: &AstTex) -> R<String> {
        let tx = self
            .res
            .imgs
            .get(&(t.set, t.binding))
            .ok_or("image is not in the argument buffer")?;
        let name = tx.name.clone();
        if tx.def.array_length != 1 {
            let idx = self.tsrc(o, i, AggType::S32)?;
            Ok(format!("{SET_NAME}.{name}[{}]", idx.s))
        } else {
            Ok(format!("{SET_NAME}.{name}"))
        }
    }

    /// The coordinate arguments of an image access: the coordinate and the array layer.
    fn image_coords(&mut self, o: &OpData, i: &mut usize, t: &AstTex) -> R<Vec<String>> {
        let dims = dimensions(t.ty)?;
        let mut c = Vec::new();
        for _ in 0..dims {
            c.push(self.tsrc(o, i, AggType::S32)?.s);
        }
        let mask = t.ty.0 & SamplerType::MASK;
        // A cube image takes the texel and then the face. The face is the third coordinate.
        let mut out = match (mask, c.pop()) {
            (5, Some(face)) => vec![vec_of("uint", &c), format!("uint({face})")],
            (_, Some(last)) => {
                c.push(last);
                vec![vec_of("uint", &c)]
            }
            _ => return Err("image without coordinates".into()),
        };
        if t.ty.0 & SamplerType::ARRAY != 0 {
            let layer = self.tsrc(o, i, AggType::S32)?.s;
            if mask == 4 {
                self.notes
                    .push("3D image array is bound as a 3D image, the layer is ignored".into());
            } else {
                out.push(format!("uint({layer})"));
            }
        }
        Ok(out)
    }

    /// Image instructions.
    pub(super) fn image(&mut self, inst: Inst, o: &OpData) -> R<Option<Val>> {
        let t = o.tex.clone().ok_or("not a texture operation")?;
        let comp = format_component_type(t.format);
        let mut i = 0;
        match inst {
            Inst::ImageLoad => {
                let img = self.image_ref(o, &mut i, &t)?;
                let c = self.image_coords(o, &mut i, &t)?;
                let s = format!("{img}.read({})", c.join(", "));
                let ty = result_type(o.index, comp);
                Ok(Some(Val::new(format!("{s}{}", swizzle(o.index)), ty)))
            }
            Inst::ImageStore => {
                let img = self.image_ref(o, &mut i, &t)?;
                let c = self.image_coords(o, &mut i, &t)?;
                let mut texel = Vec::new();
                for _ in 0..4 {
                    if i < o.sources.len() {
                        texel.push(self.tsrc(o, &mut i, comp)?.s);
                    } else {
                        texel.push(literal(comp, 0));
                    }
                }
                let ty = msl_type(comp | AggType::VECTOR4)?;
                self.line(&format!(
                    "{img}.write({ty}({}), {});",
                    texel.join(", "),
                    c.join(", ")
                ));
                Ok(None)
            }
            _ => self.image_atomic(o, &t).map(Some),
        }
    }

    fn image_atomic(&mut self, o: &OpData, t: &AstTex) -> R<Val> {
        if !self.props.host.metal_image_atomics {
            return Err("unported: image atomics without Metal image atomics".into());
        }
        if t.flags & (BOUNDS_MASK | PREDICATE_RESULT) != 0 {
            return Err("unported: image atomic with trap semantics".into());
        }
        let ty = t.ty;
        let ok_type = ty == SamplerType::TEXTURE_BUFFER
            || ty == SamplerType::TEXTURE_2D
            || ty.0 == (SamplerType::TEXTURE_2D.0 | SamplerType::ARRAY)
            || ty == SamplerType::TEXTURE_3D;
        let array = self
            .res
            .imgs
            .get(&(t.set, t.binding))
            .is_some_and(|x| x.def.array_length != 1);
        if array || !ok_type || (t.format != FORMAT_R32_UINT && t.format != FORMAT_R32_SINT) {
            return Err("unported: image atomic on this image".into());
        }
        let comp = format_component_type(t.format);
        let cty = msl_type(comp)?;
        let mut i = 0;
        let img = self.image_ref(o, &mut i, t)?;
        let c = self.image_coords(o, &mut i, t)?;
        let value = self.tsrc(o, &mut i, comp)?.s;
        let f = match t.flags & ATOMIC_MASK {
            x if x == 1 << 16 => "min",
            x if x == 2 << 16 => "max",
            x if x == 3 << 16 || x == 0 => "add",
            x if x == 4 << 16 => "sub",
            x if x == 5 << 16 => "and",
            x if x == 6 << 16 => "or",
            x if x == 7 << 16 => "xor",
            x if x == 8 << 16 => "exchange",
            _ => "compare",
        };
        let v4 = format!("{cty}4");
        let n = self.tmp;
        self.tmp += 1;
        let coord = format!("t{n}c");
        let layer = format!("t{n}l");
        let cty_coord = match dimensions(t.ty)? {
            1 => "uint".to_string(),
            d => format!("uint{d}"),
        };
        let init = if cty_coord == "uint" {
            format!("uint({})", c[0])
        } else {
            c[0].clone()
        };
        self.pre.push(format!("{cty_coord} {coord} = {init};"));
        let mut args = vec![coord.clone()];
        if let Some(l) = c.get(1) {
            self.pre.push(format!("uint {layer} = {l};"));
            args.push(layer.clone());
        }
        let args = args.join(", ");
        let inside = Self::inside(&img, t.ty, &coord, c.get(1).map(|_| layer.as_str()))?;
        let body = if f == "compare" {
            let cmp = self.tsrc(o, &mut i, comp)?.s;
            // a weak exchange may fail with the expected value still in the image: try again
            self.pre
                .push(format!("{v4} t{n}x = {v4}({cmp}); {v4} t{n}c = t{n}x;"));
            format!(
                "while (!{img}.atomic_compare_exchange_weak({args}, &t{n}x, {v4}({value})) && all(t{n}x == t{n}c)) {{}} t{n} = t{n}x.x;"
            )
        } else {
            let one = matches!(t.flags & ATOMIC_MASK, x if x == 3 << 16 || x == 4 << 16);
            let arg = if one { literal(comp, 1) } else { value };
            format!("t{n} = {img}.atomic_fetch_{f}({args}, {v4}({arg})).x;")
        };
        self.pre
            .push(format!("{} t{n} = {};", cty, literal(comp, 0)));
        self.pre.push(format!("if ({inside}) {{ {body} }}"));
        Ok(Val::new(format!("t{n}"), comp))
    }

    /// The bounds test of an atomic: a coordinate outside the image leaves memory alone and reads as zero.
    fn inside(img: &str, ty: SamplerType, coord: &str, layer: Option<&str>) -> R<String> {
        let mask = ty.0 & SamplerType::MASK;
        let extent = match mask {
            1 | 2 => format!("{img}.get_width()"),
            4 => format!("uint3({img}.get_width(), {img}.get_height(), {img}.get_depth())"),
            _ => format!("uint2({img}.get_width(), {img}.get_height())"),
        };
        let mut s = if matches!(mask, 1 | 2) {
            format!("{coord} < {extent}")
        } else {
            format!("all({coord} < {extent})")
        };
        if let Some(l) = layer {
            s.push_str(&format!(" && {l} < {img}.get_array_size()"));
        }
        Ok(s)
    }
}
