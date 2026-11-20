//! `AggregateType` and `InstructionInfo` (the table of destination and source types of every instruction).

use super::{Inst, OperandType};

/// `AggregateType` (a number, as the C# flags enum).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AggType(pub u32);

#[allow(dead_code)]
impl AggType {
    pub const INVALID: AggType = AggType(0);
    pub const VOID: AggType = AggType(1);
    pub const BOOL: AggType = AggType(2);
    pub const FP32: AggType = AggType(3);
    pub const FP64: AggType = AggType(4);
    pub const S32: AggType = AggType(5);
    pub const U32: AggType = AggType(6);
    pub const ELEMENT_TYPE_MASK: u32 = 0xff;
    pub const ELEMENT_COUNT_MASK: u32 = 3 << 8;
    pub const SCALAR: AggType = AggType(0);
    pub const VECTOR2: AggType = AggType(1 << 8);
    pub const VECTOR3: AggType = AggType(2 << 8);
    pub const VECTOR4: AggType = AggType(3 << 8);
    pub const ARRAY: AggType = AggType(1 << 10);

    pub fn has(self, flag: AggType) -> bool {
        self.0 & flag.0 == flag.0
    }

    /// `type & mask`
    pub fn and(self, mask: u32) -> AggType {
        AggType(self.0 & mask)
    }

    /// `GetSizeInBytes`.
    pub fn size_in_bytes(self) -> i32 {
        let mut s = match self.0 & Self::ELEMENT_TYPE_MASK {
            2 | 3 | 5 | 6 => 4,
            4 => 8,
            _ => 0,
        };
        match self.0 & Self::ELEMENT_COUNT_MASK {
            x if x == Self::VECTOR2.0 => s *= 2,
            x if x == Self::VECTOR3.0 => s *= 3,
            x if x == Self::VECTOR4.0 => s *= 4,
            _ => {}
        }
        s
    }
}

impl std::ops::BitOr for AggType {
    type Output = AggType;
    fn bitor(self, o: AggType) -> AggType {
        AggType(self.0 | o.0)
    }
}

impl std::ops::BitOrAssign for AggType {
    fn bitor_assign(&mut self, o: AggType) {
        self.0 |= o.0;
    }
}

/// `OperandInfo.GetVarType(OperandType)`.
pub fn var_type_of_operand_type(t: OperandType) -> Result<AggType, String> {
    match t {
        OperandType::Argument | OperandType::Constant | OperandType::Undefined => Ok(AggType::S32),
        _ => Err(format!("Invalid operand type \"{t:?}\".")),
    }
}

/// `TextureFormat.GetComponentType`.
pub fn format_component_type(format: usize) -> AggType {
    const U: [&str; 10] = [
        "R8Uint",
        "R16Uint",
        "R32Uint",
        "R8G8Uint",
        "R16G16Uint",
        "R32G32Uint",
        "R8G8B8A8Uint",
        "R16G16B16A16Uint",
        "R32G32B32A32Uint",
        "R10G10B10A2Uint",
    ];
    const S: [&str; 9] = [
        "R8Sint",
        "R16Sint",
        "R32Sint",
        "R8G8Sint",
        "R16G16Sint",
        "R32G32Sint",
        "R8G8B8A8Sint",
        "R16G16B16A16Sint",
        "R32G32B32A32Sint",
    ];
    let name = crate::types::TEXTURE_FORMATS
        .get(format)
        .copied()
        .unwrap_or("");
    if U.contains(&name) {
        AggType::U32
    } else if S.contains(&name) {
        AggType::S32
    } else {
        AggType::FP32
    }
}

const SC: AggType = AggType(0xffff_0000); // marker for "Scalar" in the table (resolved by the instruction flags)

struct Info {
    dest: AggType,
    src: &'static [AggType],
}

const S32: AggType = AggType::S32;
const U32: AggType = AggType::U32;
const BOOL: AggType = AggType::BOOL;
const F32: AggType = AggType::FP32;
const F64: AggType = AggType::FP64;
const VOID: AggType = AggType::VOID;

fn info(inst: Inst) -> Option<Info> {
    let i = |dest, src| Some(Info { dest, src });
    match inst {
        Inst::AtomicAdd | Inst::AtomicAnd | Inst::AtomicOr | Inst::AtomicSwap | Inst::AtomicXor => {
            i(U32, &[S32, S32, U32])
        }
        Inst::AtomicCompareAndSwap => i(U32, &[S32, S32, U32, U32]),
        Inst::AtomicMaxS32 | Inst::AtomicMinS32 => i(S32, &[S32, S32, S32]),
        Inst::AtomicMaxU32 | Inst::AtomicMinU32 => i(U32, &[S32, S32, U32]),
        Inst::Absolute => i(SC, &[SC]),
        Inst::Add => i(SC, &[SC, SC]),
        Inst::Ballot => i(U32, &[BOOL]),
        Inst::BitCount => i(S32, &[S32]),
        Inst::BitfieldExtractS32 => i(S32, &[S32, S32, S32]),
        Inst::BitfieldExtractU32 => i(U32, &[U32, S32, S32]),
        Inst::BitfieldInsert => i(S32, &[S32, S32, S32, S32]),
        Inst::BitfieldReverse => i(S32, &[S32]),
        Inst::BitwiseAnd | Inst::BitwiseExclusiveOr | Inst::BitwiseOr => i(S32, &[S32, S32]),
        Inst::BitwiseNot => i(S32, &[S32]),
        Inst::BranchIfTrue | Inst::BranchIfFalse => i(VOID, &[BOOL]),
        Inst::Call => i(SC, &[]),
        Inst::Ceiling => i(SC, &[SC, SC]),
        Inst::Clamp => i(SC, &[SC, SC, SC]),
        Inst::ClampU32 => i(U32, &[U32, U32, U32]),
        Inst::CompareEqual
        | Inst::CompareGreater
        | Inst::CompareGreaterOrEqual
        | Inst::CompareLess
        | Inst::CompareLessOrEqual
        | Inst::CompareNotEqual => i(BOOL, &[SC, SC]),
        Inst::CompareGreaterOrEqualU32
        | Inst::CompareGreaterU32
        | Inst::CompareLessOrEqualU32
        | Inst::CompareLessU32 => i(BOOL, &[U32, U32]),
        Inst::ConditionalSelect => i(SC, &[BOOL, SC, SC]),
        Inst::ConvertFP32ToFP64 => i(F64, &[F32]),
        Inst::ConvertFP64ToFP32 => i(F32, &[F64]),
        Inst::ConvertFP32ToS32 => i(S32, &[F32]),
        Inst::ConvertFP32ToU32 => i(U32, &[F32]),
        Inst::ConvertFP64ToS32 => i(S32, &[F64]),
        Inst::ConvertFP64ToU32 => i(U32, &[F64]),
        Inst::ConvertS32ToFP32 => i(F32, &[S32]),
        Inst::ConvertS32ToFP64 => i(F64, &[S32]),
        Inst::ConvertU32ToFP32 => i(F32, &[U32]),
        Inst::ConvertU32ToFP64 => i(F64, &[U32]),
        Inst::Cosine => i(SC, &[SC]),
        Inst::Ddx | Inst::Ddy => i(F32, &[F32]),
        Inst::Divide => i(SC, &[SC, SC]),
        Inst::ExponentB2 => i(SC, &[SC]),
        Inst::FindLSB | Inst::FindMSBS32 => i(S32, &[S32]),
        Inst::FindMSBU32 => i(S32, &[U32]),
        Inst::Floor => i(SC, &[SC]),
        Inst::FusedMultiplyAdd => i(SC, &[SC, SC, SC]),
        Inst::ImageLoad => i(F32, &[]),
        Inst::ImageStore => i(VOID, &[]),
        Inst::ImageAtomic => i(S32, &[]),
        Inst::IsNan => i(BOOL, &[SC]),
        Inst::Load => i(F32, &[]),
        Inst::Lod => i(F32, &[]),
        Inst::LogarithmB2 => i(SC, &[SC]),
        Inst::LogicalAnd | Inst::LogicalExclusiveOr | Inst::LogicalOr => i(BOOL, &[BOOL, BOOL]),
        Inst::LogicalNot => i(BOOL, &[BOOL]),
        Inst::Maximum | Inst::Minimum | Inst::Modulo | Inst::Multiply => i(SC, &[SC, SC]),
        Inst::MaximumU32 | Inst::MinimumU32 => i(U32, &[U32, U32]),
        Inst::MultiplyHighS32 => i(S32, &[S32, S32]),
        Inst::MultiplyHighU32 => i(U32, &[U32, U32]),
        Inst::Negate => i(SC, &[SC]),
        Inst::PackDouble2x32 => i(F64, &[U32, U32]),
        Inst::PackHalf2x16 => i(U32, &[F32, F32]),
        Inst::ReciprocalSquareRoot => i(SC, &[SC]),
        Inst::Return => i(VOID, &[U32]),
        Inst::Round => i(SC, &[SC]),
        Inst::ShiftLeft | Inst::ShiftRightS32 => i(S32, &[S32, S32]),
        Inst::ShiftRightU32 => i(U32, &[U32, S32]),
        Inst::Shuffle | Inst::ShuffleDown | Inst::ShuffleUp | Inst::ShuffleXor => {
            i(F32, &[F32, U32])
        }
        Inst::Sine => i(SC, &[SC]),
        Inst::SquareRoot => i(SC, &[SC]),
        Inst::Store => i(VOID, &[]),
        Inst::Subtract => i(SC, &[SC, SC]),
        Inst::SwizzleAdd => i(F32, &[F32, F32, S32]),
        Inst::TextureSample => i(F32, &[]),
        Inst::TextureQuerySamples => i(S32, &[S32]),
        Inst::TextureQuerySize => i(S32, &[S32, S32]),
        Inst::Truncate => i(SC, &[SC]),
        Inst::UnpackDouble2x32 => i(U32, &[F64]),
        Inst::UnpackHalf2x16 => i(F32, &[U32]),
        Inst::VectorExtract => i(SC, &[AggType::VECTOR4, S32]),
        Inst::VoteAll | Inst::VoteAllEqual | Inst::VoteAny => i(BOOL, &[BOOL]),
        _ => None,
    }
}

fn is_atomic(inst: Inst) -> bool {
    matches!(
        Inst(inst.0 & Inst::MASK),
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

fn final_type(t: AggType, inst: Inst) -> Result<AggType, String> {
    if t == SC {
        Ok(if inst.0 & Inst::FP32 != 0 {
            AggType::FP32
        } else if inst.0 & Inst::FP64 != 0 {
            AggType::FP64
        } else {
            AggType::S32
        })
    } else if t == AggType::VOID {
        Err(format!("Invalid operand for instruction {:x}.", inst.0))
    } else {
        Ok(t)
    }
}

/// `GetDestVarType`.
pub fn dest_var_type(inst: Inst) -> Result<AggType, String> {
    // an instruction without a table entry has the default (Invalid) destination type
    match info(Inst(inst.0 & Inst::MASK)) {
        Some(info) => final_type(info.dest, inst),
        None => Ok(AggType::INVALID),
    }
}

/// `GetSrcVarType`.
pub fn src_var_type(inst: Inst, index: usize) -> Result<AggType, String> {
    if matches!(
        inst,
        Inst::ImageLoad | Inst::ImageStore | Inst::ImageAtomic | Inst::Lod | Inst::TextureSample
    ) {
        return Ok(AggType::FP32);
    }
    if inst == Inst::Call || inst == Inst::Load || inst == Inst::Store || is_atomic(inst) {
        return Ok(AggType::S32);
    }
    let info = info(Inst(inst.0 & Inst::MASK))
        .ok_or_else(|| format!("no instruction info for {:x}", inst.0))?;
    let t = *info
        .src
        .get(index)
        .ok_or_else(|| format!("source {index} out of range for {:x}", inst.0))?;
    final_type(t, inst)
}

/// `IsUnary`.
pub fn is_unary(inst: Inst) -> bool {
    if inst == Inst::Copy {
        return true;
    }
    if inst == Inst::TextureSample {
        return false;
    }
    info(Inst(inst.0 & Inst::MASK)).is_some_and(|i| i.src.len() == 1)
}
