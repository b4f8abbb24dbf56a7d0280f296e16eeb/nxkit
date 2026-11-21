//! Texture sampling, gathers and fetches.

use super::args::SET_NAME;
use super::emit::{Em, R, Shader, Val};
use super::geom;
use super::pool;
use super::texoff::{Level, Off, shifted};
use super::types::scalar;
use super::usage::is_plain_sample;
use crate::ir::agg::AggType;
use crate::structured::ast::{AstTex, OpData};
use crate::types::SamplerType;

pub(super) const GATHER: u32 = 1 << 1;
pub(super) const DERIVATIVES: u32 = 1 << 2;
pub(super) const INT_COORDS: u32 = 1 << 3;
pub(super) const LOD_BIAS: u32 = 1 << 4;
pub(super) const LOD_LEVEL: u32 = 1 << 5;
pub(super) const OFFSET: u32 = 1 << 6;
pub(super) const OFFSETS: u32 = 1 << 7;

/// The number of coordinates of a sampler type without the array index.
pub(super) fn dimensions(t: SamplerType) -> R<i32> {
    Ok(match t.0 & SamplerType::MASK {
        1 | 2 => 1,
        3 => 2,
        4 | 5 => 3,
        _ => return Err(format!("invalid sampler type {}", t.0)),
    })
}

/// A vector constructor of `parts`, or the single part.
pub(super) fn vec_of(ty: &str, parts: &[String]) -> String {
    if parts.len() == 1 {
        parts[0].clone()
    } else {
        format!("{ty}{}({})", parts.len(), parts.join(", "))
    }
}

/// The components of a four vector that the mask selects, as a swizzle. Empty for all four.
pub(super) fn swizzle(mask: i32) -> String {
    if mask & 15 == 15 {
        return String::new();
    }
    let s: String = (0..4)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| ["x", "y", "z", "w"][i as usize])
        .collect();
    format!(".{s}")
}

/// The type of the swizzled result of an operation.
pub(super) fn result_type(mask: i32, comp: AggType) -> AggType {
    match (mask as u32 & 15).count_ones() {
        2 => comp | AggType::VECTOR2,
        3 => comp | AggType::VECTOR3,
        4 => comp | AggType::VECTOR4,
        _ => comp,
    }
}

/// The objects an operation names.
pub(super) struct Tref {
    pub tex: String,
    pub smp: Option<String>,
    pub border: Option<(String, u32)>,
    pub array: bool,
}

impl Em<'_> {
    /// The next source of a texture operation as a value of a type.
    pub(super) fn tsrc(&mut self, o: &OpData, i: &mut usize, ty: AggType) -> R<Val> {
        let a = self.src(o, *i)?;
        *i += 1;
        self.get(ty, a)
    }

    /// The next source as a constant.
    pub(super) fn tconst(&mut self, o: &OpData, i: &mut usize) -> R<i32> {
        let a = self.src(o, *i)?;
        *i += 1;
        self.constant(a)
            .ok_or_else(|| "unported: texture operand that is not constant".to_string())
    }

    /// The texture and sampler of an operation, reading the index sources the same way the front end wrote them.
    pub(super) fn tex_ref(&mut self, o: &OpData, i: &mut usize, t: &AstTex) -> R<Tref> {
        let tx = self
            .res
            .texs
            .get(&(t.set, t.binding))
            .ok_or("texture is not in the argument buffer")?;
        let used = tx.name.clone();
        let (name, smp_name, twin, array, pooled) = (
            tx.name.clone(),
            tx.sampler.clone(),
            tx.twin.clone(),
            tx.def.array_length != 1,
            tx.pool,
        );
        self.fetch_use(&used);
        let idx = if array {
            Some(self.tsrc(o, i, AggType::S32)?.s)
        } else {
            None
        };
        let at = |n: &str, idx: &Option<String>, pool: bool, mark: bool| match idx {
            Some(x) if pool => pool::element(n, x, mark),
            Some(x) => format!("{SET_NAME}.{n}[{x}]"),
            None => format!("{SET_NAME}.{n}"),
        };
        let tex = at(&name, &idx, pooled, true);
        if !t.is_separate() {
            return Ok(Tref {
                tex,
                smp: smp_name.map(|n| at(&n, &idx, pooled, false)),
                border: twin,
                array,
            });
        }
        let key = (t.sampler_set, t.sampler_binding);
        let sampler = self.res.texs.get(&key);
        let spool = sampler.is_some_and(|s| s.pool);
        let sarray = match sampler {
            Some(s) => s.def.array_length != 1,
            None => self
                .props
                .properties
                .textures
                .iter()
                .any(|d| (d.set, d.binding) == key && d.array_length != 1),
        };
        let sidx = if sarray {
            Some(self.tsrc(o, i, AggType::S32)?.s)
        } else {
            None
        };
        Ok(Tref {
            tex,
            smp: sampler.map(|s| at(&s.name, &sidx, spool, false)),
            border: sampler.and_then(|s| s.twin.clone()),
            array,
        })
    }

    /// Texture sample, gather and fetch.
    pub(super) fn sample(&mut self, o: &OpData) -> R<Option<Val>> {
        let t = o.tex.clone().ok_or("not a texture operation")?;
        let f = t.flags;
        let (gather, grad, int_coords) =
            (f & GATHER != 0, f & DERIVATIVES != 0, f & INT_COORDS != 0);
        let is_array = t.ty.0 & SamplerType::ARRAY != 0;
        let ms = t.ty.0 & SamplerType::MULTISAMPLE != 0;
        let shadow = t.ty.0 & SamplerType::SHADOW != 0;
        let mut i = 0;
        let r = self.tex_ref(o, &mut i, &t)?;
        let dims = dimensions(t.ty)?;
        let coord_ty = if int_coords {
            AggType::S32
        } else {
            AggType::FP32
        };
        let mut coords = Vec::new();
        let mut coord_nodes = Vec::new();
        for _ in 0..dims {
            coord_nodes.push(self.src(o, i)?);
            coords.push(self.tsrc(o, &mut i, coord_ty)?.s);
        }
        let array = if is_array {
            Some(self.tsrc(o, &mut i, AggType::S32)?.s)
        } else {
            None
        };
        let mut coord = if int_coords && coords.len() == 1 {
            // A buffer or 1D fetch: a plain `int` is ambiguous between the `ushort` and `uint` overloads of
            // `texture_buffer::read`, and the shader did not compile.
            format!("uint({})", coords[0])
        } else if int_coords {
            vec_of("uint", &coords)
        } else {
            self.finite_coords(&coords)?
        };
        let dref = if shadow {
            Some(self.tsrc(o, &mut i, AggType::FP32)?.s)
        } else {
            None
        };
        let mut dxy = Vec::new();
        if grad {
            for _ in 0..2 {
                let mut p = Vec::new();
                for _ in 0..dims {
                    p.push(self.tsrc(o, &mut i, AggType::FP32)?.s);
                }
                dxy.push(vec_of("float", &p));
            }
        }
        let (mut sample, mut level) = (None, None);
        if ms {
            sample = Some(self.tsrc(o, &mut i, AggType::S32)?.s);
        } else if f & LOD_LEVEL != 0 {
            level = Some(self.tsrc(o, &mut i, coord_ty)?.s);
        }
        let mut offsets = Vec::new();
        if f & OFFSET != 0 {
            offsets.push(self.offset_of(o, &mut i, dims)?);
        } else if f & OFFSETS != 0 {
            for _ in 0..4 {
                offsets.push(self.offset_of(o, &mut i, dims)?);
            }
        }
        let mut bias = None;
        if f & LOD_BIAS != 0 {
            bias = Some(self.tsrc(o, &mut i, AggType::FP32)?.s);
        }
        if !gather
            && !int_coords
            && !ms
            && level.is_none()
            && !grad
            && self.shader != Shader::Fragment
        {
            level = Some(bias.take().unwrap_or_else(|| "0.0f".into()));
        }
        let comp = if gather && !shadow {
            let c = self.tconst(o, &mut i)?;
            Some(
                ["x", "y", "z", "w"]
                    .get(c as usize)
                    .copied()
                    .ok_or("gather component")?,
            )
        } else {
            None
        };
        let layered = t.ty.0 & SamplerType::MASK != 4;
        if array.is_some() && !layered {
            self.notes
                .push("3D texture array is bound as a 3D texture, the layer is ignored".into());
        }
        let array_arg = array
            .filter(|_| layered)
            .map(|a| format!("uint(max({a}, 0))"));
        let mut opts = Vec::new();
        if let Some(b) = &bias {
            opts.push(format!("bias({b})"));
        }
        if let Some(l) = level.as_ref().filter(|_| !int_coords) {
            opts.push(format!("level({l})"));
        }
        let one_d = t.ty.0 & SamplerType::MASK == 1;
        let lod_given = level.as_deref().is_some_and(|l| l != "0.0f");
        if one_d && !int_coords && (bias.is_some() || grad || lod_given) {
            self.notes.push(
                "1D texture sample with a level, bias or gradient reads the default level (Metal has no such form)"
                    .into(),
            );
        }
        if one_d {
            opts.clear();
        } else if grad {
            let g = match dims {
                2 => "gradient2d",
                3 if t.ty.0 & SamplerType::MASK == 5 => "gradientcube",
                _ => "gradient3d",
            };
            opts.push(format!("{g}({}, {})", dxy[0], dxy[1]));
        }
        let implicit = self.shader == Shader::Fragment
            && !int_coords
            && !gather
            && !grad
            && level.is_none()
            && bias.is_none();
        let lod = match (&level, &r.smp) {
            (Some(l), _) if int_coords => Level::Int(l),
            (Some(l), _) if !gather => Level::Float(l),
            (None, Some(s)) if implicit => Level::Implicit(s),
            _ => Level::Base,
        };
        let call = |coord: &str, off: Option<&Off>| -> String {
            let mut off_arg = None;
            let mut shift = None;
            if let Some(x) = off.or_else(|| offsets.first()) {
                if x.constant && !int_coords {
                    off_arg = Some(x.vector());
                } else {
                    shift = Some(shifted(coord, x, &r.tex, int_coords, &lod));
                }
            }
            let coord = shift.as_deref().unwrap_or(coord);
            let mut a: Vec<String> = Vec::new();
            if let Some(s) = r.smp.as_ref().filter(|_| !int_coords) {
                a.push(s.clone());
            }
            a.push(coord.to_string());
            if let Some(x) = &array_arg {
                a.push(x.clone());
            }
            if int_coords {
                if let Some(read) = geom::topology_read(&r.tex, coord) {
                    return read;
                }
                if let Some(s) = &sample {
                    a.push(format!("uint({s})"));
                } else if let Some(l) = &level {
                    a.push(format!("uint({l})"));
                }
                return format!("{}.read({})", r.tex, a.join(", "));
            }
            if let Some(d) = &dref {
                a.push(d.clone());
            }
            if gather {
                let off = off_arg;
                if shadow {
                    a.extend(off);
                    return format!("{}.gather_compare({})", r.tex, a.join(", "));
                }
                let zero = vec_of("int", &vec!["0".to_string(); dims as usize]);
                a.push(off.unwrap_or(zero));
                a.push(format!("component::{}", comp.unwrap_or("x")));
                return format!("{}.gather({})", r.tex, a.join(", "));
            }
            a.extend(opts.iter().cloned());
            a.extend(off_arg);
            let name = if shadow { "sample_compare" } else { "sample" };
            format!("{}.{name}({})", r.tex, a.join(", "))
        };
        let color = gather || !shadow;
        let mut s = if gather && offsets.len() == 4 {
            coord = self.once(Val::new(coord, float_ty(coords.len())))?.s;
            let parts: Vec<String> = offsets
                .iter()
                .map(|x| format!("{}.w", call(&coord, Some(x))))
                .collect();
            format!("float4({})", parts.join(", "))
        } else {
            call(&coord, None)
        };
        let plain = !int_coords && !gather && !shadow && is_plain_sample(f, t.ty) && !r.array;
        if let (true, Some((tw, bit)), Some(_)) = (plain, &r.border, &r.smp) {
            let args = call_args(&call(&coord, None));
            s = format!(
                "dm_bs({s}, {SET_NAME}.dm_border, {bit}u, {}, {SET_NAME}.{tw}, {args})",
                r.tex
            );
            self.helpers.insert("border");
        }
        if color && self.fetch.is_some() {
            s = self.fetch_rewrite(&t, &r, &coord_nodes, s);
        }
        if color {
            let ty = result_type(o.index, AggType::FP32);
            Ok(Some(Val::new(format!("{s}{}", swizzle(o.index)), ty)))
        } else {
            Ok(Some(Val::new(s, AggType::FP32)))
        }
    }

    /// Coordinates with NaNs replaced by zero.
    fn finite_coords(&mut self, coords: &[String]) -> R<String> {
        let c = vec_of("float", coords);
        let v = self.once(Val::new(c, float_ty(coords.len())))?;
        let zero = if coords.len() == 1 {
            "0.0f".to_string()
        } else {
            format!("float{}(0.0f)", coords.len())
        };
        Ok(format!("select({0}, {zero}, isnan({0}))", v.s))
    }
}

/// The type of `n` float coordinates.
fn float_ty(n: usize) -> AggType {
    if n == 1 {
        AggType::FP32
    } else {
        scalar(AggType::FP32) | vec_flag(n)
    }
}

fn vec_flag(n: usize) -> AggType {
    match n {
        2 => AggType::VECTOR2,
        3 => AggType::VECTOR3,
        _ => AggType::VECTOR4,
    }
}

/// The arguments of a call without the sampler, which the border helper takes separately.
fn call_args(call: &str) -> String {
    let open = call.find('(').map_or(0, |p| p + 1);
    let inner = &call[open..call.len().saturating_sub(1)];
    let mut depth = 0;
    for (k, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => return inner[k + 1..].trim_start().to_string(),
            _ => {}
        }
    }
    String::new()
}
