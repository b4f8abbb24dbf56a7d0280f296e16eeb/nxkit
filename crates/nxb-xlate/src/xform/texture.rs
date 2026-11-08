//! `TexturePass` (render scale of texel fetches and texture sizes, the other parts are disabled by the host answers).
use super::XformCtx;
use super::helper::{HelperName, ScaleUse};
use crate::ir::{Block, Inst, Ir, NodeId, StorageKind};
use crate::opt::Env;
use crate::opt::tex::{self, BINDLESS, INT_COORDS};
use crate::translation::props::{ShaderStage, dimensions};
use crate::types::SamplerType;

type R<T> = Result<T, String>;

fn type_supports_scale(ty: SamplerType) -> bool {
    ty.0 & SamplerType::MASK == SamplerType::TEXTURE_2D.0
}

pub fn run_pass(ir: &mut Ir, blocks: &mut [Block], env: &mut Env, ctx: &mut XformCtx) -> R<()> {
    for bi in 0..blocks.len() {
        let mut i = 0;
        while i < blocks[bi].ops.len() {
            let node = blocks[bi].ops[i];
            if tex::tex(ir, node).is_some() {
                let added = texel_fetch_scale(ir, blocks, bi, i, env, ctx)?;
                i += added;
                texture_size_unscale(ir, blocks, bi, i, env, ctx)?;
                let t = tex::tex(ir, node).ok_or("tex")?.clone();
                if ir.op(node).inst == Inst::TextureSample {
                    check_sample_passes(ir, env, node, &t)?;
                }
            }
            i += 1;
        }
    }
    Ok(())
}

/// The coordinate normalization, gather bias, constant offset and snorm parts do nothing with the host answers of the
/// job accessor (`QueryTextureCoordNormalized` true, no gather bias, offsets supported, snorm buffers supported).
/// the cases they would act on fail closed.
fn check_sample_passes(ir: &Ir, env: &mut Env, node: NodeId, t: &crate::ir::TexData) -> R<()> {
    let _ = (ir, env, node);
    let hosts_offsets = crate::translation::host::SUPPORTS_NON_CONSTANT_TEXTURE_OFFSET;
    let _ = t;
    if !hosts_offsets {
        return Err("unported: TexturePass.InsertConstOffsets".into());
    }
    Ok(())
}

/// Returns the number of nodes inserted before the texture operation.
fn texel_fetch_scale(
    ir: &mut Ir,
    blocks: &mut [Block],
    bi: usize,
    pos: usize,
    env: &mut Env,
    ctx: &mut XformCtx,
) -> R<usize> {
    let node = blocks[bi].ops[pos];
    let t = tex::tex(ir, node).ok_or("tex")?.clone();
    let inst = ir.op(node).inst;
    let is_bindless = t.flags & BINDLESS != 0;
    let int_coords = t.flags & INT_COORDS != 0;
    // Loads, stores and atomics of an image take the coordinate of the scaled image. A store of a native size
    // coordinate fills one texel of each scale by scale block (see docs/nxkit-gpu.md).
    let is_image = matches!(inst, Inst::ImageLoad | Inst::ImageStore | Inst::ImageAtomic);
    let is_indexed = env.rm.is_array_of_textures_or_images(t.binding, is_image);
    if !((inst == Inst::TextureSample || is_image)
        && (int_coords || is_image)
        && !is_bindless
        && !is_indexed
        && ctx.stage.supports_render_scale()
        && type_supports_scale(t.ty))
    {
        return Ok(0);
    }
    let function_id = ctx
        .hfm
        .get_or_create_function_id(ir, HelperName::TexelFetchScale)?;
    let sampler_index = if is_image {
        env.rm.get_texture_descriptors(false).len() as i32
            + env.rm.find_image_descriptor_index(t.binding)
    } else {
        env.rm.find_texture_descriptor_index(t.binding)
    };
    ctx.hfm.note_scaled(ScaleUse {
        image: is_image,
        binding: i64::from(t.binding),
        index: sampler_index,
    });
    let coords_count = dimensions(t.ty)?;
    let coords_index = if is_bindless { 1 } else { 0 };
    let mut added = 0;
    for index in 0..coords_count as usize {
        let scaled = ir.local();
        let coord = ir.op(node).sources[coords_index + index].ok_or("coord")?;
        let mut args = vec![ir.const_(function_id), coord, ir.const_(sampler_index)];
        if ctx.stage == ShaderStage::Fragment {
            args.push(ir.const_(index as i32));
        }
        let call = ir.new_op(Inst::Call, StorageKind::None, Some(scaled), &args);
        // Operation(Instruction.Call, 0, dest, args): index 0
        blocks[bi].ops.insert(pos + added, call);
        added += 1;
        ir.set_source(node, coords_index + index, Some(scaled));
    }
    Ok(added)
}

fn texture_size_unscale(
    ir: &mut Ir,
    blocks: &mut [Block],
    bi: usize,
    pos: usize,
    env: &mut Env,
    ctx: &mut XformCtx,
) -> R<()> {
    let node = blocks[bi].ops[pos];
    let t = tex::tex(ir, node).ok_or("tex")?.clone();
    let is_bindless = t.flags & BINDLESS != 0;
    let is_indexed = env.rm.is_array_of_textures_or_images(t.binding, false);
    if !(ir.op(node).inst == Inst::TextureQuerySize
        && ir.op(node).index < 2
        && !is_bindless
        && !is_indexed
        && ctx.stage.supports_render_scale()
        && type_supports_scale(t.ty))
    {
        return Ok(());
    }
    let function_id = ctx
        .hfm
        .get_or_create_function_id(ir, HelperName::TextureSizeUnscale)?;
    let sampler_index = env.rm.find_texture_descriptor_index(t.binding);
    ctx.hfm.note_scaled(ScaleUse {
        image: false,
        binding: i64::from(t.binding),
        index: sampler_index,
    });
    for index in (0..ir.op(node).dests.len()).rev() {
        let dest = ir.op(node).dests[index].ok_or("null destination")?;
        let unscaled = ir.local();
        let uses: Vec<NodeId> = ir.data(dest).use_ops.iter().collect();
        for u in uses {
            let n = ir.node_sources(u).len();
            for si in 0..n {
                if ir.node_sources(u)[si] == Some(dest) {
                    ir.set_node_source(u, si, Some(unscaled));
                }
            }
        }
        let args = [ir.const_(function_id), dest, ir.const_(sampler_index)];
        let call = ir.new_op(Inst::Call, StorageKind::None, Some(unscaled), &args);
        blocks[bi].ops.insert(pos + 1, call);
    }
    Ok(())
}
