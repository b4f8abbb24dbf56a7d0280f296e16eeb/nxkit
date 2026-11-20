//! The arithmetic, logic, comparison, conversion and bit instructions.

use super::emit::{Em, R, Val};
use crate::ir::Inst;
use crate::ir::agg::AggType;
use crate::structured::ast::OpData;

const F: AggType = AggType::FP32;
const S: AggType = AggType::S32;
const U: AggType = AggType::U32;
const B: AggType = AggType::BOOL;

fn flags(o: &OpData) -> R<AggType> {
    if o.inst.0 & Inst::FP64 != 0 {
        Err("unported: 64 bit float in MSL".into())
    } else if o.inst.0 & Inst::FP32 != 0 {
        Ok(F)
    } else {
        Ok(S)
    }
}

impl Em<'_> {
    /// One operation as a value. `None` for an instruction that is a statement.
    pub(super) fn operation(&mut self, o: &OpData) -> R<Option<Val>> {
        let inst = Self::inst_of(o);
        if let Some(v) = self.alu(inst, o)? {
            return Ok(Some(v));
        }
        match inst {
            Inst::Load
            | Inst::Store
            | Inst::AtomicAdd
            | Inst::AtomicAnd
            | Inst::AtomicCompareAndSwap
            | Inst::AtomicMinS32
            | Inst::AtomicMinU32
            | Inst::AtomicMaxS32
            | Inst::AtomicMaxU32
            | Inst::AtomicOr
            | Inst::AtomicSwap
            | Inst::AtomicXor => self.memory(inst, o),
            Inst::Call => self.call(o),
            Inst::Ballot
            | Inst::VoteAll
            | Inst::VoteAllEqual
            | Inst::VoteAny
            | Inst::Shuffle
            | Inst::ShuffleDown
            | Inst::ShuffleUp
            | Inst::ShuffleXor
            | Inst::SwizzleAdd
            | Inst::Barrier
            | Inst::SubgroupBarrier
            | Inst::GroupMemoryBarrier
            | Inst::MemoryBarrier => self.subgroup(inst, o),
            Inst::TextureSample
            | Inst::TextureQuerySamples
            | Inst::TextureQuerySize
            | Inst::Lod => self.texture(inst, o),
            Inst::ImageLoad | Inst::ImageStore | Inst::ImageAtomic => self.image(inst, o),
            Inst::Return
            | Inst::Discard
            | Inst::LoopBreak
            | Inst::LoopContinue
            | Inst::FSIBegin
            | Inst::FSIEnd
            | Inst::Comment
            | Inst::EmitVertex
            | Inst::EndPrimitive => self.control_op(inst, o),
            _ => Err(format!("unported: instruction {:x}", o.inst.0)),
        }
    }

    fn fp_contract(&mut self, o: &OpData, v: Val) -> R<Val> {
        if o.force_precise && self.props.host.reduced_precision {
            let n = self.tmp;
            self.tmp += 1;
            let t = super::types::msl_type(v.ty)?;
            self.pre.push(format!("{t} t{n};"));
            self.pre.push("{".into());
            self.pre.push("#pragma clang fp contract(off)".into());
            self.pre.push(format!("t{n} = {};", v.s));
            self.pre.push("}".into());
            return Ok(Val::new(format!("t{n}"), v.ty));
        }
        Ok(v)
    }

    fn un(&mut self, o: &OpData, f: &str, int: Option<&str>) -> R<Val> {
        let t = flags(o)?;
        let a = self.get(t, self.src(o, 0)?)?;
        let name = if t == F { Some(f) } else { int };
        let name = name.ok_or("no integer form of this instruction")?;
        Ok(Val::new(format!("{name}({})", a.s), t))
    }

    fn bin(&mut self, o: &OpData, op: &str, int: bool) -> R<Val> {
        let t = flags(o)?;
        if t == S && !int {
            return Err("no integer form of this instruction".into());
        }
        let a = self.get(t, self.src(o, 0)?)?;
        let b = self.get(t, self.src(o, 1)?)?;
        let v = Val::new(format!("({} {op} {})", a.s, b.s), t);
        self.fp_contract(o, v)
    }

    fn call2(&mut self, t: AggType, name: &str, o: &OpData) -> R<Val> {
        let a = self.get(t, self.src(o, 0)?)?;
        let b = self.get(t, self.src(o, 1)?)?;
        Ok(Val::new(format!("{name}({}, {})", a.s, b.s), t))
    }

    fn call3(&mut self, t: AggType, name: &str, o: &OpData) -> R<Val> {
        let a = self.get(t, self.src(o, 0)?)?;
        let b = self.get(t, self.src(o, 1)?)?;
        let c = self.get(t, self.src(o, 2)?)?;
        Ok(Val::new(format!("{name}({}, {}, {})", a.s, b.s, c.s), t))
    }

    fn cmp(&mut self, o: &OpData, t: AggType, op: &str) -> R<Val> {
        let a = self.get(t, self.src(o, 0)?)?;
        let b = self.get(t, self.src(o, 1)?)?;
        Ok(Val::new(format!("({} {op} {})", a.s, b.s), B))
    }

    fn conv(&mut self, o: &OpData, from: AggType, to: &str, ty: AggType) -> R<Val> {
        let a = self.get(from, self.src(o, 0)?)?;
        Ok(Val::new(format!("{to}({})", a.s), ty))
    }

    fn alu(&mut self, inst: Inst, o: &OpData) -> R<Option<Val>> {
        let sh = |i: &mut Em<'_>, op: &str| -> R<Val> {
            let a = i.get(S, i.src(o, 0)?)?;
            let b = i.get(S, i.src(o, 1)?)?;
            Ok(Val::new(format!("({} {op} {})", a.s, b.s), S))
        };
        let v = match inst {
            Inst::Absolute => {
                let v = self.un(o, "abs", Some("abs"))?;
                if v.ty == S {
                    Val::new(format!("int({})", v.s), S)
                } else {
                    v
                }
            }
            Inst::Add => self.bin(o, "+", true)?,
            Inst::Subtract => self.bin(o, "-", true)?,
            Inst::Multiply => self.bin(o, "*", true)?,
            Inst::Divide => self.bin(o, "/", true)?,
            Inst::Negate => {
                let t = flags(o)?;
                let a = self.get(t, self.src(o, 0)?)?;
                Val::new(format!("(-{})", a.s), t)
            }
            Inst::Ceiling => self.un(o, "ceil", None)?,
            Inst::Floor => self.un(o, "floor", None)?,
            Inst::Truncate => self.un(o, "trunc", None)?,
            Inst::Round => self.un(o, "rint", None)?,
            Inst::Cosine => self.un(o, "cos", None)?,
            Inst::Sine => self.un(o, "sin", None)?,
            Inst::ExponentB2 => self.un(o, "exp2", None)?,
            Inst::LogarithmB2 => self.un(o, "log2", None)?,
            Inst::SquareRoot => self.un(o, "sqrt", None)?,
            Inst::ReciprocalSquareRoot => self.un(o, "rsqrt", None)?,
            Inst::FusedMultiplyAdd => {
                let t = flags(o)?;
                if t != F {
                    return Err("no integer form of this instruction".into());
                }
                self.call3(F, "fma", o)?
            }
            Inst::Clamp => {
                let t = flags(o)?;
                self.call3(t, if t == F { "precise::clamp" } else { "clamp" }, o)?
            }
            Inst::Minimum => {
                let t = flags(o)?;
                self.call2(t, if t == F { "precise::min" } else { "min" }, o)?
            }
            Inst::Maximum => {
                let t = flags(o)?;
                self.call2(t, if t == F { "precise::max" } else { "max" }, o)?
            }
            Inst::ClampU32 => self.call3(U, "clamp", o)?,
            Inst::MinimumU32 => self.call2(U, "min", o)?,
            Inst::MaximumU32 => self.call2(U, "max", o)?,
            Inst::Modulo => {
                if flags(o)? != F {
                    return Err("no integer form of this instruction".into());
                }
                let x = self.get(F, self.src(o, 0)?)?;
                let x = self.once(x)?;
                let y = self.get(F, self.src(o, 1)?)?;
                let y = self.once(y)?;
                let v = Val::new(format!("({0} - {1} * floor({0} / {1}))", x.s, y.s), F);
                self.fp_contract(o, v)?
            }
            Inst::CompareEqual => self.cmp_any(o, "==")?,
            Inst::CompareGreater => self.cmp_any(o, ">")?,
            Inst::CompareGreaterOrEqual => self.cmp_any(o, ">=")?,
            Inst::CompareLess => self.cmp_any(o, "<")?,
            Inst::CompareLessOrEqual => self.cmp_any(o, "<=")?,
            Inst::CompareNotEqual => {
                let t = flags(o)?;
                if t == F {
                    let a = self.get(F, self.src(o, 0)?)?;
                    let a = self.once(a)?;
                    let b = self.get(F, self.src(o, 1)?)?;
                    let b = self.once(b)?;
                    Val::new(format!("({0} < {1} || {0} > {1})", a.s, b.s), B)
                } else {
                    self.cmp(o, S, "!=")?
                }
            }
            Inst::CompareGreaterOrEqualU32 => self.cmp(o, U, ">=")?,
            Inst::CompareGreaterU32 => self.cmp(o, U, ">")?,
            Inst::CompareLessOrEqualU32 => self.cmp(o, U, "<=")?,
            Inst::CompareLessU32 => self.cmp(o, U, "<")?,
            Inst::ConditionalSelect => {
                let t = flags(o)?;
                let c = self.get(B, self.src(o, 0)?)?;
                let a = self.get(t, self.src(o, 1)?)?;
                let b = self.get(t, self.src(o, 2)?)?;
                Val::new(format!("({} ? {} : {})", c.s, a.s, b.s), t)
            }
            Inst::ConvertFP32ToS32 => self.conv(o, F, "int", S)?,
            Inst::ConvertFP32ToU32 => self.conv(o, F, "uint", U)?,
            Inst::ConvertS32ToFP32 => self.conv(o, S, "float", F)?,
            Inst::ConvertU32ToFP32 => self.conv(o, U, "float", F)?,
            Inst::ConvertFP32ToFP64
            | Inst::ConvertFP64ToFP32
            | Inst::ConvertFP64ToS32
            | Inst::ConvertFP64ToU32
            | Inst::ConvertS32ToFP64
            | Inst::ConvertU32ToFP64
            | Inst::PackDouble2x32
            | Inst::UnpackDouble2x32 => return Err("unported: 64 bit float in MSL".into()),
            Inst::Ddx | Inst::Ddy => {
                let a = self.get(F, self.src(o, 0)?)?;
                let f = if inst == Inst::Ddx { "dfdx" } else { "dfdy" };
                Val::new(format!("{f}({})", a.s), F)
            }
            Inst::BitCount => {
                let a = self.get(S, self.src(o, 0)?)?;
                Val::new(format!("popcount({})", a.s), S)
            }
            Inst::BitfieldExtractS32 => {
                let a = self.get(S, self.src(o, 0)?)?;
                let b = self.get(U, self.src(o, 1)?)?;
                let c = self.get(U, self.src(o, 2)?)?;
                Val::new(format!("extract_bits({}, {}, {})", a.s, b.s, c.s), S)
            }
            Inst::BitfieldExtractU32 => self.call3(U, "extract_bits", o)?,
            Inst::BitfieldInsert => {
                let a = self.get(U, self.src(o, 0)?)?;
                let b = self.get(U, self.src(o, 1)?)?;
                let c = self.get(U, self.src(o, 2)?)?;
                let d = self.get(U, self.src(o, 3)?)?;
                Val::new(
                    format!("insert_bits({}, {}, {}, {})", a.s, b.s, c.s, d.s),
                    U,
                )
            }
            Inst::BitfieldReverse => {
                let a = self.get(S, self.src(o, 0)?)?;
                Val::new(format!("reverse_bits({})", a.s), S)
            }
            Inst::BitwiseAnd => sh(self, "&")?,
            Inst::BitwiseOr => sh(self, "|")?,
            Inst::BitwiseExclusiveOr => sh(self, "^")?,
            Inst::BitwiseNot => {
                let a = self.get(S, self.src(o, 0)?)?;
                Val::new(format!("(~{})", a.s), S)
            }
            Inst::ShiftLeft => sh(self, "<<")?,
            Inst::ShiftRightS32 => sh(self, ">>")?,
            Inst::ShiftRightU32 => {
                let a = self.get(U, self.src(o, 0)?)?;
                let b = self.get(U, self.src(o, 1)?)?;
                Val::new(format!("as_type<int>({} >> {})", a.s, b.s), S)
            }
            Inst::FindLSB => {
                let a = self.get(U, self.src(o, 0)?)?;
                let a = self.once(a)?;
                Val::new(format!("({0} == 0u ? 0xffffffffu : ctz({0}))", a.s), U)
            }
            Inst::FindMSBS32 => {
                let a = self.get(S, self.src(o, 0)?)?;
                self.helpers.insert("findmsb");
                Val::new(format!("dm_find_smsb({})", a.s), U)
            }
            Inst::FindMSBU32 => {
                let a = self.get(U, self.src(o, 0)?)?;
                self.helpers.insert("findmsb");
                Val::new(format!("dm_find_umsb({})", a.s), U)
            }
            Inst::MultiplyHighS32 => self.call2(S, "mulhi", o)?,
            Inst::MultiplyHighU32 => self.call2(U, "mulhi", o)?,
            Inst::PackHalf2x16 => {
                let a = self.get(F, self.src(o, 0)?)?;
                let b = self.get(F, self.src(o, 1)?)?;
                Val::new(format!("as_type<uint>(half2(float2({}, {})))", a.s, b.s), U)
            }
            Inst::UnpackHalf2x16 => {
                let a = self.get(U, self.src(o, 0)?)?;
                Val::new(format!("float(as_type<half2>({})[{}])", a.s, o.index), F)
            }
            Inst::IsNan => {
                if flags(o)? != F {
                    return Err("no integer form of this instruction".into());
                }
                let a = self.get(F, self.src(o, 0)?)?;
                Val::new(format!("isnan({})", a.s), B)
            }
            Inst::LogicalAnd => self.call_b(o, "&&")?,
            Inst::LogicalOr => self.call_b(o, "||")?,
            Inst::LogicalExclusiveOr => self.call_b(o, "!=")?,
            Inst::LogicalNot => {
                let a = self.get(B, self.src(o, 0)?)?;
                Val::new(format!("(!{})", a.s), B)
            }
            Inst::VectorExtract => {
                let v = self.eval(self.src(o, 0)?)?;
                let v = self.once(v)?;
                let t = AggType(v.ty.0 & !AggType::ELEMENT_COUNT_MASK);
                let idx = match self.constant(self.src(o, 1)?) {
                    Some(c) => c.to_string(),
                    None => self.get(S, self.src(o, 1)?)?.s,
                };
                Val::new(format!("{}[{idx}]", v.s), t)
            }
            _ => return Ok(None),
        };
        Ok(Some(v))
    }

    fn cmp_any(&mut self, o: &OpData, op: &str) -> R<Val> {
        let t = flags(o)?;
        self.cmp(o, t, op)
    }

    fn call_b(&mut self, o: &OpData, op: &str) -> R<Val> {
        let a = self.get(B, self.src(o, 0)?)?;
        let b = self.get(B, self.src(o, 1)?)?;
        Ok(Val::new(format!("({} {op} {})", a.s, b.s), B))
    }
}
