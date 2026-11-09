//! The CPU reference of the vr network programs: a port of the Python model of `docs/plans/ndsr-native.md` section 7,
//! step for step (window, patches, six layers with an f16 rounding after every HMMA, grid with the f16 base sample,
//! unsharp mask, tile rule, resample, chroma, encoding, hash dither).
//!
//! The model equals a lane-exact emulation of the game's SASS on every output pixel it was tested on (V emulated).
//! The unknowns of section 13 (the HMMA's internal rounding, the MUFU error, the texture unit's weights) are modelled
//! as exact: [`arith::hmma`] rounds the exact sum once, the sampler of [`Picture`] is exact bilinear with clamp to edge.
//! The sampler is a parameter ([`Texture`]).
//!
//! A threadgroup is checked in three parts: the grid before the unsharp pass ([`Tile::grid`]), after it
//! ([`Tile::sharp`]), and the pixels ([`Tile::pixels`]).

pub mod arith;
pub mod grid;
pub mod network;
pub mod output;
pub mod picture;

#[cfg(test)]
mod tests;

pub use grid::Grid;
pub use network::Weights;
pub use picture::{Picture, Texture};

use crate::half;
use crate::net::{Net, Pack};
use arith::Mat;

/// The push words a vr program reads (section 7.9), as values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Push {
    /// `W_in`: words 2 and 3 are `W_in - 1`, `H_in - 1`.
    pub w_in: u32,
    /// `H_in`.
    pub h_in: u32,
    /// `W_out` (words 6, 7 are `W_out - 1`, `H_out - 1`).
    pub w_out: u32,
    /// `H_out`.
    pub h_out: u32,
    /// Output offset added to the written pixel position (words 4, 5).
    pub offset: (i32, i32),
    /// Unsharp strength (word 18).
    pub unsharp: f32,
    /// Dither on (word 19 != 0).
    pub dither: bool,
}

/// One written pixel: the position after the offset and the value of the four channels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pixel {
    /// Column.
    pub x: i32,
    /// Row.
    pub y: i32,
    /// RGBA before the image format's quantisation.
    pub rgba: [f32; 4],
}

/// What one threadgroup produces.
#[derive(Clone, Debug)]
pub struct Tile {
    /// The grid `G`, before the unsharp mask.
    pub grid: Grid,
    /// The grid `H`, after it.
    pub sharp: Grid,
    /// The pixels of the tile, rows in order.
    pub pixels: Vec<Pixel>,
}

/// Why the reference refuses a program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Only vr programs are modelled.
    NotVr,
    /// The blob is not 0x3000 bytes.
    BadBlob,
}

/// The number of threadgroups `(x, y)` the dispatch needs: `ceil(W_in / b0) x ceil(H_in / b1)`.
pub fn groups(net: &Net, push: &Push) -> (usize, usize) {
    (
        (push.w_in as usize).div_ceil(usize::from(net.step_x)),
        (push.h_in as usize).div_ceil(usize::from(net.step_y)),
    )
}

/// Run threadgroup `(wx, wy)` of a vr program.
pub fn run_tile(
    net: &Net,
    weights: &Weights,
    push: &Push,
    tex: &dyn Texture,
    wx: usize,
    wy: usize,
) -> Result<Tile, Error> {
    let consts = net
        .vr
        .as_ref()
        .filter(|_| net.pack == Pack::Vr)
        .ok_or(Error::NotVr)?;
    let (nin, margin) = (usize::from(net.in_px), net.margin() as i64);
    let (npx, npy) = (net.patches_x(), net.patches_y());
    let (ww, wh) = net.window_size();
    // The window of halves of the texture's second channel, from (b0 wx - m, b1 wy - m).
    let (ox, oy) = (
        (usize::from(net.step_x) * wx) as i64 - margin,
        (usize::from(net.step_y) * wy) as i64 - margin,
    );
    let mut window = vec![0.0f32; ww * wh];
    for j in 0..wh {
        for i in 0..ww {
            let v = tex.texel(ox + i as i64, oy + j as i64)[1];
            window[j * ww + i] = half::to_f32(half::from_f32(v));
        }
    }
    let mut ys: Vec<Mat> = Vec::with_capacity(npx * npy);
    for p in 0..npy {
        for q in 0..npx {
            let x: Mat = std::array::from_fn(|k| {
                std::array::from_fn(|n| window[(nin * p + n) * ww + nin * q + k])
            });
            ys.push(network::network(weights, &x));
        }
    }
    let grid = grid::build(net, consts, push, tex, wx, wy, &ys);
    let sharp = grid::unsharp(&grid, push.unsharp);
    let pixels = output::pixels(net, consts, push, tex, &sharp, (wx, wy));
    Ok(Tile {
        grid,
        sharp,
        pixels,
    })
}

/// The whole output image: every threadgroup, pixels in `(y, x)` order of the output (without the offset).
#[derive(Clone, Debug)]
pub struct Image {
    /// `W_out`.
    pub width: usize,
    /// `H_out`.
    pub height: usize,
    /// RGBA, row-major.
    pub rgba: Vec<[f32; 4]>,
}

/// Run every threadgroup of the dispatch and assemble the image.
pub fn run_image(net: &Net, blob: &[u8], push: &Push, tex: &dyn Texture) -> Result<Image, Error> {
    let weights = Weights::from_vr_blob(blob).ok_or(Error::BadBlob)?;
    let (width, height) = (push.w_out as usize, push.h_out as usize);
    let mut rgba = vec![[f32::NAN; 4]; width * height];
    let (gx, gy) = groups(net, push);
    for wy in 0..gy {
        for wx in 0..gx {
            for p in run_tile(net, &weights, push, tex, wx, wy)?.pixels {
                let (x, y) = (
                    (p.x - push.offset.0) as usize,
                    (p.y - push.offset.1) as usize,
                );
                rgba[y * width + x] = p.rgba;
            }
        }
    }
    Ok(Image {
        width,
        height,
        rgba,
    })
}
