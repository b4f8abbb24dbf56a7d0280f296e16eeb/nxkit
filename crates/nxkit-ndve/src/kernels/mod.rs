//! The plan of the movie network: the packed weights, the activation buffers and the dispatches of the kernels in
//! [`crate::msl::SOURCE`], for the passes 3 to 16 of `docs/plans/ndve-native.md` (section 4).
//!
//! The layer table below is private on purpose (it repeats section 4 of that document and will be replaced by `net.rs`).
//!
//! Activations are NHWC f16 buffers. Eight are enough because a tensor dies when its last reader has run:
//!
//! | buffer | holds | size |
//! |---|---|---|
//! | `t1` | layer 10's output (layer 11 reads it) | 960x540x16 |
//! | `s16` | layer 2's output (read by layer 3 and, as the skip, by layer 10) | 960x540x16 |
//! | `d32a` | layers 3's output, later layer 8's | 480x270x32 |
//! | `s32` | layer 4's output (the skip of layer 8) | 480x270x32 |
//! | `d32b` | layer 9's output | 480x270x32 |
//! | `e64a`, `e64b` | layers 5 to 7 | 240x135x64 |
//! | `luma` | the luma of the 1920x1080 picture as f16, for the resampler | 1920x1080 |
//!
//! The colour conversion is fused into layers 1 and 12 (nothing writes the RGBA8 picture; the planes are converted
//! where they are read), and layer 12 writes the RGB10A2 picture ([`GameResource::Intermediate`] 1) which the luma pass
//! and the resampler read.
//!
//! Layers 1 + 2 and 11 + 12 are one dispatch each (`ndve_l01_l02`, `ndve_l11_l12`): the first layer's output, with a
//! one pixel halo, lives in threadgroup memory, so layer 1's output and layer 11's output are never buffers. The
//! 12 layers still own their packed weights (buffers 0 to 11); the fused steps bind two of them.

use crate::pack::pack_layer;
use crate::plan::{Binding, Buffer, GameResource, Plan, Source, Step};

/// The weight blob of the network: layers 1 to 12 with their biases (`docs/plans/ndve-native.md` section 2).
const BLOB_BYTES: usize = 0x3c320;
/// The weights of the resampler's six blocks.
const RESAMPLER_BYTES: usize = 0x3000;
/// Threads of a threadgroup of every convolution kernel (4 simdgroups).
const THREADS: u32 = 128;

/// How a layer is cut into threadgroups.
#[derive(Clone, Copy, Debug)]
enum Geometry {
    /// 3x3: tiles of `tw` x `th` output pixels, one threadgroup a tile.
    Tiles { tw: usize, th: usize },
    /// Two fused 3x3 layers (`Pair` in `conv.metal`): tiles of `tw` x `th` pixels of the second layer's output.
    Pair { tw: usize, th: usize },
    /// 2x2: tasks of `mt` m-tiles (8 pixels each) in one row, one simdgroup a task, four a threadgroup.
    Tasks { mt: usize },
}

/// The convolution a layer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Layer 1: 3x3 from the NV12 planes.
    First,
    /// 3x3, buffer in, buffer out.
    Conv3,
    /// 2x2 stride 2.
    Down,
    /// 2x2 stride 2 transposed with a skip.
    Up,
    /// Layer 12: 3x3 to the RGB10A2 picture.
    Last,
}

/// One row of section 4 of `ndve-native.md`.
#[derive(Clone, Copy, Debug)]
struct Layer {
    kernel: &'static str,
    label: &'static str,
    kind: Kind,
    cin: usize,
    cout: usize,
    /// The size in pixels of the tensor the layer reads (for `First`: the packed picture).
    in_wh: (usize, usize),
    /// The size in pixels of the tensor the layer writes.
    out_wh: (usize, usize),
    w_off: usize,
    b_off: usize,
    geo: Geometry,
}

const T32X4: Geometry = Geometry::Tiles { tw: 32, th: 4 };
/// The tile of the fused pairs (`Pair::TW`, `Pair::TH` in `conv.metal`).
const PAIR: Geometry = Geometry::Pair {
    tw: PAIR_TW,
    th: PAIR_TH,
};
const PAIR_TW: usize = 30;
const PAIR_TH: usize = 12;
/// Simdgroups of the fused pairs' threadgroups (`THREADS / 32`); only the tests read it.
#[cfg(test)]
const PAIR_SIMDGROUPS: usize = 4;
const T16X4: Geometry = Geometry::Tiles { tw: 16, th: 4 };

const LAYERS: [Layer; 12] = [
    Layer {
        kernel: "ndve_l01",
        label: "ndve.w01",
        kind: Kind::First,
        cin: 16,
        cout: 16,
        in_wh: (960, 540),
        out_wh: (960, 540),
        w_off: 0x0,
        b_off: 0x1200,
        geo: PAIR,
    },
    Layer {
        kernel: "ndve_l02",
        label: "ndve.w02",
        kind: Kind::Conv3,
        cin: 16,
        cout: 16,
        in_wh: (960, 540),
        out_wh: (960, 540),
        w_off: 0x1300,
        b_off: 0x2500,
        geo: PAIR,
    },
    Layer {
        kernel: "ndve_l03",
        label: "ndve.w03",
        kind: Kind::Down,
        cin: 16,
        cout: 32,
        in_wh: (960, 540),
        out_wh: (480, 270),
        w_off: 0x2600,
        b_off: 0x3600,
        geo: Geometry::Tasks { mt: 2 },
    },
    Layer {
        kernel: "ndve_l04",
        label: "ndve.w04",
        kind: Kind::Conv3,
        cin: 32,
        cout: 32,
        in_wh: (480, 270),
        out_wh: (480, 270),
        w_off: 0x3700,
        b_off: 0x7f00,
        geo: T32X4,
    },
    Layer {
        kernel: "ndve_l05",
        label: "ndve.w05",
        kind: Kind::Down,
        cin: 32,
        cout: 64,
        in_wh: (480, 270),
        out_wh: (240, 135),
        w_off: 0x8000,
        b_off: 0xc000,
        geo: Geometry::Tasks { mt: 2 },
    },
    Layer {
        kernel: "ndve_l06",
        label: "ndve.w06",
        kind: Kind::Conv3,
        cin: 64,
        cout: 64,
        in_wh: (240, 135),
        out_wh: (240, 135),
        w_off: 0xc100,
        b_off: 0x1e100,
        geo: T16X4,
    },
    Layer {
        kernel: "ndve_l07",
        label: "ndve.w07",
        kind: Kind::Conv3,
        cin: 64,
        cout: 64,
        in_wh: (240, 135),
        out_wh: (240, 135),
        w_off: 0x1e200,
        b_off: 0x30200,
        geo: T16X4,
    },
    Layer {
        kernel: "ndve_l08",
        label: "ndve.w08",
        kind: Kind::Up,
        cin: 64,
        cout: 32,
        in_wh: (240, 135),
        out_wh: (480, 270),
        w_off: 0x30300,
        b_off: 0x34300,
        geo: Geometry::Tasks { mt: 2 },
    },
    Layer {
        kernel: "ndve_l09",
        label: "ndve.w09",
        kind: Kind::Conv3,
        cin: 32,
        cout: 32,
        in_wh: (480, 270),
        out_wh: (480, 270),
        w_off: 0x34400,
        b_off: 0x38c00,
        geo: T32X4,
    },
    Layer {
        kernel: "ndve_l10",
        label: "ndve.w10",
        kind: Kind::Up,
        cin: 32,
        cout: 16,
        in_wh: (480, 270),
        out_wh: (960, 540),
        w_off: 0x38d00,
        b_off: 0x39d00,
        geo: Geometry::Tasks { mt: 4 },
    },
    Layer {
        kernel: "ndve_l11",
        label: "ndve.w11",
        kind: Kind::Conv3,
        cin: 16,
        cout: 16,
        in_wh: (960, 540),
        out_wh: (960, 540),
        w_off: 0x39e00,
        b_off: 0x3b000,
        geo: PAIR,
    },
    Layer {
        kernel: "ndve_l12",
        label: "ndve.w12",
        kind: Kind::Last,
        cin: 16,
        cout: 16,
        in_wh: (960, 540),
        out_wh: (960, 540),
        w_off: 0x3b100,
        b_off: 0x3c300,
        geo: PAIR,
    },
];

impl Layer {
    /// Taps of the convolution: 9 for 3x3, 4 for 2x2.
    fn taps(&self) -> usize {
        match self.kind {
            Kind::First | Kind::Conv3 | Kind::Last => 9,
            Kind::Down | Kind::Up => 4,
        }
    }
    /// Bytes of the weights in the game's blob.
    fn weight_bytes(&self) -> usize {
        self.taps() * self.cin * self.cout * 2
    }
    /// Bytes of the input tensor.
    #[cfg(test)]
    fn in_bytes(&self) -> u64 {
        (self.in_wh.0 * self.in_wh.1 * self.cin * 2) as u64
    }
    /// Bytes of the output tensor.
    #[cfg(test)]
    fn out_bytes(&self) -> u64 {
        (self.out_wh.0 * self.out_wh.1 * self.cout * 2) as u64
    }
    /// The threadgroups of the layer's dispatch.
    fn groups(&self) -> [u32; 3] {
        let ceil = |a: usize, b: usize| u32::try_from(a.div_ceil(b)).unwrap_or(u32::MAX);
        match self.geo {
            Geometry::Tiles { tw, th } | Geometry::Pair { tw, th } => {
                [ceil(self.out_wh.0, tw), ceil(self.out_wh.1, th), 1]
            }
            Geometry::Tasks { mt } => {
                // tasks are over the output (down) or the input (up) pixels
                let (w, h) = if self.kind == Kind::Up {
                    self.in_wh
                } else {
                    self.out_wh
                };
                [ceil(w / (8 * mt) * h, 4), 1, 1]
            }
        }
    }
}

// Buffer indices in `Plan::buffers`: the 12 packed layers first, then these.
const B_T1: usize = 12;
const B_S16: usize = 13;
const B_D32A: usize = 14;
const B_S32: usize = 15;
const B_D32B: usize = 16;
const B_E64A: usize = 17;
const B_E64B: usize = 18;
const B_LUMA: usize = 19;
const B_RS_W: usize = 20;

/// Layers 2 and 11 run inside the step of layers 1 and 12.
fn fused_second(n: usize) -> bool {
    n == 1 || n == 10
}
/// The kernel of the step that starts at layer `n`.
fn step_kernel(n: usize, l: &Layer) -> &'static str {
    match n {
        0 => "ndve_l01_l02",
        11 => "ndve_l11_l12",
        _ => l.kernel,
    }
}

fn buf(index: u32, source: usize, writes: bool) -> Binding {
    Binding {
        index,
        source: Source::Buffer(source),
        writes,
    }
}
fn game(index: u32, resource: GameResource, writes: bool) -> Binding {
    Binding {
        index,
        source: Source::Game(resource),
        writes,
    }
}

/// The plan for the network of the weight blob `weights` (0x3c320 bytes) and the resampler's weights (0x3000 bytes).
///
/// The steps, in order: layers 1 + 2 fused (`ndve_l01_l02`, takes the game's NV12 planes and conversion push block),
/// layers 3 to 10, layers 11 + 12 fused (`ndve_l11_l12`, writes the RGB10A2 picture), the luma pass, the resampler:
/// 12 steps. The kernels and their binding indices are those of [`crate::msl::SOURCE`]: buffer 0 input, 1 output,
/// 2 packed weights, 3 skip, 4 push block, 5 the packed weights of a fused pair's second layer; the resampler's are
/// listed with its step.
///
/// # Errors
///
/// When a slice has the wrong length or a layer's weights cannot be packed.
pub fn plan(weights: &[u8], resampler: &[u8]) -> Result<Plan, String> {
    if weights.len() != BLOB_BYTES {
        return Err(format!(
            "the weight blob has {} bytes, expected {BLOB_BYTES}",
            weights.len()
        ));
    }
    if resampler.len() != RESAMPLER_BYTES {
        return Err(format!(
            "the resampler weights have {} bytes, expected {RESAMPLER_BYTES}",
            resampler.len()
        ));
    }
    let mut buffers = Vec::with_capacity(21);
    for l in &LAYERS {
        let w = weights
            .get(l.w_off..l.w_off + l.weight_bytes())
            .ok_or_else(|| format!("{}: weights outside the blob", l.label))?;
        let b = weights
            .get(l.b_off..)
            .ok_or_else(|| format!("{}: bias outside the blob", l.label))?;
        let packed =
            pack_layer(w, b, l.taps(), l.cin, l.cout).map_err(|e| format!("{}: {e}", l.label))?;
        buffers.push(Buffer {
            name: l.label,
            bytes: packed.len() as u64,
            init: Some(packed),
        });
    }
    let scratch = |name: &'static str, w: usize, h: usize, c: usize| Buffer {
        name,
        bytes: (w * h * c * 2) as u64,
        init: None,
    };
    buffers.push(scratch("ndve.t1", 960, 540, 16));
    buffers.push(scratch("ndve.s16", 960, 540, 16));
    buffers.push(scratch("ndve.d32a", 480, 270, 32));
    buffers.push(scratch("ndve.s32", 480, 270, 32));
    buffers.push(scratch("ndve.d32b", 480, 270, 32));
    buffers.push(scratch("ndve.e64a", 240, 135, 64));
    buffers.push(scratch("ndve.e64b", 240, 135, 64));
    buffers.push(scratch("ndve.luma", 1920, 1080, 1));
    buffers.push(Buffer {
        name: "ndve.rs_w",
        bytes: RESAMPLER_BYTES as u64,
        init: Some(resampler.to_vec()),
    });

    // (input, output, skip) per layer, as buffer indices
    let routes: [(usize, usize, Option<usize>); 12] = [
        (usize::MAX, B_T1, None),
        (B_T1, B_S16, None),
        (B_S16, B_D32A, None),
        (B_D32A, B_S32, None),
        (B_S32, B_E64A, None),
        (B_E64A, B_E64B, None),
        (B_E64B, B_E64A, None),
        (B_E64A, B_D32A, Some(B_S32)),
        (B_D32A, B_D32B, None),
        (B_D32B, B_T1, Some(B_S16)),
        (B_T1, B_S16, None),
        (B_S16, usize::MAX, None),
    ];
    let mut steps = Vec::with_capacity(12);
    for (n, (l, &(input, output, skip))) in LAYERS.iter().zip(routes.iter()).enumerate() {
        let mut buffers = Vec::new();
        let mut textures = Vec::new();
        match l.kind {
            Kind::First => {
                // fused with layer 2: that layer's output is the step's
                buffers.push(buf(1, routes[n + 1].1, true));
                buffers.push(buf(2, n, false));
                buffers.push(buf(5, n + 1, false));
                buffers.push(game(4, GameResource::ConversionPush, false));
                textures.push(game(0, GameResource::LumaPlane, false));
                textures.push(game(1, GameResource::ChromaPlane, false));
            }
            Kind::Last => {
                // fused with layer 11: the step reads that layer's input and has both weights
                buffers.push(buf(0, routes[n - 1].0, false));
                buffers.push(buf(2, n - 1, false));
                buffers.push(buf(5, n, false));
                buffers.push(game(4, GameResource::ConversionPush, false));
                textures.push(game(0, GameResource::LumaPlane, false));
                textures.push(game(1, GameResource::ChromaPlane, false));
                textures.push(game(2, GameResource::Intermediate { index: 1 }, true));
            }
            Kind::Conv3 | Kind::Down | Kind::Up => {
                if fused_second(n) {
                    continue; // the step of the layer before covers it
                }
                buffers.push(buf(0, input, false));
                buffers.push(buf(1, output, true));
                buffers.push(buf(2, n, false));
                if let Some(s) = skip {
                    buffers.push(buf(3, s, false));
                }
            }
        }
        steps.push(Step {
            kernel: step_kernel(n, l),
            groups: l.groups(),
            threads: [THREADS, 1, 1],
            threadgroup_bytes: 0,
            buffers,
            textures,
        });
    }
    steps.push(Step {
        kernel: "ndve_luma",
        groups: [120, 135, 1],
        threads: [16, 8, 1],
        threadgroup_bytes: 0,
        buffers: vec![buf(0, B_LUMA, true)],
        textures: vec![game(0, GameResource::Intermediate { index: 1 }, false)],
    });
    steps.push(Step {
        kernel: "ndve_resample",
        // 320 x 180 blocks of 8x8 pixels: a threadgroup of 4 simdgroups takes 4 rounds of 4 x 4 blocks of a row
        groups: [5, 180, 1],
        threads: [THREADS, 1, 1],
        threadgroup_bytes: 0,
        buffers: vec![
            buf(0, B_LUMA, false),
            buf(1, B_RS_W, false),
            game(2, GameResource::ResamplerPush, false),
        ],
        textures: vec![
            game(0, GameResource::Intermediate { index: 1 }, false),
            game(1, GameResource::Dither, false),
            game(2, GameResource::Output, true),
        ],
    });
    Ok(Plan { buffers, steps })
}

#[cfg(test)]
mod tests;
