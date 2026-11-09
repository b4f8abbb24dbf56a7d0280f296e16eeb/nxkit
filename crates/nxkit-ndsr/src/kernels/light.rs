//! The plan of a vr_light network program (`docs/plans/ndsr-native.md` section 15): the packed weights and one dispatch
//! of a kernel of `ndsr_light.metal`.
//!
//! There is one kernel per program of the pack: seven ratios (1.2, 4/3, 1.5, 12/7, 2, 2.4, 3), two programs each (A
//! encodes sRGB, B writes linear) and a third for 3.0 (C, linear with an f32 luma). The kernel reads the output tile's
//! mapping, the offset, the output size and the sharpening strength from the game's push block, so a plan serves every
//! input and output size; only the threadgroup count follows the input size. The tile (words 18, 19) must be the
//! kernel's grid, which the plan checks.

use super::{OUT, PUSH, TEX0};
use crate::net::{Net, Pack, Variant};
use crate::pack;
use crate::plan::{Binding, Buffer, Plan, Source, Step};

/// One network program of the vr_light pack: the parameters of its instantiation in `ndsr_light.metal`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LightSpec {
    /// The kernel's function name.
    pub name: &'static str,
    /// Input pixels per patch step (`in`).
    pub in_px: u32,
    /// Grid cells per patch (`out`): the ratio is `out / in`.
    pub out_px: u32,
    /// Input pixels per threadgroup in x and y (config bytes `b0`, `b1`).
    pub step: [u32; 2],
    /// Layers of the network: 6, or 3 for the 4/3 model.
    pub layers: u32,
    /// The window's luma is computed in f32 (the programs that load with `TLD`).
    pub f32_luma: bool,
    /// The program of variant A: the result is sRGB-encoded.
    pub srgb: bool,
    /// Simdgroups of a threadgroup (`NSG`).
    pub simdgroups: u32,
    /// Patches a simdgroup takes at once (`NB`).
    pub patch_group: u32,
}

impl LightSpec {
    /// Patches of a threadgroup along x and y.
    #[must_use]
    pub const fn patches(&self) -> [u32; 2] {
        [
            (self.step[0] + 1) / self.in_px,
            (self.step[1] + 1) / self.in_px,
        ]
    }

    /// The grid of a threadgroup, which is its output tile, in pixels.
    #[must_use]
    pub const fn tile(&self) -> [u32; 2] {
        let [npx, npy] = self.patches();
        [self.out_px * npx, self.out_px * npy]
    }

    /// The threads of a threadgroup.
    #[must_use]
    pub const fn threads(&self) -> u32 {
        32 * self.simdgroups
    }

    /// The threadgroups for an input of `in_size` pixels, as the game dispatches them.
    #[must_use]
    pub const fn groups(&self, in_size: [u32; 2]) -> [u32; 3] {
        [
            in_size[0].div_ceil(self.step[0]),
            in_size[1].div_ceil(self.step[1]),
            1,
        ]
    }

    /// The threadgroup memory of the kernel in bytes (a mirror of `ndsr_light`): grid, window or luma, result
    /// scratch, staged weights.
    #[must_use]
    pub const fn threadgroup_bytes(&self) -> u32 {
        let [npx, npy] = self.patches();
        let [gw, gh] = self.tile();
        let win = (self.in_px * (npx - 1) + 16) * (self.in_px * (npy - 1) + 16);
        let scr = if win > gw * gh { win } else { gw * gh };
        let ys = if self.out_px == 8 { 1 } else { 256 };
        let base = (gw * gh + scr + self.simdgroups * ys) * 2;
        let fit = if base + 2048 <= 32768 - 256 {
            (32768 - 256 - base) / 2048
        } else {
            0
        };
        let staged = if fit > self.layers { self.layers } else { fit };
        base + if staged == 0 { 16 } else { 2048 * staged }
    }
}

#[allow(clippy::too_many_arguments)]
const fn light(
    name: &'static str,
    in_px: u32,
    out_px: u32,
    step: [u32; 2],
    layers: u32,
    f32_luma: bool,
    srgb: bool,
    shape: [u32; 2],
) -> LightSpec {
    LightSpec {
        name,
        in_px,
        out_px,
        step,
        layers,
        f32_luma,
        srgb,
        simdgroups: shape[0],
        patch_group: shape[1],
    }
}

/// The 15 network programs of the vr_light pack, in pack order (the tile is the config bytes `b0 b1`).
pub const LIGHT_KERNELS: [LightSpec; 15] = [
    light(
        "ndsr_light_r12_a",
        10,
        12,
        [40, 40],
        6,
        false,
        true,
        [16, 1],
    ),
    light(
        "ndsr_light_r12_b",
        10,
        12,
        [40, 40],
        6,
        false,
        false,
        [16, 1],
    ),
    light("ndsr_light_r43_a", 6, 8, [48, 48], 3, true, true, [16, 1]),
    light("ndsr_light_r43_b", 6, 8, [48, 24], 3, false, false, [16, 1]),
    light("ndsr_light_r15_a", 8, 12, [32, 32], 6, false, true, [16, 1]),
    light(
        "ndsr_light_r15_b",
        8,
        12,
        [32, 32],
        6,
        false,
        false,
        [16, 1],
    ),
    light(
        "ndsr_light_r127_a",
        7,
        12,
        [28, 28],
        6,
        false,
        true,
        [16, 1],
    ),
    light(
        "ndsr_light_r127_b",
        7,
        12,
        [28, 28],
        6,
        false,
        false,
        [16, 1],
    ),
    light("ndsr_light_r2_a", 6, 12, [24, 24], 6, false, true, [16, 1]),
    light("ndsr_light_r2_b", 6, 12, [24, 24], 6, false, false, [16, 1]),
    light("ndsr_light_r24_a", 5, 12, [20, 20], 6, false, true, [16, 1]),
    light(
        "ndsr_light_r24_b",
        5,
        12,
        [20, 20],
        6,
        false,
        false,
        [16, 1],
    ),
    light("ndsr_light_r3_a", 4, 12, [16, 16], 6, false, true, [16, 1]),
    light("ndsr_light_r3_b", 4, 12, [16, 16], 6, false, false, [16, 1]),
    light("ndsr_light_r3_c", 4, 12, [16, 16], 6, true, false, [16, 1]),
];

/// The kernel of a vr_light program: by scale, tile and variant (A: sRGB; B: linear; C: linear, f32 luma).
#[must_use]
pub fn find(net: &Net) -> Option<&'static LightSpec> {
    let srgb = net.variant == Variant::A;
    let f32_luma = net.variant == Variant::C || (net.out_px == 8 && srgb);
    LIGHT_KERNELS.iter().find(|k| {
        net.pack == Pack::VrLight
            && k.in_px == u32::from(net.in_px)
            && k.out_px == u32::from(net.out_px)
            && k.step == [u32::from(net.step_x), u32::from(net.step_y)]
            && k.srgb == srgb
            && k.f32_luma == f32_luma
    })
}

/// The word `i` of a push block.
fn word(push: &[u8], i: usize) -> Option<u32> {
    let b: [u8; 4] = push.get(4 * i..4 * i + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(b))
}

/// The plan of a vr_light program for an input of `in_size` pixels: the weights repacked to the canonical layout and
/// one dispatch.
///
/// # Errors
///
/// When the program has no kernel, the blob is not its length, the push block is shorter than 29 words, or its tile
/// (words 18, 19) is not the kernel's.
pub fn plan(net: &Net, blob: &[u8], in_size: [u32; 2], push: &[u8]) -> Result<Plan, String> {
    let kernel = find(net).ok_or_else(|| format!("{}: no vr_light kernel", net.key))?;
    if blob.len() != kernel.layers as usize * pack::LAYER_BYTES {
        return Err(format!(
            "{}: a blob of {:#x} bytes, the network has {} layers",
            net.key,
            blob.len(),
            kernel.layers
        ));
    }
    if in_size.contains(&0) {
        return Err(format!("{}: input {in_size:?} has a zero side", net.key));
    }
    let tile = [word(push, 18), word(push, 19)];
    let [Some(tw), Some(th)] = tile else {
        return Err(format!("{}: the push block has no words 18, 19", net.key));
    };
    if word(push, 28).is_none() {
        return Err(format!(
            "{}: the push block is shorter than 29 words",
            net.key
        ));
    }
    if [tw, th] != kernel.tile() {
        return Err(format!(
            "{}: the game's tile {tw}x{th} is not the kernel's {:?}",
            net.key,
            kernel.tile()
        ));
    }
    let packed = pack::from_vr(blob)?;
    let bind = |index, source, writes| Binding {
        index,
        source,
        writes,
    };
    Ok(Plan {
        buffers: vec![Buffer {
            name: "ndsr.weights",
            bytes: packed.len() as u64,
            init: Some(packed),
        }],
        steps: vec![Step {
            kernel: kernel.name,
            groups: kernel.groups(in_size),
            threads: [kernel.threads(), 1, 1],
            threadgroup_bytes: 0,
            buffers: vec![
                bind(0, Source::Buffer(0), false),
                bind(1, Source::Game(PUSH), false),
            ],
            textures: vec![
                bind(0, Source::Game(TEX0), false),
                bind(1, Source::Game(OUT), true),
            ],
        }],
    })
}

#[cfg(test)]
#[path = "light_tests.rs"]
mod tests;
