//! The `lum` diagnostic. On each sampled frame (one in 300, see `crate::diag::sampled`) it copies nine pixels
//! immediately after each stored colour pass that passes `NXKIT_DIAG_TEX`. Reading those snapshots after completion
//! preserves each pass's output even when later passes overwrite its target. With `img` too, targets of 640 texels and
//! wider are sampled on a 96x54 grid and written as pictures into `<NXKIT_DIAG_DIR>/img`.

use std::sync::Mutex;

use nxkit_metal::{
    ArgumentTable, CommandBuffer, ComputeCommand, Context, Dependency, Format, Scope, Stages,
    Storage, Texture, TextureKind, TextureRegion, Use, Visibility,
};
use nxkit_nvn::resource::TexId;

/// The sample grid of the `img` diagnostic for the main resolution: columns and rows of one-texel copies that make a
/// coarse picture of a full size target.
const GRID: [u32; 2] = [96, 54];

/// The grid that `capture` samples for a target of `width` by `height`. With the `img` diagnostic on, targets of the
/// main resolution get the picture grid and small targets (cube faces, probes) are copied texel by texel. Every other
/// target keeps the 3 by 3 probe.
fn grid(width: u32, height: u32) -> [u32; 2] {
    if !crate::diag("img") {
        [3, 3]
    } else if width >= 640 && height >= 360 {
        GRID
    } else if (8..=64).contains(&width) && (4..=64).contains(&height) {
        [width, height]
    } else {
        [3, 3]
    }
}

static TARGETS: Mutex<Vec<(u32, TexId, [u32; 2], Texture)>> = Mutex::new(Vec::new());

/// True when the current frame is sampled. The caller starts the frame with `crate::diag::begin_frame` first.
pub(crate) fn begin() -> bool {
    active()
}

/// True while the current frame is sampled.
pub(crate) fn active() -> bool {
    crate::diag("lum") && crate::diag::sampled()
}

/// Saves pixels before another pass can overwrite the target. This changes GPU scheduling only on sampled frames.
pub(crate) fn capture(
    ctx: &Context,
    cb: &mut CommandBuffer,
    table: &ArgumentTable,
    cmd: u32,
    id: TexId,
    tex: &Texture,
    depth_plane: u32,
) {
    let d = tex.desc();
    if d.storage == Storage::Memoryless || d.format.is_compressed() || d.format.has_stencil() {
        return;
    }
    let mut sd = d;
    sd.kind = TextureKind::D2;
    let g = grid(d.width, d.height);
    sd.width = g[0];
    sd.height = g[1];
    sd.depth = 1;
    sd.array_layers = 1;
    sd.mip_levels = 1;
    sd.storage = Storage::Private;
    sd.render_target = false;
    sd.shader_write = false;
    let Ok(snapshot) = Texture::new(ctx, sd) else {
        return;
    };
    let Ok(mut cp) = cb.begin_compute_pass(table) else {
        return;
    };
    let _ = cp.encode(&ComputeCommand::Depend(Dependency::between(
        Use::AttachmentWrite,
        Use::CopyRead,
    )));
    for j in 0..g[1] {
        for i in 0..g[0] {
            let region = TextureRegion {
                level: 0,
                slice: 0,
                origin: [
                    d.width * (2 * i + 1) / (2 * g[0]),
                    d.height * (2 * j + 1) / (2 * g[1]),
                    depth_plane,
                ],
                size: [1, 1, 1],
            };
            let _ = cp.encode(&ComputeCommand::CopyTexture {
                src: tex.clone(),
                dst: snapshot.clone(),
                src_region: region,
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
    if let Ok(mut t) = TARGETS.lock() {
        t.push((cmd, id, [d.width, d.height], snapshot));
    }
}

/// Prints the noted targets in command order. The GPU has to be done with the frame.
pub(crate) fn dump(ctx: &Context) {
    let mut all = match TARGETS.lock() {
        Ok(mut t) => std::mem::take(&mut *t),
        Err(_) => return,
    };
    all.sort_by_key(|(c, _, _, _)| *c);
    eprintln!(
        "DIAG lum frame {} passes {}",
        crate::diag::frame(),
        all.len()
    );
    for (cmd, id, size, tex) in all {
        let d = tex.desc();
        let g = grid(size[0], size[1]);
        let pts: Vec<[u32; 2]> = (0..g[1])
            .flat_map(|j| (0..g[0]).map(move |i| [i, j]))
            .collect();
        let px = tex.debug_read_raw(ctx, &pts);
        if g != [3, 3] {
            write_picture(cmd, id, d.format, g, px.as_deref().unwrap_or_default());
            continue;
        }
        let vals: Vec<String> = px.iter().flatten().map(|b| decode(d.format, b)).collect();
        eprintln!(
            "DIAG lum cmd {cmd} {id:?} {}x{} {:?} {}",
            size[0],
            size[1],
            d.format,
            vals.join(" ")
        );
    }
}

/// The colour of a texel for a picture, clamped to 0..1.
fn rgb(f: Format, b: &[u8]) -> [f32; 3] {
    let u32s = |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
    let c = match f {
        Format::Rgba8Unorm | Format::Rgba8UnormSrgb => [
            f32::from(b[0]) / 255.0,
            f32::from(b[1]) / 255.0,
            f32::from(b[2]) / 255.0,
        ],
        Format::Bgra8Unorm | Format::Bgra8UnormSrgb => [
            f32::from(b[2]) / 255.0,
            f32::from(b[1]) / 255.0,
            f32::from(b[0]) / 255.0,
        ],
        Format::Rg11B10Float => {
            let w = u32s(0);
            [
                small_float(w & 0x7ff, 6),
                small_float(w >> 11 & 0x7ff, 6),
                small_float(w >> 22, 5),
            ]
        }
        Format::Rgb10A2Unorm => {
            let w = u32s(0);
            let c = |s: u32| (w >> s & 0x3ff) as f32 / 1023.0;
            [c(0), c(10), c(20)]
        }
        Format::R32Float | Format::Depth32Float => {
            let v = f32::from_bits(u32s(0));
            [v, v, v]
        }
        Format::R16Float => {
            let v = half(u16::from_le_bytes([b[0], b[1]]));
            [v, v, v]
        }
        _ => [0.0; 3],
    };
    c.map(|v| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    })
}

/// Writes the sampled texels of a full size target as a binary PPM under `<NXKIT_DIAG_DIR>/img`.
fn write_picture(cmd: u32, id: TexId, format: Format, g: [u32; 2], px: &[Vec<u8>]) {
    use std::io::Write;
    let dir = crate::diag::diag_dir().join("img");
    let _ = std::fs::create_dir_all(&dir);
    let name = dir.join(format!("cmd{cmd:05}-tex{}-{format:?}.ppm", id.0));
    let Ok(mut file) = std::fs::File::create(&name) else {
        return;
    };
    let _ = write!(file, "P6\n{} {}\n255\n", g[0], g[1]);
    let mut bytes = Vec::with_capacity(px.len() * 3);
    for texel in px {
        if texel.len() < 4 {
            bytes.extend_from_slice(&[0, 0, 0]);
            continue;
        }
        bytes.extend(rgb(format, texel).map(|v| (v * 255.0) as u8));
    }
    let _ = file.write_all(&bytes);
}

fn half(h: u16) -> f32 {
    let s = if h >> 15 != 0 { -1.0 } else { 1.0 };
    let e = i32::from(h >> 10 & 0x1f);
    let m = f32::from(h & 0x3ff);
    match e {
        0 => s * m * 2f32.powi(-24),
        31 => s * f32::INFINITY,
        _ => s * (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

fn small_float(bits: u32, mant: u32) -> f32 {
    let e = (bits >> mant) as i32;
    let m = (bits & ((1 << mant) - 1)) as f32 / (1u32 << mant) as f32;
    if e == 0 {
        m * 2f32.powi(-14)
    } else {
        (1.0 + m) * 2f32.powi(e - 15)
    }
}

fn decode(f: Format, b: &[u8]) -> String {
    let u16s = |i: usize| u16::from_le_bytes([b[2 * i], b[2 * i + 1]]);
    let u32s = |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
    match f {
        Format::Rgba16Float | Format::Rg16Float | Format::R16Float => {
            let v: Vec<String> = (0..b.len() / 2)
                .map(|i| format!("{:.3}", half(u16s(i))))
                .collect();
            format!("({})", v.join(","))
        }
        Format::Rgba32Float | Format::Rg32Float | Format::R32Float | Format::Depth32Float => {
            let v: Vec<String> = (0..b.len() / 4)
                .map(|i| format!("{:.3}", f32::from_bits(u32s(i))))
                .collect();
            format!("({})", v.join(","))
        }
        Format::Rg11B10Float => {
            let w = u32s(0);
            let (r, g, bl) = (
                small_float(w & 0x7ff, 6),
                small_float(w >> 11 & 0x7ff, 6),
                small_float(w >> 22, 5),
            );
            format!("({r:.3},{g:.3},{bl:.3})")
        }
        Format::Rgb10A2Unorm => {
            let w = u32s(0);
            let c = |s: u32| f64::from(w >> s & 0x3ff) / 1023.0;
            format!("({:.3},{:.3},{:.3},{})", c(0), c(10), c(20), w >> 30)
        }
        _ => format!("{b:02x?}"),
    }
}
