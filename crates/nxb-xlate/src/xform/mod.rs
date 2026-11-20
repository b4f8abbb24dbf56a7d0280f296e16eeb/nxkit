//! `TransformPasses` and the transforms the corpus exercises.
#![allow(missing_docs)]

pub mod geometry_to_compute;
pub mod shared_store;
pub mod shuffle;
pub mod tensor;
pub mod texture;
mod uniformity;

use crate::accessor::Accessor;
use crate::helper::Hfm;
use crate::ir::{Block, Inst, Ir, OperandType};
use crate::opt::Env;
use crate::translation::definitions::Definitions;
use crate::translation::props::ShaderStage;

type R<T> = Result<T, String>;

/// `FeatureFlags` bits the transforms read or set.
pub mod feature {
    pub const DRAW_PARAMETERS: u32 = 1 << 4;
    pub const SHUFFLE: u32 = 1 << 6;
    pub const SHARED_MEMORY: u32 = 1 << 11;
    pub const VTG_AS_COMPUTE: u32 = 1 << 13;
}

const NVN_BASE_VERTEX_WORD: i32 = 0x640 / 4;
const NVN_BASE_INSTANCE_WORD: i32 = 0x644 / 4;
const NVN_DRAW_INDEX_WORD: i32 = 0x648 / 4;

/// What the passes read from `TransformContext`.
pub struct XformCtx<'a> {
    pub stage: ShaderStage,
    pub used_features: &'a mut u32,
    pub hfm: &'a mut Hfm,
    /// the definitions of the original stage (`TransformContext.Definitions`), for the vertex-as-compute passes
    pub definitions: Option<&'a Definitions>,
    /// The immediate dominator of each block (`BasicBlock.ImmediateDominator`), for the tensor pass's divergence analysis
    pub idom: &'a [Option<usize>],
}

fn fp(i: Inst) -> Inst {
    Inst(Inst::FP32 | i.0)
}

/// `TransformPasses.RunPass`.
pub fn run_pass(ir: &mut Ir, blocks: &mut [Block], env: &mut Env, ctx: &mut XformCtx) -> R<()> {
    // on the optimised IR, before any lowering
    tensor::prepare(ir, blocks, ctx);
    if ctx.stage == ShaderStage::Vertex {
        draw_parameters(ir, blocks, env.acc, ctx.used_features)?;
    }
    if ctx.stage == ShaderStage::Fragment && env.acc.query_host_reduced_precision() {
        force_precise(ir, blocks)?;
    }
    // VectorComponentSelect: QueryHostHasVectorIndexingBug is false
    texture::run_pass(ir, blocks, env, ctx)?;
    if ctx.stage == ShaderStage::Compute && *ctx.used_features & feature::SHARED_MEMORY != 0 {
        shared_store::run_pass(ir, blocks, ctx)?;
    }
    if *ctx.used_features & feature::SHUFFLE != 0 {
        shuffle::run_pass(ir, blocks, ctx)?;
    }
    tensor::run_pass(ir, blocks, ctx)?;
    if *ctx.used_features & feature::VTG_AS_COMPUTE != 0 {
        if ctx.stage != ShaderStage::Geometry {
            return Err("unported: VertexToCompute".into());
        }
        geometry_to_compute::run_pass(ir, blocks, env, ctx)?;
    }
    Ok(())
}

/// `DrawParametersReplace` (`QueryHasConstantBufferDrawParameters` is false).
fn draw_parameters(ir: &Ir, blocks: &[Block], _acc: &Accessor, used_features: &mut u32) -> R<()> {
    for b in blocks {
        for &n in &b.ops {
            if !ir.is_op(n) {
                continue;
            }
            for s in ir.op(n).sources.iter().flatten() {
                if ir.ty(*s) == OperandType::ConstantBuffer
                    && ir.cbuf_slot(*s) == 0
                    && matches!(
                        ir.cbuf_offset(*s),
                        NVN_BASE_VERTEX_WORD | NVN_BASE_INSTANCE_WORD | NVN_DRAW_INDEX_WORD
                    )
                {
                    *used_features |= feature::DRAW_PARAMETERS;
                }
            }
        }
    }
    Ok(())
}

/// `ForcePreciseEnable`.
fn force_precise(ir: &mut Ir, blocks: &[Block]) -> R<()> {
    for b in blocks {
        for &n in &b.ops {
            if !ir.is_op(n) || ir.op(n).inst != fp(Inst::Divide) {
                continue;
            }
            let (Some(s0), Some(s1)) = (
                ir.op(n).sources.first().copied().flatten(),
                ir.op(n).sources.get(1).copied().flatten(),
            ) else {
                continue;
            };
            if ir.ty(s0) == OperandType::Constant
                && ir.as_float(s0) == 1.0
                && let Some(add) = ir.asg_operation(s1)
                && ir.op(add).inst == fp(Inst::Add)
                && let Some(a1) = ir.op(add).sources.get(1).copied().flatten()
                && ir.ty(a1) == OperandType::Constant
            {
                ir.op_mut(add).force_precise = true;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
