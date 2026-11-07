//! The transfer functions of the command buffer: render targets, clears, copies, downsample, discards, barrier,
//! sync, counters, push constants and inline uniform data.
//!
//! The textures of these calls are named by the GPU address of their texture, which the seam gives.

use nxkit_nvn::ext::ExtCode;
use nxkit_nvn::frame::MAX_COLOR;
use nxkit_nvn::record::Record;

use super::args::{Words, hi, lo, span};
use super::rec::{emit, emit_rec};
use super::seam::seam;
use crate::nvn::abi::{AbiResult, Obj, arg_bool, guard, log_once, other, table};
use crate::nvn::ctx::gpu;

/// The slot of the depth and stencil view in a `TargetView` record.
const DEPTH_VIEW: u16 = 8;
/// The bytes of a `NVNtextureView` and of a `NVNcopyRegion`.
const VIEW_BYTES: usize = 16;
const REGION_BYTES: usize = 24;
/// The bytes of push constants in one record (`offset` and 24 words) and of inline uniform data in one record.
const PUSH_CHUNK: u64 = 96;
const UNIFORM_CHUNK: u64 = 88;

fn texture(tex: u64) -> AbiResult<u64> {
    seam().texture_address(tex)
}

extern "C" fn set_render_targets(c: u64, n: i32, colors: u64, views: u64, depth: u64, depth_view: u64) {
    let name = "nvnCommandBufferSetRenderTargets";
    guard(name, || {
        let (_, n) = span(0, n, MAX_COLOR as u32)?;
        let mut color = [0u64; MAX_COLOR];
        let mut view = [0u64; MAX_COLOR];
        for i in 0..n as usize {
            let tex = Obj(colors + i as u64 * 8).u64(0)?;
            if tex != 0 {
                color[i] = texture(tex)?;
            }
            if views != 0 {
                view[i] = Obj(views + i as u64 * 8).u64(0)?;
            }
        }
        let depth_addr = if depth != 0 { texture(depth)? } else { 0 };
        emit_rec(name, c, Record::SetTargets { color, depth: depth_addr });
        for (i, v) in view.iter().enumerate().take(n as usize) {
            if *v != 0 {
                target_view(name, c, i as u16, *v)?;
            }
        }
        if depth_view != 0 {
            target_view(name, c, DEPTH_VIEW, depth_view)?;
        }
        Ok(())
    });
}

fn target_view(name: &'static str, c: u64, slot: u16, view: u64) -> AbiResult<()> {
    let mut w = Words::new();
    w.load(view, VIEW_BYTES)?;
    emit(name, c, ExtCode::TargetView, slot, w.as_slice());
    Ok(())
}

fn clear_color(name: &'static str, c: u64, index: i32, rgba: u64, mask: i32, kind: u16) {
    guard(name, || {
        let (slot, _) = span(index, 1, MAX_COLOR as u32)?;
        let mut w = Words::new();
        w.u32(mask as u32)?.load(rgba, 16)?;
        emit(name, c, ExtCode::ClearColor, slot as u16 | kind << 8, w.as_slice());
        Ok(())
    });
}

extern "C" fn clear_color_f(c: u64, index: i32, rgba: u64, mask: i32) {
    clear_color("nvnCommandBufferClearColor", c, index, rgba, mask, 0);
}

extern "C" fn clear_color_i(c: u64, index: i32, rgba: u64, mask: i32) {
    clear_color("nvnCommandBufferClearColori", c, index, rgba, mask, 1);
}

extern "C" fn clear_color_ui(c: u64, index: i32, rgba: u64, mask: i32) {
    clear_color("nvnCommandBufferClearColorui", c, index, rgba, mask, 2);
}

extern "C" fn clear_depth_stencil(c: u64, depth: f32, depth_mask: u32, stencil: i32, stencil_mask: i32) {
    emit(
        "nvnCommandBufferClearDepthStencil",
        c,
        ExtCode::ClearDepthStencil,
        0,
        &[depth.to_bits(), u32::from(arg_bool(depth_mask)), stencil as u32, stencil_mask as u32],
    );
}

extern "C" fn clear_buffer(c: u64, addr: u64, n: u64, value: u32) {
    emit(
        "nvnCommandBufferClearBuffer",
        c,
        ExtCode::ClearBuffer,
        0,
        &[lo(addr), hi(addr), lo(n), hi(n), value],
    );
}

fn clear_texture(name: &'static str, c: u64, a: [u64; 4], mask: i32, kind: u16) {
    let [tex, view, region, rgba] = a;
    guard(name, || {
        let mut w = Words::new();
        w.u64(texture(tex)?)?.u32(mask as u32)?.load(rgba, 16)?;
        if view != 0 {
            w.load(view, VIEW_BYTES)?;
        }
        w.load(non_null(region, "copy region")?, REGION_BYTES)?;
        emit(
            name,
            c,
            ExtCode::ClearTexture,
            kind | u16::from(view != 0) << 8,
            w.as_slice(),
        );
        Ok(())
    });
}

fn non_null(p: u64, what: &str) -> AbiResult<u64> {
    if p == 0 {
        Err(other(format!("the {what} is null")))
    } else {
        Ok(p)
    }
}

extern "C" fn clear_texture_f(c: u64, tex: u64, view: u64, region: u64, rgba: u64, mask: i32) {
    clear_texture("nvnCommandBufferClearTexture", c, [tex, view, region, rgba], mask, 0);
}

extern "C" fn clear_texture_ui(c: u64, tex: u64, view: u64, region: u64, rgba: u64, mask: i32) {
    clear_texture("nvnCommandBufferClearTextureui", c, [tex, view, region, rgba], mask, 2);
}

extern "C" fn copy_buffer_to_buffer(c: u64, src: u64, dst: u64, n: u64, flags: i32) {
    let name = "nvnCommandBufferCopyBufferToBuffer";
    guard(name, || {
        let mut w = Words::new();
        w.u64(src)?.u64(dst)?.u64(n)?.u32(flags as u32)?;
        emit(name, c, ExtCode::CopyBufferToBuffer, 0, w.as_slice());
        Ok(())
    });
}

fn copy_with_buffer(
    name: &'static str,
    code: ExtCode,
    c: u64,
    (tex, buf): (u64, u64),
    (view, region): (u64, u64),
    flags: i32,
) {
    guard(name, || {
        let mut w = Words::new();
        w.u64(texture(tex)?)?.u64(buf)?.u32(flags as u32)?;
        if view != 0 {
            w.load(view, VIEW_BYTES)?;
        }
        w.load(non_null(region, "copy region")?, REGION_BYTES)?;
        emit(name, c, code, u16::from(view != 0), w.as_slice());
        Ok(())
    });
}

extern "C" fn copy_buffer_to_texture(c: u64, src: u64, tex: u64, view: u64, region: u64, flags: i32) {
    copy_with_buffer(
        "nvnCommandBufferCopyBufferToTexture",
        ExtCode::CopyBufferToTexture,
        c,
        (tex, src),
        (view, region),
        flags,
    );
}

extern "C" fn copy_texture_to_buffer(c: u64, tex: u64, view: u64, region: u64, dst: u64, flags: i32) {
    copy_with_buffer(
        "nvnCommandBufferCopyTextureToBuffer",
        ExtCode::CopyTextureToBuffer,
        c,
        (tex, dst),
        (view, region),
        flags,
    );
}

extern "C" fn copy_texture_to_texture(
    c: u64,
    src: u64,
    src_view: u64,
    src_region: u64,
    dst: u64,
    dst_view: u64,
    dst_region: u64,
    flags: i32,
) {
    let name = "nvnCommandBufferCopyTextureToTexture";
    guard(name, || {
        let mut w = Words::new();
        w.u64(texture(src)?)?.u64(texture(dst)?)?.u32(flags as u32)?;
        if src_view != 0 {
            w.load(src_view, VIEW_BYTES)?;
        }
        w.load(non_null(src_region, "source region")?, REGION_BYTES)?;
        if dst_view != 0 {
            w.load(dst_view, VIEW_BYTES)?;
        }
        w.load(non_null(dst_region, "destination region")?, REGION_BYTES)?;
        let arg = u16::from(src_view != 0) | u16::from(dst_view != 0) << 1;
        emit(name, c, ExtCode::CopyTextureToTexture, arg, w.as_slice());
        Ok(())
    });
}

fn downsample(name: &'static str, c: u64, (src, dst): (u64, u64), (sv, dv): (u64, u64)) {
    guard(name, || {
        let mut w = Words::new();
        w.u64(texture(src)?)?.u64(texture(dst)?)?;
        if sv != 0 {
            w.load(sv, VIEW_BYTES)?;
        }
        if dv != 0 {
            w.load(dv, VIEW_BYTES)?;
        }
        let arg = u16::from(sv != 0) | u16::from(dv != 0) << 1;
        emit(name, c, ExtCode::DownsampleView, arg, w.as_slice());
        Ok(())
    });
}

extern "C" fn downsample_plain(c: u64, src: u64, dst: u64) {
    downsample("nvnCommandBufferDownsample", c, (src, dst), (0, 0));
}

extern "C" fn downsample_view(c: u64, src: u64, sv: u64, dst: u64, dv: u64) {
    downsample("nvnCommandBufferDownsampleTextureView", c, (src, dst), (sv, dv));
}

extern "C" fn discard_color(c: u64, index: i32) {
    let name = "nvnCommandBufferDiscardColor";
    guard(name, || {
        let (i, _) = span(index, 1, MAX_COLOR as u32)?;
        emit_rec(name, c, Record::Discard { color: 1 << i, depth: false });
        Ok(())
    });
}

extern "C" fn discard_depth_stencil(c: u64) {
    emit_rec(
        "nvnCommandBufferDiscardDepthStencil",
        c,
        Record::Discard { color: 0, depth: true },
    );
}

extern "C" fn barrier(c: u64, bits: i32) {
    emit_rec("nvnCommandBufferBarrier", c, Record::Barrier(bits as u32 & 0xff));
}

extern "C" fn fence_sync(c: u64, sync: u64, cond: i32, flags: i32) {
    emit(
        "nvnCommandBufferFenceSync",
        c,
        ExtCode::FenceSync,
        0,
        &[lo(sync), hi(sync), cond as u32, flags as u32],
    );
}

extern "C" fn signal_event(c: u64, event: u64, mode: i32, location: i32, flags: i32, value: u32) {
    emit(
        "nvnCommandBufferSignalEvent",
        c,
        ExtCode::SignalEvent,
        0,
        &[lo(event), hi(event), mode as u32, location as u32, flags as u32, value],
    );
}

/// The host clock for a report. It is read when the command is recorded.
fn now(name: &str) -> u64 {
    (gpu(name).settings().clock_ns)()
}

extern "C" fn report_value(c: u64, value: u32, addr: u64) {
    let name = "nvnCommandBufferReportValue";
    let t = now(name);
    emit(
        name,
        c,
        ExtCode::ReportValue,
        0,
        &[value, lo(addr), hi(addr), lo(t), hi(t)],
    );
}

extern "C" fn report_counter(c: u64, kind: i32, addr: u64) {
    let name = "nvnCommandBufferReportCounter";
    let t = now(name);
    emit(
        name,
        c,
        ExtCode::ReportCounter,
        0,
        &[kind as u32, lo(addr), hi(addr), lo(t), hi(t)],
    );
}

extern "C" fn reset_counter(c: u64, kind: i32) {
    emit("nvnCommandBufferResetCounter", c, ExtCode::ResetCounter, 0, &[kind as u32]);
}

/// Push constants in chunks. `compute` tells which stage kind they are for.
fn push(name: &'static str, c: u64, compute: bool, a: [u64; 4]) {
    let [w0, off, n, data] = a;
    guard(name, || {
        if w0 != 0 {
            log_once(name, format!("the first argument {w0:#x} is not used"));
        }
        if n == 0 {
            return Ok(());
        }
        let data = non_null(data, "push constant data")?;
        if off.checked_add(n).is_none_or(|end| end > u64::from(u32::MAX)) {
            return Err(other("the push constants do not fit"));
        }
        let mut done = 0;
        while done < n {
            let len = PUSH_CHUNK.min(n - done);
            let mut w = Words::new();
            w.u32((off + done) as u32)?.load(data + done, len as usize)?;
            let arg = u16::from(compute) | (len as u16) << 1;
            emit(name, c, ExtCode::Push, arg, w.as_slice());
            done += len;
        }
        Ok(())
    });
}

extern "C" fn push_graphics_constants(c: u64, w: u64, off: u64, n: u64, data: u64) {
    push("nvnCommandBufferPushGraphicsConstants", c, false, [w, off, n, data]);
}

extern "C" fn push_compute_constants(c: u64, w: u64, off: u64, n: u64, data: u64) {
    push("nvnCommandBufferPushComputeConstants", c, true, [w, off, n, data]);
}

extern "C" fn update_uniform_buffer(c: u64, buf: u64, size: u64, off: u64, n: u64, data: u64) {
    let name = "nvnCommandBufferUpdateUniformBuffer";
    guard(name, || {
        if n == 0 {
            return Ok(());
        }
        let data = non_null(data, "uniform data")?;
        let mut done = 0;
        while done < n {
            let len = UNIFORM_CHUNK.min(n - done);
            let mut w = Words::new();
            w.u64(buf)?.u64(size)?.u64(off + done)?.load(data + done, len as usize)?;
            emit(name, c, ExtCode::UpdateUniform, len as u16, w.as_slice());
            done += len;
        }
        Ok(())
    });
}

pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnCommandBufferSetRenderTargets" => set_render_targets as extern "C" fn(u64, i32, u64, u64, u64, u64),
        "nvnCommandBufferClearColor" => clear_color_f as extern "C" fn(u64, i32, u64, i32),
        "nvnCommandBufferClearColori" => clear_color_i as extern "C" fn(u64, i32, u64, i32),
        "nvnCommandBufferClearColorui" => clear_color_ui as extern "C" fn(u64, i32, u64, i32),
        "nvnCommandBufferClearDepthStencil" => clear_depth_stencil as extern "C" fn(u64, f32, u32, i32, i32),
        "nvnCommandBufferClearBuffer" => clear_buffer as extern "C" fn(u64, u64, u64, u32),
        "nvnCommandBufferClearTexture" => clear_texture_f as extern "C" fn(u64, u64, u64, u64, u64, i32),
        "nvnCommandBufferClearTextureui" => clear_texture_ui as extern "C" fn(u64, u64, u64, u64, u64, i32),
        "nvnCommandBufferCopyBufferToBuffer" => copy_buffer_to_buffer as extern "C" fn(u64, u64, u64, u64, i32),
        "nvnCommandBufferCopyBufferToTexture" => copy_buffer_to_texture as extern "C" fn(u64, u64, u64, u64, u64, i32),
        "nvnCommandBufferCopyTextureToBuffer" => copy_texture_to_buffer as extern "C" fn(u64, u64, u64, u64, u64, i32),
        "nvnCommandBufferCopyTextureToTexture" => copy_texture_to_texture as extern "C" fn(u64, u64, u64, u64, u64, u64, u64, i32),
        "nvnCommandBufferDownsample" => downsample_plain as extern "C" fn(u64, u64, u64),
        "nvnCommandBufferDownsampleTextureView" => downsample_view as extern "C" fn(u64, u64, u64, u64, u64),
        "nvnCommandBufferDiscardColor" => discard_color as extern "C" fn(u64, i32),
        "nvnCommandBufferDiscardDepthStencil" => discard_depth_stencil as extern "C" fn(u64),
        "nvnCommandBufferBarrier" => barrier as extern "C" fn(u64, i32),
        "nvnCommandBufferFenceSync" => fence_sync as extern "C" fn(u64, u64, i32, i32),
        "nvnCommandBufferSignalEvent" => signal_event as extern "C" fn(u64, u64, i32, i32, i32, u32),
        "nvnCommandBufferReportValue" => report_value as extern "C" fn(u64, u32, u64),
        "nvnCommandBufferReportCounter" => report_counter as extern "C" fn(u64, i32, u64),
        "nvnCommandBufferResetCounter" => reset_counter as extern "C" fn(u64, i32),
        "nvnCommandBufferPushGraphicsConstants" => push_graphics_constants as extern "C" fn(u64, u64, u64, u64, u64),
        "nvnCommandBufferPushComputeConstants" => push_compute_constants as extern "C" fn(u64, u64, u64, u64, u64),
        "nvnCommandBufferUpdateUniformBuffer" => update_uniform_buffer as extern "C" fn(u64, u64, u64, u64, u64, u64),
    ]
}
