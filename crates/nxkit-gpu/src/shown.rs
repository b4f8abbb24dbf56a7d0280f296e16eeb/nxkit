//! The first picture that shows something. A game presents black pictures while it boots, often for seconds. The host
//! wants to know when the picture is no longer black (`when_shown`): a window over a launcher stays hidden until then,
//! and the launcher's starting screen stays up (`nxkit-host` `sys/present.rs`).
//!
//! While someone waits, each present copies an 8 by 5 grid of texels of the presented picture into a small shared
//! texture (as the `lum` diagnostic does) and reads it when the GPU is done. A picture with two texels brighter than
//! nearly black has something on it, and the wait ends. A format this cannot judge counts as shown.

use std::sync::{Mutex, PoisonError};

use nxkit_metal::{
    ArgumentTable, CommandBuffer, ComputeCommand, Context, Dependency, Format, Scope, Stages,
    Storage, Texture, TextureKind, TextureRegion, Use, Visibility,
};

/// The texels the probe looks at, across the presented part of the picture.
const GRID: [u32; 2] = [8, 5];
/// A channel above this (of 1) is not black.
const LIT: f32 = 0.06;
/// So many texels must be lit.
const ENOUGH: usize = 2;

type Hook = Box<dyn FnOnce() + Send>;

// Process-wide (W26): the host sets the hook before the game starts; the present path and a Metal completion handler,
// which have no context of the host, take it.
static WAITING: Mutex<Option<Hook>> = Mutex::new(None);

/// Runs `hook` once, on a Metal thread, when a presented picture first shows something.
pub fn when_shown(hook: impl FnOnce() + Send + 'static) {
    *WAITING.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(hook));
}

fn waiting() -> bool {
    WAITING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some()
}

fn fire() {
    let hook = WAITING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(h) = hook {
        h();
    }
}

/// Copies the grid of `src` inside `crop` (`[x, y, width, height]`, all of it when empty) after the present pass, and
/// judges it when the GPU is done. Nothing while nobody waits.
pub(crate) fn probe(
    ctx: &Context,
    cb: &mut CommandBuffer,
    table: &ArgumentTable,
    src: &Texture,
    crop: [u32; 4],
) {
    if !waiting() {
        return;
    }
    let d = src.desc();
    if d.storage == Storage::Memoryless || d.format.is_compressed() || d.format.has_stencil() {
        fire();
        return;
    }
    let [x, y, w, h] = if crop[2] == 0 || crop[3] == 0 {
        [0, 0, d.width, d.height]
    } else {
        crop
    };
    let mut sd = d;
    sd.kind = TextureKind::D2;
    sd.width = GRID[0];
    sd.height = GRID[1];
    sd.depth = 1;
    sd.array_layers = 1;
    sd.mip_levels = 1;
    sd.storage = Storage::Shared;
    sd.render_target = false;
    sd.shader_write = false;
    let Ok(grid) = Texture::new(ctx, sd) else {
        fire();
        return;
    };
    let Ok(mut cp) = cb.begin_compute_pass(table) else {
        return;
    };
    let _ = cp.encode(&ComputeCommand::Depend(Dependency::between(
        Use::AttachmentWrite,
        Use::CopyRead,
    )));
    for j in 0..GRID[1] {
        for i in 0..GRID[0] {
            let origin = [
                (x + w * (2 * i + 1) / (2 * GRID[0])).min(d.width - 1),
                (y + h * (2 * j + 1) / (2 * GRID[1])).min(d.height - 1),
                0,
            ];
            let _ = cp.encode(&ComputeCommand::CopyTexture {
                src: src.clone(),
                dst: grid.clone(),
                src_region: TextureRegion {
                    level: 0,
                    slice: 0,
                    origin,
                    size: [1, 1, 1],
                },
                dst_level: 0,
                dst_slice: 0,
                dst_origin: [i, j, 0],
            });
        }
    }
    let _ = cp.encode(&ComputeCommand::Depend(Dependency {
        scope: Scope::ToLaterPasses,
        after: Stages::BLIT,
        before: Stages::ALL,
        visibility: Visibility::Device,
    }));
    cp.end();
    let format = d.format;
    cb.after_gpu(move |ok| {
        if !ok || !waiting() {
            return;
        }
        let bpp = format.bytes_per_pixel() as usize;
        let row = bpp * GRID[0] as usize;
        let mut px = vec![0u8; row * GRID[1] as usize];
        if grid.read(row, &mut px).is_err() {
            return;
        }
        if lit(format, &px, bpp) {
            fire();
        }
    });
}

/// Whether enough texels of `px` (`bpp` bytes each) in `format` are brighter than nearly black. A format it cannot
/// read counts as lit.
fn lit(format: Format, px: &[u8], bpp: usize) -> bool {
    let bright = |b: &[u8]| -> Option<bool> {
        let u32s =
            |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        let max = match format {
            Format::Rgba8Unorm
            | Format::Rgba8UnormSrgb
            | Format::Bgra8Unorm
            | Format::Bgra8UnormSrgb => f32::from(b[0].max(b[1]).max(b[2])) / 255.0,
            Format::Rgb10A2Unorm => {
                let w = u32s(0);
                (0..3)
                    .map(|c| (w >> (10 * c) & 0x3ff) as f32 / 1023.0)
                    .fold(0.0, f32::max)
            }
            Format::Rgba16Float => (0..3)
                .map(|i| crate::lum::half(u16::from_le_bytes([b[2 * i], b[2 * i + 1]])))
                .fold(0.0, f32::max),
            Format::Rg11B10Float => {
                let w = u32s(0);
                [
                    crate::lum::small_float(w & 0x7ff, 6),
                    crate::lum::small_float(w >> 11 & 0x7ff, 6),
                    crate::lum::small_float(w >> 22, 5),
                ]
                .into_iter()
                .fold(0.0, f32::max)
            }
            _ => return None,
        };
        Some(max > LIT)
    };
    let mut n = 0;
    for texel in px.chunks_exact(bpp) {
        match bright(texel) {
            None => return true,
            Some(true) => n += 1,
            Some(false) => {}
        }
    }
    n >= ENOUGH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_is_not_shown_and_a_logo_is() {
        let black = [0u8, 0, 0, 255].repeat(40);
        assert!(!lit(Format::Bgra8Unorm, &black, 4));
        let mut logo = black.clone();
        logo[4 * 17..4 * 17 + 3].copy_from_slice(&[200, 200, 200]);
        assert!(
            !lit(Format::Bgra8Unorm, &logo, 4),
            "one texel is not enough"
        );
        logo[4 * 18..4 * 18 + 3].copy_from_slice(&[200, 10, 10]);
        assert!(lit(Format::Bgra8Unorm, &logo, 4));
    }

    #[test]
    fn a_format_it_cannot_read_counts_as_shown() {
        assert!(lit(Format::R32Float, &[0u8; 4 * 40], 4));
    }
}
