//! The texture the network reads and the sampler of the model.

/// What the model asks of the game's texture and sampler (section 7: the sampler is the game's, R). Values are RGBA.
pub trait Texture {
    /// The texel at integer coordinates; outside the picture the address mode decides (clamp to edge by default).
    fn texel(&self, x: i64, y: i64) -> [f32; 4];
    /// The filtered sample at normalized coordinates, lod 0, as exact doubles (the caller rounds to f16 or f32).
    fn sample(&self, u: f32, v: f32) -> [f64; 4];
}

/// A picture in memory, sampled with bilinear filtering and clamp to edge.
///
/// The filter is the model's: `(1-a)(1-b) t00 + a(1-b) t10 + (1-a) b t01 + a b t11` in doubles, left to right, with
/// `a`, `b` the fractions of `u w - 1/2` and `v h - 1/2`. The texture unit's 8-bit weights are not modelled.
#[derive(Clone, Debug)]
pub struct Picture {
    /// Width in texels.
    pub width: usize,
    /// Height in texels.
    pub height: usize,
    /// Row-major texels.
    pub data: Vec<[f32; 4]>,
}

impl Picture {
    /// A picture whose texel `(x, y)` is `f(x, y)`.
    pub fn from_fn(width: usize, height: usize, f: impl Fn(usize, usize) -> [f32; 4]) -> Picture {
        let data = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| f(x, y));
        Picture {
            width,
            height,
            data: data.collect(),
        }
    }
}

impl Texture for Picture {
    fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        let cx = x.clamp(0, self.width as i64 - 1) as usize;
        let cy = y.clamp(0, self.height as i64 - 1) as usize;
        self.data[cy * self.width + cx]
    }

    fn sample(&self, u: f32, v: f32) -> [f64; 4] {
        let fx = f64::from(u) * self.width as f64 - 0.5;
        let fy = f64::from(v) * self.height as f64 - 0.5;
        let (i0, j0) = (fx.floor(), fy.floor());
        let (a, b) = (fx - i0, fy - j0);
        let (i0, j0) = (i0 as i64, j0 as i64);
        let t00 = self.texel(i0, j0);
        let t10 = self.texel(i0 + 1, j0);
        let t01 = self.texel(i0, j0 + 1);
        let t11 = self.texel(i0 + 1, j0 + 1);
        let c = |k: usize| {
            (1.0 - a) * (1.0 - b) * f64::from(t00[k])
                + a * (1.0 - b) * f64::from(t10[k])
                + (1.0 - a) * b * f64::from(t01[k])
                + a * b * f64::from(t11[k])
        };
        [c(0), c(1), c(2), c(3)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_to_the_edge() {
        let p = Picture::from_fn(3, 2, |x, y| [x as f32, y as f32, 0.0, 1.0]);
        assert_eq!(p.texel(-5, 7), [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(p.texel(9, -1), [2.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn bilinear_at_centres_and_between() {
        let p = Picture::from_fn(4, 4, |x, y| [x as f32, y as f32, 0.0, 1.0]);
        // At a texel centre the sample is the texel.
        assert_eq!(p.sample(1.5 / 4.0, 2.5 / 4.0), [1.0, 2.0, 0.0, 1.0]);
        // Halfway between two texels it is their mean.
        assert_eq!(p.sample(2.0 / 4.0, 2.5 / 4.0)[0], 1.5);
        // Left of the first centre the edge texel is repeated.
        assert_eq!(p.sample(0.0, 0.5)[0], 0.0);
    }
}
