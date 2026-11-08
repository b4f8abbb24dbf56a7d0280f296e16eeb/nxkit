//! The setting functions of the command buffer: viewport, scissor, depth range, stencil values, blend colour and the
//! scalar settings.
//!
//! Every setting is recorded, also the ones that a host backend may not use. The backend decides what it does with
//! them, and a recording that the front end replays later still carries them.

use nxkit_nvn::ext::{ExtCode, Setting};

use super::args::{Words, span};
use super::rec::{O_IMAGE_STRIDE, O_ROW_STRIDE, emit};
use crate::nvn::abi::{AbiResult, Obj, arg_bool, guard, table};

/// The most viewports, scissors and depth ranges.
const VIEWPORTS: u32 = 16;

fn setting(name: &'static str, c: u64, s: Setting, words: &[u32]) {
    emit(name, c, ExtCode::Setting, s as u16, words);
}

fn flag(name: &'static str, c: u64, s: Setting, on: u32) {
    setting(name, c, s, &[u32::from(arg_bool(on))]);
}

/// Records the copy stride and keeps it in the command buffer for the getter.
pub(super) fn copy_stride(name: &'static str, c: u64, image: bool, v: i64) {
    guard(name, || {
        let (off, s) = if image {
            (O_IMAGE_STRIDE, Setting::CopyImageStride)
        } else {
            (O_ROW_STRIDE, Setting::CopyRowStride)
        };
        Obj(c).set_i64(off, v)?;
        setting(name, c, s, &[v as u32, (v >> 32) as u32]);
        Ok(())
    });
}

extern "C" fn set_viewport(c: u64, x: i32, y: i32, w: i32, h: i32) {
    emit(
        "nvnCommandBufferSetViewport",
        c,
        ExtCode::Viewport,
        0,
        &[
            (x as f32).to_bits(),
            (y as f32).to_bits(),
            (w as f32).to_bits(),
            (h as f32).to_bits(),
        ],
    );
}

/// Records `n` entries of `bytes` bytes each from a guest array, one record for each.
fn array(
    name: &'static str,
    c: u64,
    code: ExtCode,
    (first, n): (i32, i32),
    (data, bytes): (u64, usize),
) {
    guard(name, || -> AbiResult<()> {
        let (first, n) = span(first, n, VIEWPORTS)?;
        for k in 0..n {
            let mut w = Words::new();
            w.load(data + u64::from(k) * bytes as u64, bytes)?;
            emit(name, c, code, (first + k) as u16, w.as_slice());
        }
        Ok(())
    });
}

extern "C" fn set_viewports(c: u64, first: i32, n: i32, v: u64) {
    array(
        "nvnCommandBufferSetViewports",
        c,
        ExtCode::Viewport,
        (first, n),
        (v, 16),
    );
}

extern "C" fn set_scissor(c: u64, x: i32, y: i32, w: i32, h: i32) {
    emit(
        "nvnCommandBufferSetScissor",
        c,
        ExtCode::Scissor,
        0,
        &[x as u32, y as u32, w as u32, h as u32],
    );
}

extern "C" fn set_scissors(c: u64, first: i32, n: i32, v: u64) {
    array(
        "nvnCommandBufferSetScissors",
        c,
        ExtCode::Scissor,
        (first, n),
        (v, 16),
    );
}

extern "C" fn set_depth_range(c: u64, near: f32, far: f32) {
    emit(
        "nvnCommandBufferSetDepthRange",
        c,
        ExtCode::DepthRange,
        0,
        &[near.to_bits(), far.to_bits()],
    );
}

extern "C" fn set_depth_ranges(c: u64, first: i32, n: i32, v: u64) {
    array(
        "nvnCommandBufferSetDepthRanges",
        c,
        ExtCode::DepthRange,
        (first, n),
        (v, 8),
    );
}

/// `SetViewportSwizzles`: four swizzles for each viewport.
extern "C" fn set_viewport_swizzles(c: u64, first: i32, n: i32, v: u64) {
    array(
        "nvnCommandBufferSetViewportSwizzles",
        c,
        ExtCode::ViewportSwizzle,
        (first, n),
        (v, 16),
    );
}

extern "C" fn set_blend_color(c: u64, rgba: u64) {
    let name = "nvnCommandBufferSetBlendColor";
    guard(name, || {
        let mut w = Words::new();
        w.load(rgba, 16)?;
        setting(name, c, Setting::BlendColor, w.as_slice());
        Ok(())
    });
}

extern "C" fn set_stencil_value_mask(c: u64, face: i32, v: i32) {
    setting(
        "nvnCommandBufferSetStencilValueMask",
        c,
        Setting::StencilValueMask,
        &[face as u32, v as u32],
    );
}

extern "C" fn set_stencil_mask(c: u64, face: i32, v: i32) {
    setting(
        "nvnCommandBufferSetStencilMask",
        c,
        Setting::StencilMask,
        &[face as u32, v as u32],
    );
}

extern "C" fn set_stencil_ref(c: u64, face: i32, v: i32) {
    setting(
        "nvnCommandBufferSetStencilRef",
        c,
        Setting::StencilRef,
        &[face as u32, v as u32],
    );
}

extern "C" fn set_line_width(c: u64, w: f32) {
    setting(
        "nvnCommandBufferSetLineWidth",
        c,
        Setting::LineWidth,
        &[w.to_bits()],
    );
}

extern "C" fn set_point_size(c: u64, s: f32) {
    setting(
        "nvnCommandBufferSetPointSize",
        c,
        Setting::PointSize,
        &[s.to_bits()],
    );
}

extern "C" fn set_alpha_ref(c: u64, r: f32) {
    setting(
        "nvnCommandBufferSetAlphaRef",
        c,
        Setting::AlphaRef,
        &[r.to_bits()],
    );
}

extern "C" fn set_depth_clamp(c: u64, on: u32) {
    flag("nvnCommandBufferSetDepthClamp", c, Setting::DepthClamp, on);
}

extern "C" fn set_polygon_offset_clamp(c: u64, factor: f32, units: f32, clamp: f32) {
    setting(
        "nvnCommandBufferSetPolygonOffsetClamp",
        c,
        Setting::PolygonOffsetClamp,
        &[factor.to_bits(), units.to_bits(), clamp.to_bits()],
    );
}

extern "C" fn set_primitive_restart(c: u64, on: u32, index: i32) {
    setting(
        "nvnCommandBufferSetPrimitiveRestart",
        c,
        Setting::PrimitiveRestart,
        &[u32::from(arg_bool(on)), index as u32],
    );
}

extern "C" fn set_rasterizer_discard(c: u64, on: u32) {
    flag(
        "nvnCommandBufferSetRasterizerDiscard",
        c,
        Setting::RasterizerDiscard,
        on,
    );
}

extern "C" fn set_sample_mask(c: u64, mask: i32) {
    setting(
        "nvnCommandBufferSetSampleMask",
        c,
        Setting::SampleMask,
        &[mask as u32],
    );
}

extern "C" fn set_tiled_cache_action(c: u64, action: i32) {
    setting(
        "nvnCommandBufferSetTiledCacheAction",
        c,
        Setting::TiledCacheAction,
        &[action as u32],
    );
}

extern "C" fn set_tiled_cache_tile_size(c: u64, w: i32, h: i32) {
    setting(
        "nvnCommandBufferSetTiledCacheTileSize",
        c,
        Setting::TiledCacheTileSize,
        &[w as u32, h as u32],
    );
}

extern "C" fn set_render_enable(c: u64, on: u32) {
    flag(
        "nvnCommandBufferSetRenderEnable",
        c,
        Setting::RenderEnable,
        on,
    );
}

extern "C" fn set_render_enable_conditional(c: u64, mode: i32, addr: u64) {
    setting(
        "nvnCommandBufferSetRenderEnableConditional",
        c,
        Setting::RenderEnableConditional,
        &[mode as u32, addr as u32, (addr >> 32) as u32],
    );
}

extern "C" fn set_conservative_raster_enable(c: u64, on: u32) {
    flag(
        "nvnCommandBufferSetConservativeRasterEnable",
        c,
        Setting::ConservativeRaster,
        on,
    );
}

extern "C" fn set_copy_row_stride(c: u64, v: i64) {
    copy_stride("nvnCommandBufferSetCopyRowStride", c, false, v);
}

extern "C" fn set_copy_image_stride(c: u64, v: i64) {
    copy_stride("nvnCommandBufferSetCopyImageStride", c, true, v);
}

pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnCommandBufferSetViewport" => set_viewport as extern "C" fn(u64, i32, i32, i32, i32),
        "nvnCommandBufferSetViewports" => set_viewports as extern "C" fn(u64, i32, i32, u64),
        "nvnCommandBufferSetScissor" => set_scissor as extern "C" fn(u64, i32, i32, i32, i32),
        "nvnCommandBufferSetScissors" => set_scissors as extern "C" fn(u64, i32, i32, u64),
        "nvnCommandBufferSetDepthRange" => set_depth_range as extern "C" fn(u64, f32, f32),
        "nvnCommandBufferSetDepthRanges" => set_depth_ranges as extern "C" fn(u64, i32, i32, u64),
        "nvnCommandBufferSetViewportSwizzles" => set_viewport_swizzles as extern "C" fn(u64, i32, i32, u64),
        "nvnCommandBufferSetBlendColor" => set_blend_color as extern "C" fn(u64, u64),
        "nvnCommandBufferSetStencilValueMask" => set_stencil_value_mask as extern "C" fn(u64, i32, i32),
        "nvnCommandBufferSetStencilMask" => set_stencil_mask as extern "C" fn(u64, i32, i32),
        "nvnCommandBufferSetStencilRef" => set_stencil_ref as extern "C" fn(u64, i32, i32),
        "nvnCommandBufferSetLineWidth" => set_line_width as extern "C" fn(u64, f32),
        "nvnCommandBufferSetPointSize" => set_point_size as extern "C" fn(u64, f32),
        "nvnCommandBufferSetAlphaRef" => set_alpha_ref as extern "C" fn(u64, f32),
        "nvnCommandBufferSetDepthClamp" => set_depth_clamp as extern "C" fn(u64, u32),
        "nvnCommandBufferSetPolygonOffsetClamp" => set_polygon_offset_clamp as extern "C" fn(u64, f32, f32, f32),
        "nvnCommandBufferSetPrimitiveRestart" => set_primitive_restart as extern "C" fn(u64, u32, i32),
        "nvnCommandBufferSetRasterizerDiscard" => set_rasterizer_discard as extern "C" fn(u64, u32),
        "nvnCommandBufferSetSampleMask" => set_sample_mask as extern "C" fn(u64, i32),
        "nvnCommandBufferSetTiledCacheAction" => set_tiled_cache_action as extern "C" fn(u64, i32),
        "nvnCommandBufferSetTiledCacheTileSize" => set_tiled_cache_tile_size as extern "C" fn(u64, i32, i32),
        "nvnCommandBufferSetRenderEnable" => set_render_enable as extern "C" fn(u64, u32),
        "nvnCommandBufferSetRenderEnableConditional" => set_render_enable_conditional as extern "C" fn(u64, i32, u64),
        "nvnCommandBufferSetConservativeRasterEnable" => set_conservative_raster_enable as extern "C" fn(u64, u32),
        "nvnCommandBufferSetCopyRowStride" => set_copy_row_stride as extern "C" fn(u64, i64),
        "nvnCommandBufferSetCopyImageStride" => set_copy_image_stride as extern "C" fn(u64, i64),
    ]
}
