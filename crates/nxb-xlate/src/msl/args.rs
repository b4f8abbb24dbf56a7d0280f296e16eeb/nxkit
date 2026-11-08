//! The argument buffer: its struct, the binding of every member and the lookups the instructions use.
//!
//! Each used buffer, texture, sampler and image is one member with its own `[[id(n)]]`. The member names are the
//! names of the front end (`fp_c3`, `fp_s0`, `fp_t_cb4_1C`, ...). The kind of a member and its facts come from the
//! descriptors of the front end (see `handles`).

use super::abi::{self, Kind};
use super::buffers;
use super::emit::R;
use super::geom;
use super::handles::{Decoded, decode};
use super::pool;
use super::refl::Binding;
use super::usage::Usage;
use crate::driver::{BufferInfo, Program, TexInfo};
use crate::props::{BufferDef, Props, TextureDef};
use crate::structured::types::format_component_type;
use crate::types::SamplerType;
use std::collections::{BTreeMap, BTreeSet};

/// The name of the argument buffer struct and its parameter.
pub const SET_TYPE: &str = "spvDescriptorSetBuffer0";
/// The parameter name of the argument buffer.
pub const SET_NAME: &str = "spvDescriptorSet0";

/// A buffer member.
#[derive(Clone, Debug)]
pub struct Buf {
    /// The member name.
    pub name: String,
    /// The definition of the front end.
    pub def: BufferDef,
    /// The shader writes it (storage buffers).
    pub write: bool,
}

/// A texture, sampler or image member.
#[derive(Clone, Debug)]
pub struct Tx {
    /// The member name.
    pub name: String,
    /// The definition of the front end.
    pub def: TextureDef,
    /// The member name of the sampler of a combined texture.
    pub sampler: Option<String>,
    /// The twin sampler for border colours and its bit, on the sampler of a plain sample.
    pub twin: Option<(String, u32)>,
    /// The member is a pointer to a pool table (see `pool`).
    pub pool: bool,
}

/// The resources of a program as the argument buffer holds them.
#[derive(Debug, Default)]
pub struct Res {
    /// Uniform buffers by binding.
    pub cbs: BTreeMap<i32, Buf>,
    /// Storage buffers by binding.
    pub sbs: BTreeMap<i32, Buf>,
    /// Textures and separate samplers by `(set, binding)`.
    pub texs: BTreeMap<(i32, i32), Tx>,
    /// Images by `(set, binding)`.
    pub imgs: BTreeMap<(i32, i32), Tx>,
    /// The bindings in member order.
    pub bindings: Vec<Binding>,
    /// The block structs and the argument buffer struct.
    pub text: String,
    /// The program has border emulation.
    pub border: bool,
    /// The argument buffer has members.
    pub has_args: bool,
    /// A fragment shader tests depth early because its pool marks only (see `pool::early`).
    pub early: bool,
}

/// The number in `<lowercase>_<letter><digits>`.
pub fn bank_name(nm: &str, letter: char) -> Option<i64> {
    let low = nm.bytes().take_while(u8::is_ascii_lowercase).count();
    let r = nm[low..].strip_prefix('_')?.strip_prefix(letter)?;
    (low > 0 && !r.is_empty() && r.bytes().all(|c| c.is_ascii_digit())).then(|| r.parse().ok())?
}

/// The MSL type of a texture. `access` is `None` for a sampled texture.
pub fn texture_type(ty: SamplerType, comp: &str, access: Option<&str>) -> R<String> {
    let dim = ty.0 & SamplerType::MASK;
    let (shadow, arr, ms) = (
        ty.0 & SamplerType::SHADOW != 0,
        ty.0 & SamplerType::ARRAY != 0,
        ty.0 & SamplerType::MULTISAMPLE != 0,
    );
    let acc = access.map(|a| format!(", access::{a}")).unwrap_or_default();
    if dim == 2 {
        return Ok(format!("texture_buffer<{comp}{acc}>"));
    }
    let mut n = match (shadow, dim) {
        (false, 1) => "texture1d",
        (false, 3) => "texture2d",
        (false, 4) => "texture3d",
        (false, 5) => "texturecube",
        (true, 3) => "depth2d",
        (true, 5) => "depthcube",
        _ => return Err(format!("unported: texture type {:#x}", ty.0)),
    }
    .to_string();
    if ms {
        n.push_str("_ms");
    }
    // Metal has no 3D texture array. The layer is dropped where the texture is used.
    if arr && dim != 4 {
        n.push_str("_array");
    }
    Ok(format!("{n}<{comp}{acc}>"))
}

fn arrayed(t: String, len: i32) -> R<String> {
    match len {
        1 => Ok(t),
        n if n > 1 => Ok(format!("array<{t}, {n}>")),
        _ => Err("unported: run time sized resource array".into()),
    }
}

struct Builder<'a> {
    program: &'a Program,
    usage: &'a Usage,
    stage: &'a str,
    notes: &'a [String],
    fetch1d: &'a BTreeSet<i32>,
    res: Res,
    members: String,
    id: u32,
    marks: bool,
}

fn find_info<'a>(l: &'a [TexInfo], d: &TextureDef) -> R<&'a TexInfo> {
    l.iter()
        .find(|t| {
            t.binding == i64::from(d.binding)
                && t.array_length == i64::from(d.array_length)
                && t.separate == d.separate
        })
        .ok_or_else(|| format!("resource {} has no descriptor", d.name))
}

impl Builder<'_> {
    fn member(&mut self, ty: &str, name: &str, len: i32, kind: Kind) -> &mut Binding {
        let id = self.id;
        self.id += len.max(1) as u32;
        self.members
            .push_str(&format!("    {ty} {name} [[id({id})]];\n"));
        self.res.bindings.push(Binding::new(name, ty, id, kind));
        let n = self.res.bindings.len() - 1;
        &mut self.res.bindings[n]
    }

    /// The last member joins the raster order group of the fragment shader interlock.
    fn ordered(&mut self) {
        if let Some(at) = self.members.rfind("]];") {
            self.members.insert_str(at, ", raster_order_group(0)");
        }
    }

    fn cbuf(&mut self, d: &BufferDef) -> R<()> {
        let name = d.name.clone();
        let (kind, reg, bank) = if name == "support_buffer" {
            (Kind::Support, None, None)
        } else if let Some(slot) = geom::helper_slot(&name) {
            (Kind::Geometry, Some(slot), None)
        } else if let Some(n) = bank_name(&name, 'c') {
            match n {
                3..=16 => (Kind::Cbuf, Some(n - 3), None),
                1 => (Kind::C1, None, Some(1)),
                _ => (Kind::DrvCb, None, Some(n as u32)),
            }
        } else {
            return Err(format!("unported: uniform buffer {name}"));
        };
        self.res.text.push_str(&buffers::block(&name, d)?);
        let ty = format!("constant {name}_block*");
        let b = self.member(&ty, &name, 1, kind);
        b.reg = reg;
        b.bank = bank;
        self.res.cbs.insert(
            d.binding,
            Buf {
                name,
                def: d.clone(),
                write: false,
            },
        );
        Ok(())
    }

    fn sbuf(&mut self, d: &BufferDef) -> R<()> {
        let name = d.name.clone();
        let info: Option<&BufferInfo> = self
            .program
            .sbufs
            .iter()
            .find(|b| b.binding == i64::from(d.binding));
        let g = |f: fn(&BufferInfo) -> i64| info.map_or(0, f);
        let (cb_slot, cb_off) = (g(|b| b.cb_slot), g(|b| b.cb_offset));
        let used_write = self.usage.sb.get(&d.binding).copied().unwrap_or(false);
        let rw = used_write || info.is_some_and(|b| b.flags & 1 != 0);
        self.res.text.push_str(&buffers::block(&name, d)?);
        if let Some(slot) = geom::helper_slot(&name) {
            let ty = geom::helper_type(&name, rw);
            let b = self.member(&ty, &name, 1, Kind::Geometry);
            b.reg = Some(slot);
            self.res.sbs.insert(
                d.binding,
                Buf {
                    name,
                    def: d.clone(),
                    write: rw,
                },
            );
            return Ok(());
        }
        let ty = format!("{}device {name}_block*", if rw { "" } else { "const " });
        let off = info.filter(|_| cb_slot == 0).map(|_| cb_off * 4);
        let stage = self.stage;
        let notes = self.notes;
        let b = self.member(&ty, &name, 1, Kind::Sbuf);
        if info.is_some() && cb_slot == i64::from(abi::GMEM_MAP_CB_SLOT) {
            if cb_off == i64::from(abi::GMEM_LOG_CB_OFFSET) {
                b.kind = Kind::GmemLog;
                b.rw = Some(true);
            } else {
                b.kind = Kind::GmemMap;
                b.rw = Some(rw || notes.iter().any(|n| untraced_write(n)));
            }
        } else {
            let ubo = off.filter(|o| {
                stage == "cs"
                    && (i64::from(abi::C0_CS_UBO)..i64::from(abi::C0_CS_UBO) + 256).contains(o)
                    && (o - i64::from(abi::C0_CS_UBO)) % 16 == 0
            });
            if let Some(o) = ubo {
                b.kind = Kind::UboPtr;
                b.reg = Some((o - i64::from(abi::C0_CS_UBO)) / 16);
            } else if info.is_some() && cb_slot >= 3 {
                b.kind = Kind::SbufUbo;
                b.reg = Some(cb_slot - 3);
                b.off = Some((cb_off * 4) as u32);
            } else {
                b.reg = off.and_then(|o| {
                    let base = abi::c0_ssbo(stage).map(i64::from)?;
                    let d = o - base;
                    ((0..256).contains(&d) && d % 16 == 0).then_some(d / 16)
                });
            }
            b.rw = Some(rw);
        }
        if rw && self.usage.interlock {
            self.ordered();
        }
        self.res.sbs.insert(
            d.binding,
            Buf {
                name,
                def: d.clone(),
                write: rw,
            },
        );
        Ok(())
    }

    /// A pool member: a pointer to the table the host keeps (see `pool`).
    fn pool_member(&mut self, name: &str, elem: &str, dec: &Decoded) {
        if elem != "sampler" && !self.marks {
            self.res.text.push_str(pool::MARK);
            self.marks = true;
        }
        self.res.text.push_str(&pool::strukt(name, elem));
        let ty = pool::member_type(name);
        let b = self.member(&ty, name, 1, dec.kind);
        dec.apply(b);
        b.elem = Some(elem.to_string());
    }

    /// The `render_scale` index of a texture or image that a scale helper reads (`ScaleUse`), or `None`.
    fn rscale(&self, image: bool, d: &TextureDef) -> Option<u32> {
        if d.array_length != 1 {
            return None;
        }
        self.program
            .scaled
            .iter()
            .find(|u| u.image == image && u.binding == i64::from(d.binding))
            .and_then(|u| u32::try_from(1 + u.index).ok())
    }

    fn texture(&mut self, d: &TextureDef) -> R<()> {
        if let Some(slot) = geom::helper_slot(&d.name) {
            let b = self.member(geom::TOPOLOGY_TYPE, &d.name, 1, Kind::Geometry);
            b.reg = Some(slot);
            self.res.texs.insert(
                (d.set, d.binding),
                Tx {
                    name: d.name.clone(),
                    def: d.clone(),
                    sampler: None,
                    twin: None,
                    pool: false,
                },
            );
            return Ok(());
        }
        let info = find_info(&self.program.textures, d)?.clone();
        let key = (d.set, d.binding);
        let array = d.array_length != 1;
        if d.ty == SamplerType::NONE {
            let pooled = pool::decode(&info, true, d.array_length);
            if let Some(dec) = &pooled {
                self.pool_member(&d.name, "sampler", dec);
            } else {
                let dec = decode(&info, false, true, false, array);
                let ty = arrayed("sampler".into(), d.array_length)?;
                let b = self.member(&ty, &d.name, d.array_length, dec.kind);
                dec.apply(b);
            }
            self.res.texs.insert(
                key,
                Tx {
                    name: d.name.clone(),
                    def: d.clone(),
                    sampler: None,
                    twin: None,
                    pool: pooled.is_some(),
                },
            );
            return Ok(());
        }
        let buffer = d.ty.0 & SamplerType::MASK == 2;
        let comp = "float";
        let elem = texture_type(d.ty, comp, None)?;
        let pooled = pool::decode(&info, false, d.array_length);
        let sized = self.usage.sized.contains(&key);
        let rscale = self.rscale(false, d);
        if let Some(dec) = &pooled {
            self.pool_member(&d.name, &elem, dec);
        } else {
            let ty = arrayed(elem, d.array_length)?;
            let dec = decode(&info, false, false, buffer, array);
            let fetch1d = dec.kind == Kind::Tex
                && dec.reg.is_some_and(|r| self.fetch1d.contains(&(r as i32)));
            let b = self.member(&ty, &d.name, d.array_length, dec.kind);
            dec.apply(b);
            b.fetch1d = fetch1d;
            b.fetch1d_size = fetch1d && sized;
            b.rscale = rscale;
        }
        let mut sampler = None;
        let needs = !d.separate && self.usage.sampled.contains(&key);
        if needs {
            let sn = format!("{}Smplr", d.name);
            if let Some(sd) = pool::decode(&info, true, d.array_length) {
                self.pool_member(&sn, "sampler", &sd);
            } else {
                let sd = decode(&info, false, true, false, array);
                let ty = arrayed("sampler".into(), d.array_length)?;
                let b = self.member(&ty, &sn, d.array_length, sd.kind);
                sd.apply(b);
            }
            sampler = Some(sn);
        }
        self.res.texs.insert(
            key,
            Tx {
                name: d.name.clone(),
                def: d.clone(),
                sampler,
                twin: None,
                pool: pooled.is_some(),
            },
        );
        Ok(())
    }

    fn image(&mut self, d: &TextureDef) -> R<()> {
        let info = find_info(&self.program.images, d)?.clone();
        let key = (d.set, d.binding);
        let u = self.usage.img.get(&key).copied().unwrap_or_default();
        let comp = match format_component_type(d.format) {
            t if t == crate::structured::types::AggType::U32 => "uint",
            t if t == crate::structured::types::AggType::S32 => "int",
            _ => "float",
        };
        let acc = match (u.read || u.atomic, u.write || u.atomic) {
            (_, false) => "read",
            (false, true) => "write",
            _ => "read_write",
        };
        let ty = arrayed(texture_type(d.ty, comp, Some(acc))?, d.array_length)?;
        let dec = decode(&info, true, false, false, d.array_length != 1);
        let rscale = self.rscale(true, d);
        let b = self.member(&ty, &d.name, d.array_length, dec.kind);
        dec.apply(b);
        b.atomic = u.atomic;
        b.rscale = rscale;
        if self.usage.interlock && (u.write || u.atomic) {
            self.ordered();
        }
        self.res.imgs.insert(
            key,
            Tx {
                name: d.name.clone(),
                def: d.clone(),
                sampler: None,
                twin: None,
                pool: false,
            },
        );
        Ok(())
    }

    /// The border twins of the samplers that a plain sample uses, then the border block.
    fn border(&mut self, props: &Props) -> R<()> {
        let mut bit = 0u32;
        let keys: Vec<(i32, i32, bool)> = self.usage.plain_sample.iter().copied().collect();
        let mut order: Vec<((i32, i32), String)> = Vec::new();
        for d in &props.properties.textures {
            let key = (d.set, d.binding);
            let Some(tx) = self.res.texs.get(&key) else {
                continue;
            };
            let sep = d.ty == SamplerType::NONE;
            let name = if sep {
                Some(tx.name.clone())
            } else {
                tx.sampler.clone()
            };
            if let Some(n) = name
                && d.array_length == 1
                && keys.contains(&(key.0, key.1, sep))
            {
                order.push((key, n));
            }
        }
        for (key, n) in order.into_iter().take(abi::BORDER_SAMPLERS as usize) {
            let base = self
                .res
                .bindings
                .iter()
                .find(|b| b.msl_name == n)
                .cloned()
                .ok_or("border sampler has no binding")?;
            let tn = format!("{n}{}", abi::BORDER_TWIN);
            let b = self.member("sampler", &tn, 1, base.kind);
            b.reg = base.reg;
            b.off = base.off;
            b.soff = base.soff;
            b.sreg = base.sreg;
            b.border_bit = Some(bit);
            if let Some(tx) = self.res.texs.get_mut(&key) {
                tx.twin = Some((tn, bit));
            }
            bit += 1;
        }
        if bit > 0 {
            self.member("constant dm_border_t*", abi::BORDER_MEMBER, 1, Kind::Border);
            self.res.border = true;
        }
        Ok(())
    }
}

/// A note that tells an untraced global store or atomic.
fn untraced_write(n: &str) -> bool {
    n.match_indices("operation \"").any(|(i, m)| {
        let r = &n[i + m.len()..];
        let op = r.split('"').next().unwrap_or("");
        let ok = op == "Store"
            || op
                .strip_prefix("Atomic")
                .is_some_and(|x| x.chars().all(|c| c.is_alphanumeric() || c == '_'));
        ok && r[op.len()..].starts_with("\" traced to no storage buffer")
    })
}

/// Lay out the argument buffer of a program.
pub fn build(
    props: &Props,
    program: &Program,
    usage: &Usage,
    stage: &str,
    notes: &[String],
    fetch1d: &BTreeSet<i32>,
) -> R<Res> {
    let mut b = Builder {
        program,
        usage,
        stage,
        notes,
        fetch1d,
        res: Res::default(),
        members: String::new(),
        id: 0,
        marks: false,
    };
    let mut cbs: Vec<&BufferDef> = props
        .properties
        .constant_buffers
        .iter()
        .filter(|d| usage.cb.contains(&d.binding))
        .collect();
    cbs.sort_by_key(|d| d.binding);
    for d in cbs {
        b.cbuf(d)?;
    }
    let mut sbs: Vec<&BufferDef> = props
        .properties
        .storage_buffers
        .iter()
        .filter(|d| usage.sb.contains_key(&d.binding))
        .collect();
    sbs.sort_by_key(|d| d.binding);
    for d in sbs {
        b.sbuf(d)?;
    }
    for d in &props.properties.textures {
        let key = (d.set, d.binding);
        let used = if d.ty == SamplerType::NONE {
            usage.smp.contains(&key)
        } else {
            usage.tex.contains(&key)
        };
        if used {
            b.texture(d)?;
        }
    }
    for d in &props.properties.images {
        if usage.img.contains_key(&(d.set, d.binding)) {
            b.image(d)?;
        }
    }
    b.border(props)?;
    let mut res = b.res;
    res.has_args = !b.members.is_empty();
    res.early = pool::early(&res, usage);
    if res.has_args {
        res.text
            .push_str(&format!("struct {SET_TYPE}\n{{\n{}}};\n", b.members));
    }
    Ok(res)
}

impl Res {
    /// The expression of a buffer member.
    pub fn deref(&self, b: &Buf) -> String {
        format!("{SET_NAME}.{}", b.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_types_name_their_dimension() {
        let t = |ty: SamplerType| texture_type(ty, "float", None).unwrap();
        assert_eq!(t(SamplerType::TEXTURE_2D), "texture2d<float>");
        assert_eq!(
            t(SamplerType::TEXTURE_2D.with(SamplerType::SHADOW | SamplerType::ARRAY)),
            "depth2d_array<float>"
        );
        assert_eq!(
            t(SamplerType::TEXTURE_2D.with(SamplerType::MULTISAMPLE)),
            "texture2d_ms<float>"
        );
        assert_eq!(t(SamplerType::TEXTURE_BUFFER), "texture_buffer<float>");
        assert_eq!(
            texture_type(SamplerType::TEXTURE_CUBE, "uint", Some("read_write")).unwrap(),
            "texturecube<uint, access::read_write>"
        );
    }

    #[test]
    fn bank_names_have_a_stage_prefix() {
        assert_eq!(bank_name("fp_c3", 'c'), Some(3));
        assert_eq!(bank_name("vp_s12", 's'), Some(12));
        assert_eq!(bank_name("support_buffer", 'c'), None);
    }
}
