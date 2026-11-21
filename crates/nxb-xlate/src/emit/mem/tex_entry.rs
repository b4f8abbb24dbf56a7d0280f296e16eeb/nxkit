//! `InstEmitTexture.cs`: the per-instruction entry points.

use super::tex::*;
use super::tex_misc::*;
use super::texs::*;
use crate::emit::emitter::EmitterContext;
use crate::ir::ops::texflags;
use crate::maxwell::enums::Lod;
use crate::maxwell::insts::*;
pub(super) fn tex(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTex(ctx.curr_op.raw);
    emit_tex(
        ctx,
        texflags::NONE,
        op.dim(),
        op.lod().0,
        op.tid_b(),
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        false,
        op.dc(),
        op.aoffi(),
    )
}

pub(super) fn tex_b(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTexB(ctx.curr_op.raw);
    emit_tex(
        ctx,
        texflags::BINDLESS,
        op.dim(),
        op.lodb().0,
        0,
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        false,
        op.dc(),
        op.aoffib(),
    )
}

pub(super) fn texs(ctx: &mut EmitterContext<'_, '_>, is_f16: bool) -> Result<(), String> {
    let op = InstTexs(ctx.curr_op.raw);
    emit_texs(
        ctx,
        TexsType::Texs,
        op.tid_b(),
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        op.dest2(),
        is_f16,
    )
}

pub(super) fn tld(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTld(ctx.curr_op.raw);
    let lod = if op.lod() { Lod::Ll } else { Lod::Lz };
    emit_tex(
        ctx,
        texflags::INT_COORDS,
        op.dim(),
        lod.0,
        op.tid_b(),
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        op.ms(),
        false,
        op.toff(),
    )
}

pub(super) fn tld_b(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTldB(ctx.curr_op.raw);
    let lod = if op.lod() { Lod::Ll } else { Lod::Lz };
    emit_tex(
        ctx,
        texflags::INT_COORDS | texflags::BINDLESS,
        op.dim(),
        lod.0,
        0,
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        op.ms(),
        false,
        op.toff(),
    )
}

pub(super) fn tlds(ctx: &mut EmitterContext<'_, '_>, is_f16: bool) -> Result<(), String> {
    let op = InstTlds(ctx.curr_op.raw);
    emit_texs(
        ctx,
        TexsType::Tlds,
        op.tid_b(),
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        op.dest2(),
        is_f16,
    )
}

pub(super) fn tld4(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTld4(ctx.curr_op.raw);
    emit_tld4(
        ctx,
        op.dim(),
        op.tex_comp(),
        op.tid_b(),
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        op.toff(),
        op.dc(),
        false,
    )
}

pub(super) fn tld4_b(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTld4B(ctx.curr_op.raw);
    emit_tld4(
        ctx,
        op.dim(),
        op.tex_comp(),
        0,
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        op.toff(),
        op.dc(),
        true,
    )
}

pub(super) fn tld4s(ctx: &mut EmitterContext<'_, '_>, is_f16: bool) -> Result<(), String> {
    let op = InstTld4s(ctx.curr_op.raw);
    emit_texs(
        ctx,
        TexsType::Tld4s,
        op.tid_b(),
        4,
        op.src_a(),
        op.src_b(),
        op.dest(),
        op.dest2(),
        is_f16,
    )
}

pub(super) fn tmml(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTmml(ctx.curr_op.raw);
    emit_tmml(
        ctx,
        op.dim(),
        op.tid_b(),
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        false,
    )
}

pub(super) fn tmml_b(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTmmlB(ctx.curr_op.raw);
    emit_tmml(
        ctx,
        op.dim(),
        0,
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        true,
    )
}

pub(super) fn txd(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTxd(ctx.curr_op.raw);
    emit_txd(
        ctx,
        op.dim(),
        op.tid_b(),
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        op.toff(),
        false,
    )
}

pub(super) fn txd_b(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTxdB(ctx.curr_op.raw);
    emit_txd(
        ctx,
        op.dim(),
        0,
        op.w_mask(),
        op.src_a(),
        op.src_b(),
        op.dest(),
        op.toff(),
        true,
    )
}

pub(super) fn txq(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTxq(ctx.curr_op.raw);
    emit_txq(
        ctx,
        op.tex_query(),
        op.tid_b(),
        op.w_mask(),
        op.src_a(),
        op.dest(),
        false,
    )
}

pub(super) fn txq_b(ctx: &mut EmitterContext<'_, '_>) -> Result<(), String> {
    let op = InstTxqB(ctx.curr_op.raw);
    emit_txq(
        ctx,
        op.tex_query(),
        0,
        op.w_mask(),
        op.src_a(),
        op.dest(),
        true,
    )
}
