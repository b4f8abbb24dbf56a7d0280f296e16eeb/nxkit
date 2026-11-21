//! The Metal description of a record of the resource table.

use nxkit_metal::{Format, Storage, TextureDesc, TextureKind};
use nxkit_nvn::format::{Aspects, format_info};
use nxkit_nvn::layout::Target;
use nxkit_nvn::resource::TextureRecord;

use super::Use;
use crate::format_map::{Mapped, texture_format};
use crate::skip::Skip;

pub(super) fn kind_of(t: Target) -> Option<TextureKind> {
    Some(match t {
        Target::D1 | Target::D2 | Target::Rectangle | Target::D2Multisample => TextureKind::D2,
        Target::D1Array | Target::D2Array | Target::D2MultisampleArray => TextureKind::D2Array,
        Target::D3 => TextureKind::D3,
        Target::Cubemap => TextureKind::Cube,
        Target::CubemapArray => TextureKind::CubeArray,
        Target::Buffer => TextureKind::D2,
    })
}

/// The widest and highest 2D texture Metal makes, also the texels of one row of a texture buffer.
pub(crate) const MAX_2D: u32 = 16384;

/// The width and height of the 2D texture that holds a texture buffer of `elements` texels: rows of [`MAX_2D`]
/// texels, element `x` at (x % 16384, x / 16384), as the translator wraps a 1D fetch. The last row may be short; its
/// tail reads the bytes after the buffer. None when the rows do not fit a 2D texture.
pub(crate) fn buffer_rows(elements: u32) -> Option<(u32, u32)> {
    let h = elements.div_ceil(MAX_2D).max(1);
    (h <= MAX_2D).then_some((elements.clamp(1, MAX_2D), h))
}

/// The float format of the same bits as a 32-bit integer format. The translated shaders declare every texture they
/// fetch from as `float` and take the bits of the result (Maxwell's fetch gives raw register bits), so a texture buffer of
/// 32-bit integers is read through a float format, which keeps every bit. Narrower integers keep their format.
fn float_bits(f: Format) -> Format {
    match f {
        Format::R32Uint | Format::R32Sint => Format::R32Float,
        Format::Rg32Uint | Format::Rg32Sint => Format::Rg32Float,
        Format::Rgba32Uint | Format::Rgba32Sint => Format::Rgba32Float,
        f => f,
    }
}

/// The bytes between two rows of a pitch-linear record: its stride, or for a texture buffer one row of
/// [`buffer_rows`].
pub(crate) fn row_pitch(rec: &TextureRecord) -> u32 {
    if rec.desc.target != Target::Buffer {
        return rec.desc.stride;
    }
    let bpb = format_info(rec.desc.format).map_or(0, |i| u32::from(i.bytes_per_block));
    buffer_rows(rec.desc.width).map_or(0, |(w, _)| w * bpb)
}

fn scaled_size(v: u32, scale: f32) -> u32 {
    ((v as f32 * scale).round() as u32).max(1)
}

/// The Metal description of a record at `scale` (1.0 for an unscaled texture).
pub(crate) fn describe(
    rec: &TextureRecord,
    scale: f32,
    u: Use,
) -> Result<(TextureDesc, bool), Skip> {
    let d = &rec.desc;
    let unsupported = |what: &str| {
        nxkit_nvn::diag::note(
            nxkit_nvn::diag::Reason::FormatUnsupported,
            format_args!(
                "{what} {:?} {:?} {}x{}x{}",
                d.format, d.target, d.width, d.height, d.depth
            ),
        );
        Skip::Format
    };
    let mapped = texture_format(d.format).ok_or_else(|| unsupported("format"))?;
    // Cube textures are 2D arrays of their faces, so that a shader may read them as a cube, a cube array or a 2D
    // array: Metal makes a cube array view only of a 2D array.
    let kind = match kind_of(d.target).ok_or_else(|| unsupported("target"))? {
        TextureKind::Cube | TextureKind::CubeArray => TextureKind::D2Array,
        k => k,
    };
    let format = mapped.format();
    let format = if d.target == Target::Buffer {
        float_bits(format)
    } else {
        format
    };
    let info = format_info(d.format).map_err(|_| Skip::Format)?;
    let depth_like = matches!(
        info.aspects,
        Aspects::Depth | Aspects::Stencil | Aspects::DepthStencil
    );
    let (width, height, depth, layers) = match d.target {
        Target::D1 => (d.width, 1, 1, 1),
        Target::D1Array => (d.width, 1, 1, d.height.max(1)),
        Target::D2 | Target::Rectangle | Target::D2Multisample => (d.width, d.height, 1, 1),
        Target::Cubemap => (d.width, d.height, 1, 6),
        Target::D2Array | Target::D2MultisampleArray => (d.width, d.height, 1, d.depth.max(1)),
        Target::D3 => (d.width, d.height, d.depth.max(1), 1),
        Target::CubemapArray => (d.width, d.height, 1, (d.depth / 6).max(1) * 6),
        // A texture buffer is a 2D texture of rows of 16384 texels over the same bytes: the translated shaders fetch
        // element x as a `texture2d` at (x & 0x3FFF, x >> 14) (`buffer_rows`).
        Target::Buffer => match buffer_rows(d.width) {
            Some((w, h)) => (w, h, 1, 1),
            None => return Err(unsupported("texture buffer of more rows than a 2D texture")),
        },
    };
    let scale = if d.target == Target::Buffer {
        1.0
    } else {
        scale
    };
    let (width, height) = (scaled_size(width, scale), scaled_size(height, scale));
    let max_levels = 32 - width.max(height).max(depth).leading_zeros();
    let compressed = matches!(format, Format::Bc(_) | Format::Astc { .. });
    // A texture that the frame only samples can share storage with one that another frame renders into: the game
    // makes texture objects for the same memory again, and the store shares a Metal texture between records only
    // when their descriptions match. So every texture that Metal can render into gets the usage, and a record that
    // is sampled first sees what a later render target of the same memory wrote.
    // Metal cannot render into a compressed format: an attachment of one renders into a block shadow (`blocks`).
    let render_target = !compressed && (u.attachment || format != Format::Rgb9E5Float);
    let memoryless = u.memoryless && u.attachment && !u.cpu_read;
    let exact = matches!(mapped, Mapped::Exact(_));
    let desc = TextureDesc {
        kind,
        width,
        height,
        depth,
        mip_levels: if d.target == Target::Buffer {
            1
        } else {
            d.levels.max(1).min(max_levels)
        },
        array_layers: layers,
        allow_views: true,
        format,
        shader_read: !memoryless,
        shader_write: exact && !depth_like && !compressed && !memoryless,
        render_target,
        storage: if memoryless {
            Storage::Memoryless
        } else {
            Storage::Private
        },
        shader_atomic: u.atomic && exact && !depth_like && !compressed && !memoryless,
    };
    Ok((desc, !exact))
}
