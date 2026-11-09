//! The threadgroup's grid (section 7.4) and the unsharp mask (section 7.5).

use super::Push;
use super::arith::{Mat, add, fma, h, mul, rcp};
use super::picture::Texture;
use crate::net::{Net, VrConsts};

/// A grid of halves (as `f32`), row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    /// Cells per row.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// `height * width` cells.
    pub cells: Vec<f32>,
}

impl Grid {
    /// A grid of zeros.
    pub fn new(width: usize, height: usize) -> Grid {
        Grid {
            width,
            height,
            cells: vec![0.0; width * height],
        }
    }

    /// The cell at row `i`, column `j`.
    pub fn at(&self, i: usize, j: usize) -> f32 {
        self.cells[i * self.width + j]
    }
}

/// The cell-centre offsets of one axis, in input pixels relative to the tile: `(j + 1/2) / R` with the compiler's
/// folding (section 7.4): `fma(j, s, h)` for even `j`, `fma(j - 1, s, h1)` for odd `j`, or `fma(j, s, h)` for all.
pub fn cell_centre(c: &VrConsts, j: usize, odd_bits: u32, every: bool) -> f32 {
    let (s, half) = (f32::from_bits(c.step_bits), f32::from_bits(c.half_bits));
    if every || j.is_multiple_of(2) {
        fma(j as f32, s, half)
    } else {
        fma((j - 1) as f32, s, f32::from_bits(odd_bits))
    }
}

/// Build the grid `G` of the threadgroup `(wx, wy)` from the network outputs. `ys[p * npx + q]` is the result of patch
/// `(q, p)`. The cell is `f16(s |s|)` with `s = S + Y`, `S` the f16 bilinear sample of the texture at the cell centre.
pub fn build(
    net: &Net,
    c: &VrConsts,
    push: &Push,
    tex: &dyn Texture,
    wx: usize,
    wy: usize,
    ys: &[Mat],
) -> Grid {
    let out = usize::from(net.out_px);
    let (npx, npy) = (net.patches_x(), net.patches_y());
    let mut g = Grid::new(out * npx, out * npy);
    let (r_w, r_h) = (rcp(push.w_in as f32), rcp(push.h_in as f32));
    let (ox, oy) = (
        (usize::from(net.step_x) * wx) as f32,
        (usize::from(net.step_y) * wy) as f32,
    );
    let xs: Vec<f32> = (0..g.width)
        .map(|j| cell_centre(c, j, c.odd_x_bits, false))
        .collect();
    let ys_c: Vec<f32> = (0..g.height)
        .map(|i| cell_centre(c, i, c.odd_y_bits, c.y_every_row))
        .collect();
    for p in 0..npy {
        for q in 0..npx {
            let y = &ys[p * npx + q];
            for (r, row) in y.iter().enumerate().take(out) {
                for (col, yv) in row.iter().enumerate().take(out) {
                    let (i, j) = (out * p + r, out * q + col);
                    let u = mul(add(xs[j], ox), r_w);
                    let v = mul(add(ys_c[i], oy), r_h);
                    let base = h(tex.sample(u, v)[1]);
                    let s = add(base, *yv);
                    g.cells[i * g.width + j] = h(f64::from(mul(s, s.abs())));
                }
            }
        }
    }
    g
}

/// The unsharp mask: centre `f16(1 + w/2)`, four neighbours `f16(-w/8)`, mirrored in x and clamped in y at the edge.
pub fn unsharp(g: &Grid, strength: f32) -> Grid {
    let k1 = h(f64::from(fma(strength, 0.5, 1.0)));
    let k2 = h(f64::from(mul(strength, -0.125)));
    let (gw, gh) = (g.width as i64, g.height as i64);
    let at = |i: i64, j: i64| -> f64 {
        let i = i.clamp(0, gh - 1) as usize;
        let j = if j < 0 {
            -j
        } else if j > gw - 1 {
            2 * (gw - 1) - j
        } else {
            j
        } as usize;
        f64::from(g.at(i, j))
    };
    let (k1, k2) = (f64::from(k1), f64::from(k2));
    let mut out = Grid::new(g.width, g.height);
    for i in 0..gh {
        for j in 0..gw {
            let t1 = f64::from(h(at(i, j) * k1));
            let t2 = f64::from(h(at(i, j + 1) * k2 + t1));
            let t3 = f64::from(h(at(i - 1, j) + at(i, j - 1)));
            let t4 = f64::from(h(t3 * k2));
            let t5 = f64::from(h(at(i + 1, j) * k2 + t4));
            out.cells[(i * gw + j) as usize] = h(t2 + t5);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_strength_is_the_identity() {
        let mut g = Grid::new(5, 4);
        for (i, c) in g.cells.iter_mut().enumerate() {
            *c = (i as f32 - 7.0) * 0.125;
        }
        assert_eq!(unsharp(&g, 0.0), g);
    }

    #[test]
    fn a_flat_grid_stays_flat_for_any_strength() {
        let mut g = Grid::new(6, 6);
        g.cells.fill(0.5);
        for s in [0.25, 0.5, 1.0] {
            assert_eq!(unsharp(&g, s), g, "{s}");
        }
    }

    #[test]
    fn edges_mirror_in_x_and_clamp_in_y() {
        // One bright cell in the first column. Mirrored: its left neighbour (column -1) is column 1, which is dark, and
        // the cell at column 1 sees the bright one as its left neighbour.
        let mut g = Grid::new(4, 3);
        g.cells[4] = 1.0; // row 1, column 0
        let u = unsharp(&g, 1.0); // k1 = 1.5, k2 = -0.125
        assert_eq!(u.at(1, 0), 1.5);
        assert_eq!(u.at(1, 1), -0.125);
        assert_eq!(u.at(0, 0), -0.125);
        // A bright cell in row 0, column 1. Column 0 sees it as its right neighbour and, mirrored, as its left one too
        // (-2 x 0.125). Row 0 clamps: the bright cell is its own up neighbour (1.5 - 0.125 for the down term only).
        let mut g = Grid::new(4, 3);
        g.cells[1] = 1.0;
        let u = unsharp(&g, 1.0);
        assert_eq!(u.at(0, 0), -0.25);
        assert_eq!(u.at(0, 1), 1.375);
    }
}
