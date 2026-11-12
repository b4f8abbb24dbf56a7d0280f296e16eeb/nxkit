//! Loads, stores and atomics on constant buffers, storage buffers, global memory, local and shared memory and the
//! stage inputs and outputs, and calls of helper functions.

use super::args::SET_NAME;
use super::buffers;
use super::emit::{Em, R, Shader, Val};
use super::types::{msl_type, scalar};
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::structured::ast::{A, Kind, OpData};
use crate::structured::types::AggType;

/// The address space of a place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Space {
    Thread,
    Device,
    Threadgroup,
    Constant,
}

/// An addressable element and its types.
pub(super) struct Place {
    /// The expression.
    pub s: String,
    /// The type the instruction sees.
    pub ty: AggType,
    /// The type the element is stored as.
    pub mem: AggType,
    /// The address space.
    pub space: Space,
    /// The store has no effect (a point size next to a layer).
    pub skip: bool,
    /// The value is a viewport mask and the place takes its viewport index.
    pub mask: bool,
}

pub(super) fn is_atomic(inst: Inst) -> bool {
    matches!(
        inst,
        Inst::AtomicAdd
            | Inst::AtomicAnd
            | Inst::AtomicCompareAndSwap
            | Inst::AtomicMaxS32
            | Inst::AtomicMaxU32
            | Inst::AtomicMinS32
            | Inst::AtomicMinU32
            | Inst::AtomicOr
            | Inst::AtomicSwap
            | Inst::AtomicXor
    )
}

pub(super) fn swizzle_or_index(base: String, vector: bool, index: &str, c: Option<i32>) -> String {
    match (vector, c) {
        (true, Some(c)) if (0..4).contains(&c) => {
            format!("{base}.{}", ["x", "y", "z", "w"][c as usize])
        }
        _ => format!("{base}[{index}]"),
    }
}

impl Em<'_> {
    fn indices(&mut self, o: &OpData, from: usize, to: usize) -> R<Vec<String>> {
        let mut v = Vec::new();
        for i in from..to {
            let a = self.src(o, i)?;
            v.push(self.get(AggType::S32, a)?.s);
        }
        Ok(v)
    }

    fn record_drv_read(&mut self, name: &str, o: &OpData, first: usize) {
        let Some(n) = name.strip_prefix("fp_c") else {
            return;
        };
        let Ok(bank) = n.parse::<u32>() else { return };
        if bank != 0 && bank != 2 {
            return;
        }
        let konst = o.sources.get(first).and_then(|a| self.constant(*a));
        match konst {
            Some(k) if k >= 0 => {
                self.drv.words.entry(bank).or_default().insert(k as u32);
            }
            _ => {
                self.drv.dynamic.insert(bank);
            }
        }
    }

    /// The place an access instruction reaches.
    pub(super) fn place(&mut self, inst: Inst, o: &OpData) -> R<Place> {
        let trailing = usize::from(inst == Inst::Store || is_atomic(inst));
        let n = o.sources.len();
        match o.storage_kind {
            StorageKind::GlobalMemory => {
                let lo = self.get(AggType::U32, self.src(o, 0)?)?.s;
                let hi = self.get(AggType::U32, self.src(o, 1)?)?.s;
                Ok(Place {
                    s: format!(
                        "(*reinterpret_cast<device uint*>(as_type<ulong>(uint2({lo}, {hi}))))"
                    ),
                    ty: AggType::U32,
                    mem: AggType::U32,
                    space: Space::Device,
                    skip: false,
                    mask: false,
                })
            }
            StorageKind::ConstantBuffer | StorageKind::StorageBuffer => {
                let cb = o.storage_kind == StorageKind::ConstantBuffer;
                let binding = self
                    .constant(self.src(o, 0)?)
                    .ok_or("buffer access without a constant binding")?;
                let field = self
                    .constant(self.src(o, 1)?)
                    .ok_or("buffer access without a constant field")?;
                let buf = if cb { &self.res.cbs } else { &self.res.sbs }
                    .get(&binding)
                    .ok_or_else(|| format!("buffer {binding} is not in the argument buffer"))?
                    .clone();
                let f = buf
                    .def
                    .fields
                    .get(field as usize)
                    .ok_or("field out of range")?;
                let ty = scalar(f.ty);
                let cas = usize::from(inst == Inst::AtomicCompareAndSwap);
                let end = n - trailing - cas;
                if cb {
                    self.record_drv_read(&buf.name, o, 2);
                    if self.shader == Shader::Vertex
                        && buf.name == "vp_c0"
                        && end == 4
                        && self.constant(self.src(o, 2)?) == Some(100)
                        && self.constant(self.src(o, 3)?) == Some(1)
                    {
                        return Ok(Place {
                            s: "as_type<float>(gl_BaseInstance)".into(),
                            ty: AggType::FP32,
                            mem: AggType::FP32,
                            space: Space::Constant,
                            skip: false,
                            mask: false,
                        });
                    }
                }
                let idx = self.indices(o, 2, end)?;
                let s = buffers::access(
                    &format!("{SET_NAME}.{}", buf.name),
                    &buf.def,
                    field as usize,
                    &idx,
                )?;
                let mem = if matches!(ty, t if t == AggType::BOOL) {
                    AggType::U32
                } else {
                    ty
                };
                Ok(Place {
                    s,
                    ty,
                    mem,
                    space: if cb { Space::Constant } else { Space::Device },
                    skip: false,
                    mask: false,
                })
            }
            StorageKind::LocalMemory | StorageKind::SharedMemory => {
                let id = self
                    .constant(self.src(o, 0)?)
                    .ok_or("memory access without a constant id")?;
                let local = o.storage_kind == StorageKind::LocalMemory;
                let defs = if local {
                    &self.props.properties.local_memories
                } else {
                    &self.props.properties.shared_memories
                };
                let m = defs
                    .iter()
                    .find(|m| m.id == id)
                    .ok_or("memory is not declared")?;
                let ty = scalar(m.ty);
                let cas = usize::from(inst == Inst::AtomicCompareAndSwap);
                let mut s = format!("{}{id}", if local { "lm" } else { "sm" });
                for i in self.indices(o, 1, n - trailing - cas)? {
                    s.push_str(&format!("[{i}]"));
                }
                Ok(Place {
                    s,
                    ty,
                    mem: ty,
                    space: if local {
                        Space::Thread
                    } else {
                        Space::Threadgroup
                    },
                    skip: false,
                    mask: false,
                })
            }
            StorageKind::Input | StorageKind::Output => self.io_place(inst, o, trailing),
            other => Err(format!("unported: storage kind {}", other.0)),
        }
    }

    fn io_place(&mut self, inst: Inst, o: &OpData, trailing: usize) -> R<Place> {
        let _ = inst;
        let out = o.storage_kind == StorageKind::Output;
        let var = IoVariable(
            self.constant(self.src(o, 0)?)
                .ok_or("io access without a constant variable")? as u32,
        );
        let d = &self.props.definitions;
        if out && super::ioidx::is_mask(var) {
            return Ok(self.viewport_mask_place());
        }
        if var == IoVariable::UserDefined && (if out { d.oa_indexing } else { d.ia_indexing }) {
            return self.indexed_place(o, out, 1);
        }
        let (mut at, mut loc) = (1, 0);
        if d.has_per_location_input_or_output(var, out) {
            loc = self
                .constant(self.src(o, 1)?)
                .ok_or("io access without a constant location")?;
            at = 2;
        }
        let ir = if var == IoVariable::UserDefined {
            d.user_defined_type(loc, out)?
        } else if var == IoVariable::FragmentOutputColor {
            d.fragment_output_color_type(loc)?
        } else {
            super::io::built_in_type(var)?
        };
        let color = if var == IoVariable::FragmentOutputColor {
            ir
        } else {
            AggType::VECTOR4 | AggType::FP32
        };
        let (base, stored) = if out {
            self.plan.output(var, loc, color)?
        } else {
            self.plan.input(var, loc)?
        };
        let vector = stored.0 & AggType::ELEMENT_COUNT_MASK != 0;
        let mut s = base;
        let end = o.sources.len() - trailing;
        let skip = out && var == IoVariable::PointSize && self.plan.layer;
        if end > at {
            let a = self.src(o, at)?;
            let c = self.constant(a);
            let i = self.get(AggType::S32, a)?.s;
            let is_clip = var == IoVariable::ClipDistance;
            s = if is_clip {
                format!("{s}[{i}]")
            } else {
                swizzle_or_index(s, vector, &i, c)
            };
            if end > at + 1 {
                return Err("unported: nested index into stage IO".into());
            }
        }
        Ok(Place {
            s,
            ty: scalar(ir),
            mem: scalar(stored),
            space: Space::Thread,
            skip,
            mask: false,
        })
    }

    /// A store of an integer value to a component of a user output of a vertex shader is a candidate for the layer.
    fn note_layer_source(&mut self, o: &OpData) {
        let int = matches!(self.last_src, Some(t) if t == AggType::S32 || t == AggType::U32);
        let out = o.storage_kind == StorageKind::Output;
        if self.shader != Shader::Vertex
            || self.exit.is_some()
            || !out
            || !int
            || o.sources.len() != 4
        {
            return;
        }
        let k = |em: &Self, i: usize| o.sources.get(i).and_then(|a| em.constant(*a));
        if k(self, 0) == Some(IoVariable::UserDefined.0 as i32)
            && let (Some(l), Some(c)) = (k(self, 1), k(self, 2))
        {
            self.layer_src.push((self.func, l, c));
        }
    }

    /// A load that is a lane of a wide load. The first lane reads the whole run.
    pub(super) fn wide_lane(&mut self, a: A) -> R<Option<Val>> {
        let Some(l) = self.shared.wide.get(&a).copied() else {
            return Ok(None);
        };
        if l.lane == 0 {
            let Kind::Operation(o) = self.info.ast.kind(a).clone() else {
                return Ok(None);
            };
            let p = self.place(Inst::Load, &o)?;
            self.pre.push(format!(
                "packed_uint{n} dw{r} = *(const device packed_uint{n}*)&{};",
                p.s,
                n = l.len,
                r = l.run
            ));
        }
        let v = Val::new(
            format!("dw{}.{}", l.run, ["x", "y", "z", "w"][l.lane as usize]),
            AggType::U32,
        );
        Ok(Some(self.as_type(v, l.ty)))
    }

    /// Loads, stores and atomics.
    pub(super) fn memory(&mut self, inst: Inst, o: &OpData) -> R<Option<Val>> {
        let p = self.place(inst, o)?;
        match inst {
            Inst::Load => {
                let v = Val::new(p.s, p.mem);
                Ok(Some(self.as_type(v, p.ty)))
            }
            Inst::Store => {
                let last = self.src(o, o.sources.len() - 1)?;
                let v = self.get(p.ty, last)?;
                self.note_layer_source(o);
                if p.skip {
                    return Ok(None);
                }
                let v = if p.mask {
                    Val::new(Self::mask_index(&v.s), AggType::U32)
                } else {
                    v
                };
                let v = self.as_type(v, p.mem);
                let stmt = format!("{} = {};", p.s, v.s);
                self.line(&stmt);
                Ok(None)
            }
            _ => self.atomic(inst, o, p).map(Some),
        }
    }

    fn atomic(&mut self, inst: Inst, o: &OpData, p: Place) -> R<Val> {
        let space = match p.space {
            Space::Device => "device",
            Space::Threadgroup if self.shared.spill => "device",
            Space::Threadgroup => "threadgroup",
            _ => return Err("unported: atomic on memory that is not shared".into()),
        };
        let signed = matches!(inst, Inst::AtomicMinS32 | Inst::AtomicMaxS32);
        let (aty, vty) = if signed {
            ("atomic_int", AggType::S32)
        } else {
            ("atomic_uint", AggType::U32)
        };
        let ptr = format!("reinterpret_cast<{space} {aty}*>(&{})", p.s);
        let ptr = self.hoist_text(&ptr, &format!("{space} {aty}*"));
        let n = o.sources.len();
        let value_at = |em: &mut Self, i: usize| -> R<Val> {
            let a = em.src(o, i)?;
            let v = em.get(p.ty, a)?;
            Ok(em.as_type(v, vty))
        };
        let t = msl_type(vty)?;
        let r = if inst == Inst::AtomicCompareAndSwap {
            let cmp = value_at(self, n - 2)?;
            let new = value_at(self, n - 1)?;
            let e = self.tmp;
            self.tmp += 1;
            // a weak exchange may fail with the expected value still in memory: it leaves t{e} as it was, so try again
            self.pre
                .push(format!("{t} t{e} = {}; {t} t{e}c = t{e};", cmp.s));
            self.pre.push(format!(
                "while (!atomic_compare_exchange_weak_explicit({ptr}, &t{e}, {}, memory_order_relaxed, memory_order_relaxed) && t{e} == t{e}c) {{}}",
                new.s
            ));
            Val::new(format!("t{e}"), vty)
        } else {
            let f = match inst {
                Inst::AtomicAdd => "add",
                Inst::AtomicAnd => "and",
                Inst::AtomicOr => "or",
                Inst::AtomicXor => "xor",
                Inst::AtomicMinS32 | Inst::AtomicMinU32 => "min",
                Inst::AtomicMaxS32 | Inst::AtomicMaxU32 => "max",
                _ => "exchange",
            };
            let name = if f == "exchange" {
                "atomic_exchange_explicit".to_string()
            } else {
                format!("atomic_fetch_{f}_explicit")
            };
            let v = value_at(self, n - 1)?;
            let call = format!("{name}({ptr}, {}, memory_order_relaxed)", v.s);
            self.effect(call, vty)?
        };
        Ok(self.as_type(r, p.ty))
    }

    /// A pointer or other text hoisted to a typed temporary.
    fn hoist_text(&mut self, s: &str, ty: &str) -> String {
        let n = self.tmp;
        self.tmp += 1;
        self.pre.push(format!("{ty} t{n} = {s};"));
        format!("t{n}")
    }

    /// A call of a helper function.
    pub(super) fn call(&mut self, o: &OpData) -> R<Option<Val>> {
        let id = self
            .constant(self.src(o, 0)?)
            .ok_or("call without a function id")?;
        let f = self
            .info
            .functions
            .get(id as usize)
            .ok_or("call to an unknown function")?;
        let ret = f.return_type;
        let inputs = f.in_arguments.clone();
        let mut args = Vec::new();
        for i in 1..o.sources.len() {
            let a = self.src(o, i)?;
            match inputs.get(i - 1) {
                // A parameter is a reference: a literal or an expression goes through a variable.
                Some(ty) => {
                    let v = self.get(*ty, a)?;
                    args.push(
                        if v.s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                            && !v.s.starts_with(|c: char| c.is_ascii_digit())
                        {
                            v.s
                        } else {
                            self.hoist_text(&v.s, &msl_type(*ty)?)
                        },
                    );
                }
                None => args.push(self.eval(a)?.s),
            }
        }
        if super::hmma::is_helper(self, id as usize) {
            args.push(super::hmma::ARGS.to_string());
            self.hmma_caller = true;
        }
        args.extend(self.shared.params.iter().map(|(_, a)| a.clone()));
        let call = format!("fn{id}({})", args.join(", "));
        if ret == AggType::VOID {
            self.line(&format!("{call};"));
            self.after_call()?;
            Ok(None)
        } else {
            let v = self.effect(call, ret)?;
            self.after_call()?;
            Ok(Some(v))
        }
    }
}
