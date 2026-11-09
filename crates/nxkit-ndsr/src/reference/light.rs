//! The vr_light network programs (`docs/plans/ndsr-native.md` section 15): a port of the Python model `refl.py`,
//! step for step. The model equals a lane-exact emulation of the game's SASS on every output pixel of 12 threadgroups
//! per program, for all 15 programs (V emulated).
//!
//! Unlike the vr programs the light ones resample nothing to an arbitrary size: the network's grid is the output tile
//! (`w18 x w19` pixels), looked up bilinearly at a position the push words give (`w20 .. w23`), all in f16:
//!
//! 1. window: f16 luma of the sqrt-RGB texels (`hfma(G, .7148, hfma(R, .2126, B .0722))` in f16, or for the programs
//!    that load with `TLD` the f32 luma rounded to f16);
//! 2. network: the vr network, six layers or three (the 4/3 model), the grid cell `N[y][x]` is the result as is;
//! 3. per pixel: `up` = the grid at `(fma(x, w20, w22), fma(y, w21, w23))` (f16 lerps), `t = S + up` per channel with
//!    `S` the f16 colour sample at the pixel centre, `lin = t |t|`, its f16 luma `L`;
//! 4. sharpening: `d = w28/2 L - w28/8 (4 neighbours)` with the tile's edge clamped, `out = lin + d` (f16), then the
//!    sRGB encoding for variant A.

use super::arith::{fma, h, mul};
use super::network::{self, Weights};
use super::output::srgb;
use super::picture::Texture;
use super::{Error, Grid, Image, Pixel};
use crate::net::{Net, Pack, Variant};

/// The push words a vr_light program reads (section 15.4), as values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightPush {
    /// Output offset added to the written pixel position (words 4, 5).
    pub offset: (i32, i32),
    /// The output tile of a threadgroup in pixels (words 18, 19).
    pub tile: (u32, u32),
    /// Grid cells per output pixel (words 20, 21).
    pub scale: [f32; 2],
    /// The grid position of the tile's first pixel (words 22, 23).
    pub origin: [f32; 2],
    /// `W_out`, `H_out` (words 24, 25): no pixel at or beyond them is written.
    pub out: (u32, u32),
    /// `1 / W_out`, `1 / H_out` (words 26, 27): the colour sample's coordinates.
    pub rcp_out: [f32; 2],
    /// Sharpening strength (word 28).
    pub strength: f32,
}

impl LightPush {
    /// The values of the words of a push block (at least 29 words).
    pub fn from_words(w: &[u32]) -> Option<LightPush> {
        let f = |i: usize| f32::from_bits(w[i]);
        (w.len() >= 29).then(|| LightPush {
            offset: (w[4] as i32, w[5] as i32),
            tile: (w[18], w[19]),
            scale: [f(20), f(21)],
            origin: [f(22), f(23)],
            out: (w[24], w[25]),
            rcp_out: [f(26), f(27)],
            strength: f(28),
        })
    }

    /// The 29 words: the read ones set, the others as the game leaves them for an input of `in_size` (words 2, 3, 6,
    /// 7 and 8, 9 are what Ocarina of Time writes; the programs here do not read them).
    pub fn words(&self, in_size: [u32; 2]) -> [u32; 29] {
        let mut w = [0u32; 29];
        w[2] = in_size[0] - 1;
        w[3] = in_size[1] - 1;
        w[4] = self.offset.0 as u32;
        w[5] = self.offset.1 as u32;
        w[6] = self.out.0 - 1;
        w[7] = self.out.1 - 1;
        w[8] = (1.0 / in_size[0] as f32).to_bits();
        w[9] = (1.0 / in_size[1] as f32).to_bits();
        w[18] = self.tile.0;
        w[19] = self.tile.1;
        w[20] = self.scale[0].to_bits();
        w[21] = self.scale[1].to_bits();
        w[22] = self.origin[0].to_bits();
        w[23] = self.origin[1].to_bits();
        w[24] = self.out.0;
        w[25] = self.out.1;
        w[26] = self.rcp_out[0].to_bits();
        w[27] = self.rcp_out[1].to_bits();
        w[28] = self.strength.to_bits();
        w
    }
}

/// What one threadgroup produces: the network's grid and the pixels, rows in order.
#[derive(Clone, Debug)]
pub struct LightTile {
    /// The grid `N`: the network's result, `out` cells per patch.
    pub grid: Grid,
    /// The written pixels.
    pub pixels: Vec<Pixel>,
}

fn hf(x: f64) -> f32 {
    h(x)
}
/// `a * b + c` of halves: the exact value rounded to a double, then to a half (as the model and the emulator do).
fn hfma(a: f32, b: f32, c: f32) -> f32 {
    hf(f64::from(a).mul_add(f64::from(b), f64::from(c)))
}
fn hmul(a: f32, b: f32) -> f32 {
    hf(f64::from(a) * f64::from(b))
}
fn hadd(a: f32, b: f32) -> f32 {
    hf(f64::from(a) + f64::from(b))
}

/// The f16 luma weights of R, G, B: 0x32cd, 0x39b8, 0x2c9e (0.2126, 0.7152, 0.0722 rounded).
fn k_rgb() -> [f32; 3] {
    [0x32cd, 0x39b8, 0x2c9e].map(crate::half::to_f32)
}

/// The input element of the f16 programs: the luma of the f16 channels, B first.
pub fn luma_in(t: [f32; 4]) -> f32 {
    let [r, g, b] = [t[0], t[1], t[2]].map(|c| hf(f64::from(c)));
    let [kr, kg, kb] = k_rgb();
    hfma(g, kg, hfma(r, kr, hmul(b, kb)))
}

/// The input element of the `TLD` programs: the f32 luma of section 4 (agl's constants), G first, then f16.
pub fn luma_in_f32(t: [f32; 4]) -> f32 {
    let b = |v: u32| f32::from_bits(v);
    hf(f64::from(fma(
        t[2],
        b(0x3d93_dd98),
        fma(t[0], b(0x3e59_b3d0), mul(t[1], b(0x3f37_1759))),
    )))
}

/// The luma of the linear result: G first, then R, then B.
pub fn luma_out(l: [f32; 3]) -> f32 {
    let [kr, kg, kb] = k_rgb();
    hfma(l[2], kb, hfma(l[0], kr, hmul(l[1], kg)))
}

/// The f16 lerp of the program: `fma(f, b, fma(-f, a, a))`.
pub fn lerp(a: f32, b: f32, f: f32) -> f32 {
    hfma(f, b, hfma(-f, a, a))
}

/// Whether the program loads its window with `TLD` and computes the luma in f32 (the 4/3 variant A and the 3.0
/// variant C); the others gather f16 texels with `TLD4`.
pub fn loads_f32(net: &Net) -> bool {
    matches!(net.key, "927ceaf3cf1be240" | "d99fbeeb53d38847")
}

/// The grid cell index and fraction of one axis: `floor`, clamped so that the right neighbour stays in the grid
/// (`fraction` 1 then). Below the grid (not seen in the game) it clamps to the first cell (I).
fn axis(t: f32, n: usize) -> (usize, f32) {
    let i = t.floor();
    if i > (n - 2) as f32 {
        (n - 2, 1.0)
    } else if i < 0.0 {
        (0, 0.0)
    } else {
        (i as usize, hf(f64::from(t - i)))
    }
}

/// Run threadgroup `(wx, wy)` of a vr_light program.
pub fn run_tile(
    net: &Net,
    weights: &Weights,
    push: &LightPush,
    tex: &dyn Texture,
    wx: usize,
    wy: usize,
) -> Result<LightTile, Error> {
    if net.pack != Pack::VrLight {
        return Err(Error::NotVr);
    }
    if weights.layers() != net.blob().layers() {
        return Err(Error::BadBlob);
    }
    let (nin, nout) = (usize::from(net.in_px), usize::from(net.out_px));
    let (npx, npy) = (net.patches_x(), net.patches_y());
    let (ww, wh) = net.window_size();
    let m = net.margin() as i64;
    let (ox, oy) = (
        (usize::from(net.step_x) * wx) as i64 - m,
        (usize::from(net.step_y) * wy) as i64 - m,
    );
    let element = if loads_f32(net) { luma_in_f32 } else { luma_in };
    let win: Vec<f32> = (0..wh)
        .flat_map(|j| (0..ww).map(move |i| (i, j)))
        .map(|(i, j)| element(tex.texel(ox + i as i64, oy + j as i64)))
        .collect();
    let mut grid = Grid::new(nout * npx, nout * npy);
    let odd = weights.layers() % 2 == 1;
    for p in 0..npy {
        for q in 0..npx {
            let x = std::array::from_fn(|k| {
                std::array::from_fn(|n| win[(nin * p + n) * ww + nin * q + k])
            });
            let y = network::network(weights, &x);
            let at = |r: usize, c: usize| if odd { y[c][r] } else { y[r][c] };
            for r in 0..nout {
                for c in 0..nout {
                    grid.cells[(nout * p + r) * grid.width + nout * q + c] = at(r, c);
                }
            }
        }
    }

    let (tw, th) = (push.tile.0 as usize, push.tile.1 as usize);
    let (x0, y0) = (tw * wx, th * wy);
    let mut lin = vec![[0.0f32; 3]; tw * th];
    let mut lum = vec![0.0f32; tw * th];
    for yl in 0..th {
        for xl in 0..tw {
            let gx = fma(xl as f32, push.scale[0], push.origin[0]);
            let gy = fma(yl as f32, push.scale[1], push.origin[1]);
            let ((ix, fx), (iy, fy)) = (axis(gx, grid.width), axis(gy, grid.height));
            let top = lerp(grid.at(iy, ix), grid.at(iy, ix + 1), fx);
            let bot = lerp(grid.at(iy + 1, ix), grid.at(iy + 1, ix + 1), fx);
            let up = lerp(top, bot, fy);
            let half_px = |r: f32| mul(0.5, r);
            let u = fma((x0 + xl) as f32, push.rcp_out[0], half_px(push.rcp_out[0]));
            let v = fma((y0 + yl) as f32, push.rcp_out[1], half_px(push.rcp_out[1]));
            let s = tex.sample(u, v);
            let l: [f32; 3] = std::array::from_fn(|c| {
                let t = hadd(h(s[c]), up);
                hmul(t, t.abs())
            });
            lin[yl * tw + xl] = l;
            lum[yl * tw + xl] = luma_out(l);
        }
    }

    let k1 = h(f64::from(mul(push.strength, 0.5)));
    let k2 = h(f64::from(mul(push.strength, -0.125)));
    let at = |x: i64, y: i64| {
        lum[y.clamp(0, th as i64 - 1) as usize * tw + x.clamp(0, tw as i64 - 1) as usize]
    };
    let nx = tw.min((push.out.0 as usize).saturating_sub(x0));
    let ny = th.min((push.out.1 as usize).saturating_sub(y0));
    let mut pixels = Vec::with_capacity(nx * ny);
    for yl in 0..ny {
        for xl in 0..nx {
            let (x, y) = (xl as i64, yl as i64);
            let left = hfma(at(x - 1, y), k2, hmul(at(x + 1, y), k2));
            let centre = hfma(at(x, y), k1, hmul(at(x, y + 1), k2));
            let d = hfma(at(x, y - 1), k2, hadd(left, centre));
            let mut o = lin[yl * tw + xl].map(|c| 0.0 + hadd(c, d));
            if net.variant == Variant::A {
                o = o.map(srgb);
            }
            pixels.push(Pixel {
                x: (x0 + xl) as i32 + push.offset.0,
                y: (y0 + yl) as i32 + push.offset.1,
                rgba: [o[0], o[1], o[2], 1.0],
            });
        }
    }
    Ok(LightTile { grid, pixels })
}

/// The threadgroups `(x, y)` of the game's dispatch for an input of `in_size` pixels: `ceil(W_in / b0) x ceil(H_in / b1)`.
pub fn groups(net: &Net, in_size: [u32; 2]) -> (usize, usize) {
    (
        (in_size[0] as usize).div_ceil(usize::from(net.step_x)),
        (in_size[1] as usize).div_ceil(usize::from(net.step_y)),
    )
}

/// Run every threadgroup and assemble the `W_out x H_out` image (without the offset); unwritten pixels stay NaN.
pub fn run_image(
    net: &Net,
    blob: &[u8],
    push: &LightPush,
    tex: &dyn Texture,
    in_size: [u32; 2],
) -> Result<Image, Error> {
    let weights = Weights::from_vr_blob(blob).ok_or(Error::BadBlob)?;
    let (width, height) = (push.out.0 as usize, push.out.1 as usize);
    let mut rgba = vec![[f32::NAN; 4]; width * height];
    let (gx, gy) = groups(net, in_size);
    for wy in 0..gy {
        for wx in 0..gx {
            for p in run_tile(net, &weights, push, tex, wx, wy)?.pixels {
                let (x, y) = (p.x - push.offset.0, p.y - push.offset.1);
                rgba[y as usize * width + x as usize] = p.rgba;
            }
        }
    }
    Ok(Image {
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
#[path = "light_tests.rs"]
mod tests;
