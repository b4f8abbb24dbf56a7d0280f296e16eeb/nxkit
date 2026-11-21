//! Texture queries (size, samples, level of detail) and the dispatcher of the texture instructions.

use super::emit::{Em, R, Val};
use super::inst_tex::dimensions;
use crate::ir::Inst;
use crate::ir::agg::AggType;
use crate::structured::ast::OpData;
use crate::types::SamplerType;

impl Em<'_> {
    /// The texture instructions.
    pub(super) fn texture(&mut self, inst: Inst, o: &OpData) -> R<Option<Val>> {
        match inst {
            Inst::TextureSample => self.sample(o),
            Inst::Lod => self.lod(o).map(Some),
            Inst::TextureQuerySamples => {
                let t = o.tex.clone().ok_or("not a texture operation")?;
                let r = self.tex_ref(o, &mut 0, &t)?;
                Ok(Some(Val::new(
                    format!("int({}.get_num_samples())", r.tex),
                    AggType::S32,
                )))
            }
            _ => self.query_size(o).map(Some),
        }
    }

    fn lod(&mut self, o: &OpData) -> R<Val> {
        let t = o.tex.clone().ok_or("not a texture operation")?;
        let mut i = 0;
        let r = self.tex_ref(o, &mut i, &t)?;
        let smp = r.smp.ok_or("lod query without a sampler")?;
        let mut c = Vec::new();
        for _ in 0..dimensions(t.ty)? {
            c.push(self.tsrc(o, &mut i, AggType::FP32)?.s);
        }
        let coord = super::inst_tex::vec_of("float", &c);
        let f = if o.index == 0 {
            "calculate_clamped_lod"
        } else {
            "calculate_unclamped_lod"
        };
        Ok(Val::new(
            format!("{}.{f}({smp}, {coord})", r.tex),
            AggType::FP32,
        ))
    }

    fn query_size(&mut self, o: &OpData) -> R<Val> {
        let t = o.tex.clone().ok_or("not a texture operation")?;
        let mut i = 0;
        let r = self.tex_ref(o, &mut i, &t)?;
        if o.index == 3 {
            return Ok(Val::new(
                format!("int({}.get_num_mip_levels())", r.tex),
                AggType::S32,
            ));
        }
        let ty = t.ty;
        let buffer = ty.0 & SamplerType::MASK == 2;
        let has_lod = ty.0 & SamplerType::MULTISAMPLE == 0 && !buffer;
        let mut names: Vec<&str> = match ty.0 & SamplerType::MASK {
            1 | 2 => vec!["get_width"],
            3 | 5 => vec!["get_width", "get_height"],
            _ => vec!["get_width", "get_height", "get_depth"],
        };
        if ty.0 & SamplerType::ARRAY != 0 {
            names.push("get_array_size");
        }
        // A single size answers every component, as the SPIR-V back end has no vector to pick from. A component the
        // texture does not have (the depth of a 2D texture) is 1.
        let at = if names.len() == 1 {
            0
        } else {
            o.index as usize
        };
        let Some(&f) = names.get(at) else {
            return Ok(Val::new("1", AggType::S32));
        };
        if f == "get_array_size" && ty.0 & SamplerType::MASK == 4 {
            return Ok(Val::new("1", AggType::S32));
        }
        let arg = if has_lod && f != "get_array_size" {
            let l = self.tsrc(o, &mut i, AggType::S32)?;
            format!("uint({})", l.s)
        } else {
            String::new()
        };
        Ok(Val::new(format!("int({}.{f}({arg}))", r.tex), AggType::S32))
    }
}
