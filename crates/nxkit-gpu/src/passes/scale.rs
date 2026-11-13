//! Scaled and mirrored `nvnCommandBufferCopyTextureToTexture` regions.
//!
//! The copy stretches the source region onto the destination region. The copy flags pick the filter: with
//! `NVN_COPY_FLAGS_LINEAR_FILTER_BIT` the destination texel takes a bilinear mix of the four nearest source texels,
//! without it the nearest texel. A host kernel does it, one dispatch for each layer, between two views of one level
//! and layer of the textures. The kernel reads and writes with `read` and `write`, so the region edges are exact and
//! no sampler state is involved. `MIRROR_X` and `MIRROR_Y` reverse the source traversal within the region.
//!
//! - Mapping: the centre of destination texel `d` lies at `(d + 0.5) * src_size / dst_size` in source texels. The
//!   nearest taker floors it (integer arithmetic, `nearest` is its CPU reference). The linear one interpolates between the texels
//!   around `pos - 0.5` and clamps the taps to the source region (`taps` is its CPU reference), so the edge texels repeat.
//!   A mirrored axis maps `dst_size - 1 - d` instead of `d`, before either filter runs.
//! - Formats: float, unorm, snorm and sRGB colour formats go through `texture2d<float>`, so the value is the one a
//!   shader sees (an sRGB source is filtered in linear light). Unsigned and signed integer formats take the nearest
//!   texel, whatever the flag says, since a mix of codes means nothing. Depth, stencil and compressed formats are
//!   refused, as are 3D textures. Both textures must be the same format (the caller checks), the source must be
//!   readable by a shader and the destination writable by one (`shader_write`, which a render target of an exact
//!   format has). A destination without it is refused with a reason and counted as `skip.copy`.
//! - Order: the kernel runs in the copy pass in command order between two barriers. The pass table has textures
//!   slots for it (`Tables::copy`).

use nxkit_metal::{
    ComputeCommand, ComputePass, SampleType, Scratch, Texture, TextureDesc, TextureKind,
    TextureRegion, ViewDesc,
};
use nxkit_nvn::diag::Reason;

use super::copy::copy_skip;
use crate::draws::Shared;
use crate::stats::Skip;

/// The bit of the copy flags that asks for linear filtering (`NVN_COPY_FLAGS_LINEAR_FILTER_BIT`).
pub(crate) const LINEAR_FILTER_BIT: u32 = 1;
/// The copy flags that reverse source traversal along each axis.
const MIRROR_X_BIT: u32 = 4;
const MIRROR_Y_BIT: u32 = 8;

/// True when a plain blit cannot preserve the copy's region mapping.
pub(super) fn needs_kernel(src: &TextureRegion, dst: &TextureRegion, flags: u32) -> bool {
    src.size != dst.size || flags & (MIRROR_X_BIT | MIRROR_Y_BIT) != 0
}

/// True when two textures over guest memory share real bytes. Each span is its real base and byte length.
/// Private textures have no span here, even when their guest storage overlaps.
pub(super) fn in_place_overlaps(src: Option<(u64, u64)>, dst: Option<(u64, u64)>) -> bool {
    let (Some((s, sn)), Some((d, dn))) = (src, dst) else {
        return false;
    };
    sn != 0 && dn != 0 && if s <= d { d - s < sn } else { s - d < dn }
}

/// The kernels, one for each sample type. The arithmetic is the one of [`nearest`] and [`taps`].
const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct params { uint2 so; uint2 ss; uint2 dof; uint2 ds; uint linear; uint mirror_x; uint mirror_y; };
static uint2 sample_at(uint2 gid, constant params &p) {
    return uint2(p.mirror_x ? p.ds.x - 1 - gid.x : gid.x,
                 p.mirror_y ? p.ds.y - 1 - gid.y : gid.y);
}
static uint near_at(uint d, uint s, uint n) {
    return min(uint(((ulong(d) * 2 + 1) * ulong(s)) / (ulong(n) * 2)), s - 1);
}
static void taps(uint d, uint s, uint n, thread uint &a, thread uint &b, thread float &t) {
    float f = (float(d) + 0.5) * float(s) / float(n) - 0.5;
    float fl = floor(f);
    t = f - fl;
    int last = int(s) - 1;
    a = uint(clamp(int(fl), 0, last));
    b = uint(clamp(int(fl) + 1, 0, last));
}
kernel void nxkit_tcopy_float(texture2d<float, access::read> src [[texture(0)]],
                              texture2d<float, access::write> dst [[texture(1)]],
                              constant params &p [[buffer(0)]],
                              uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= p.ds.x || gid.y >= p.ds.y) { return; }
    uint2 d = sample_at(gid, p);
    float4 v;
    if (p.linear == 0) {
        uint2 at = uint2(near_at(d.x, p.ss.x, p.ds.x), near_at(d.y, p.ss.y, p.ds.y));
        v = src.read(p.so + at);
    } else {
        uint x0, x1, y0, y1;
        float tx, ty;
        taps(d.x, p.ss.x, p.ds.x, x0, x1, tx);
        taps(d.y, p.ss.y, p.ds.y, y0, y1, ty);
        float4 a = mix(src.read(p.so + uint2(x0, y0)), src.read(p.so + uint2(x1, y0)), tx);
        float4 b = mix(src.read(p.so + uint2(x0, y1)), src.read(p.so + uint2(x1, y1)), tx);
        v = mix(a, b, ty);
    }
    dst.write(v, p.dof + gid);
}
kernel void nxkit_tcopy_uint(texture2d<uint, access::read> src [[texture(0)]],
                             texture2d<uint, access::write> dst [[texture(1)]],
                             constant params &p [[buffer(0)]],
                             uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= p.ds.x || gid.y >= p.ds.y) { return; }
    uint2 d = sample_at(gid, p);
    uint2 at = uint2(near_at(d.x, p.ss.x, p.ds.x), near_at(d.y, p.ss.y, p.ds.y));
    dst.write(src.read(p.so + at), p.dof + gid);
}
kernel void nxkit_tcopy_sint(texture2d<int, access::read> src [[texture(0)]],
                             texture2d<int, access::write> dst [[texture(1)]],
                             constant params &p [[buffer(0)]],
                             uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= p.ds.x || gid.y >= p.ds.y) { return; }
    uint2 d = sample_at(gid, p);
    uint2 at = uint2(near_at(d.x, p.ss.x, p.ds.x), near_at(d.y, p.ss.y, p.ds.y));
    dst.write(src.read(p.so + at), p.dof + gid);
}
"#;

/// The reference of the kernel's mirrored destination-local sample coordinate. `d` is inside `dst`.
#[cfg(test)]
fn sample_at(d: u32, dst: u32, mirror: bool) -> u32 {
    if mirror { dst - 1 - d } else { d }
}

/// The reference of the kernel's mapping, to test it on the CPU: the source texel that destination texel `d` takes without filtering, for a source of `src` texels stretched onto
/// `dst` texels: `floor((d + 0.5) * src / dst)`, at most `src - 1`.
#[cfg(test)]
pub(crate) fn nearest(d: u32, src: u32, dst: u32) -> u32 {
    if src == 0 || dst == 0 {
        return 0;
    }
    let at = ((u64::from(d) * 2 + 1) * u64::from(src)) / (u64::from(dst) * 2);
    at.min(u64::from(src) - 1) as u32
}

/// The two source texels and the weight of the second that destination texel `d` mixes with linear filtering. The
/// centre is `(d + 0.5) * src / dst - 0.5`; both texels are clamped to `0..src`.
#[cfg(test)]
pub(crate) fn taps(d: u32, src: u32, dst: u32) -> (u32, u32, f32) {
    if src == 0 || dst == 0 {
        return (0, 0, 0.0);
    }
    let f = (d as f32 + 0.5) * src as f32 / dst as f32 - 0.5;
    let fl = f.floor();
    let last = src as i64 - 1;
    let at = |i: i64| i.clamp(0, last) as u32;
    (at(fl as i64), at(fl as i64 + 1), f - fl)
}

/// The bytes of the kernel parameters: the source origin and size, the destination origin and size (x and y of
/// each), the filter and the mirror axes.
fn params(src: &TextureRegion, dst: &TextureRegion, linear: bool, flags: u32) -> [u8; 48] {
    let words = [
        src.origin[0],
        src.origin[1],
        src.size[0],
        src.size[1],
        dst.origin[0],
        dst.origin[1],
        dst.size[0],
        dst.size[1],
        u32::from(linear),
        u32::from(flags & MIRROR_X_BIT != 0),
        u32::from(flags & MIRROR_Y_BIT != 0),
        0,
    ];
    let mut b = [0u8; 48];
    for (i, w) in words.iter().enumerate() {
        b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    b
}

/// Why a pair of textures cannot take a scaled or mirrored copy, or `None` when it can.
fn refuse(s: &TextureDesc, d: &TextureDesc) -> Option<&'static str> {
    let fmt = d.format;
    if s.kind == TextureKind::D3 || d.kind == TextureKind::D3 {
        Some("a 3D texture")
    } else if fmt.is_depth() || fmt.has_stencil() {
        Some("a depth or stencil format")
    } else if fmt.is_compressed() {
        Some("a compressed format")
    } else if !s.shader_read {
        Some("a source a shader cannot read")
    } else if !d.shader_write {
        Some("a destination a shader cannot write")
    } else {
        None
    }
}

/// Encodes the stretch of `src_box` of `src` onto `dst_box` of `dst` for `count` layers into the copy pass `cp`. The
/// regions are already scaled by the render factor and checked against the textures. `flags` are the copy flags.
pub(super) fn encode(
    sh: &Shared<'_>,
    cp: &mut ComputePass<'_>,
    scratch: &Scratch,
    src: &Texture,
    dst: &Texture,
    (src_box, dst_box, count): (TextureRegion, TextureRegion, u32),
    flags: u32,
) -> Result<(), Skip> {
    let (sd, dd) = (src.desc(), dst.desc());
    let what = || {
        format!(
            "{:?} {}x{} {:?} -> {:?} {}x{} {:?}",
            sd.format,
            src_box.size[0],
            src_box.size[1],
            src_box.origin,
            dd.format,
            dst_box.size[0],
            dst_box.size[1],
            dst_box.origin
        )
    };
    if let Some(why) = refuse(&sd, &dd) {
        return Err(copy_skip(format_args!(
            "scaled or mirrored copy {}: {why}",
            what()
        )));
    }
    let sample = sd.format.sample_type();
    let linear = flags & LINEAR_FILTER_BIT != 0 && sample == SampleType::Float;
    let entry = match sample {
        SampleType::Float => "nxkit_tcopy_float",
        SampleType::Uint => "nxkit_tcopy_uint",
        SampleType::Sint => "nxkit_tcopy_sint",
    };
    let pipeline = sh.pipelines.host_compute("nxkit-tcopy", MSL, entry)?;
    let address = scratch
        .push(&params(&src_box, &dst_box, linear, flags))
        .map_err(|e| {
            copy_skip(format_args!(
                "scaled or mirrored copy {}: scratch {e:?}",
                what()
            ))
        })?;
    let view = |t: &Texture, level: u32, slice: u32| {
        t.view(&ViewDesc {
            format: t.desc().format,
            kind: TextureKind::D2,
            first_level: level,
            level_count: 1,
            first_slice: slice,
            slice_count: 1,
        })
    };
    let (w, h) = (dst_box.size[0], dst_box.size[1]);
    for i in 0..count {
        let (sv, dv) = (
            view(src, src_box.level, src_box.slice + i),
            view(dst, dst_box.level, dst_box.slice + i),
        );
        let (Ok(sv), Ok(dv)) = (sv, dv) else {
            return Err(copy_skip(format_args!(
                "scaled or mirrored copy {}: no view of layer {i}",
                what()
            )));
        };
        for cmd in [
            ComputeCommand::Barrier,
            ComputeCommand::SetPipeline(pipeline.clone()),
            ComputeCommand::SetTexture {
                index: 0,
                texture: sv.clone(),
            },
            ComputeCommand::SetTexture {
                index: 1,
                texture: dv.clone(),
            },
            ComputeCommand::SetBuffer {
                index: 0,
                gpu_addr: address,
            },
            ComputeCommand::DispatchThreads {
                grid: [w, h, 1],
                threads: [8, 8, 1],
            },
            ComputeCommand::Barrier,
        ] {
            cp.encode(&cmd).map_err(|_| Skip::Encode)?;
        }
        cp.retain(sv);
        cp.retain(dv);
    }
    if src_box.size != dst_box.size {
        nxkit_nvn::diag::note_n(Reason::CopyScaled, u64::from(count));
        if linear {
            nxkit_nvn::diag::note_n(Reason::CopyScaledLinear, u64::from(count));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
