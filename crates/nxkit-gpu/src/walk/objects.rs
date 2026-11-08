//! The state objects that a bind carries, read into the fields of [`crate::detail`].
//!
//! The bit layouts are those of the state group in `nxkit-rt` (`nvn/state.rs`). They are stated here again because
//! that group is private to the FFI layer. A change there needs the same change here.

use crate::detail::{BlendDetail, DepthStencilDetail, RasterDetail, StencilDetail};

/// The faces of `NVNface` that a stencil setting names.
const FRONT: u32 = 1;
const BACK: u32 = 2;

fn bits(v: u64, at: u32, n: u32) -> u32 {
    ((v >> at) & ((1u64 << n) - 1)) as u32
}

fn pair(words: &[u32]) -> u64 {
    u64::from(words.first().copied().unwrap_or(0))
        | u64::from(words.get(1).copied().unwrap_or(0)) << 32
}

/// The blend state that the walker keeps: one object per target.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Blends {
    raw: [u64; 8],
    enable: u8,
}

impl Blends {
    /// The target that an `NVNblendState` object sets.
    pub(super) fn target(words: &[u32]) -> usize {
        bits(pair(words), 0, 3) as usize
    }

    /// The object of a target as the walker keeps it.
    pub(super) fn raw(&self, target: usize) -> u64 {
        self.raw.get(target).copied().unwrap_or(0)
    }

    /// Sets the object of a target.
    pub(super) fn set_raw(&mut self, target: usize, v: u64) {
        if let Some(r) = self.raw.get_mut(target) {
            *r = v;
        }
    }

    /// The blend enable bits of the targets.
    pub(super) fn enable(&self) -> u8 {
        self.enable
    }

    /// Sets the blend enable bits.
    pub(super) fn set_enable(&mut self, v: u8) {
        self.enable = v;
    }

    /// `NVNblendState`. The target is in the object.
    pub(super) fn set_blend(&mut self, words: &[u32]) {
        let v = pair(words);
        self.raw[bits(v, 0, 3) as usize] = v;
    }

    /// `NVNcolorState`: the blend enable bits of the targets.
    pub(super) fn set_color(&mut self, words: &[u32]) {
        self.enable = words.first().map_or(0, |w| (*w & 0xff) as u8);
    }

    pub(super) fn detail(&self) -> [BlendDetail; 8] {
        let mut out = [BlendDetail::default(); 8];
        for (i, d) in out.iter_mut().enumerate() {
            let v = self.raw[i];
            *d = BlendDetail {
                enable: self.enable >> i & 1 != 0,
                src_rgb: bits(v, 3, 7),
                dst_rgb: bits(v, 10, 7),
                src_alpha: bits(v, 17, 7),
                dst_alpha: bits(v, 24, 7),
                op_rgb: bits(v, 31, 3),
                op_alpha: bits(v, 34, 3),
            };
        }
        out
    }
}

/// The write mask of each target from an `NVNchannelMaskState`.
pub(super) fn write_masks(words: &[u32]) -> [u8; 8] {
    let w = words.first().copied().unwrap_or(0xffff_ffff);
    let mut out = [0u8; 8];
    for (i, m) in out.iter_mut().enumerate() {
        *m = (w >> (4 * i) & 0xf) as u8;
    }
    out
}

/// The stencil masks and references that settings give.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct StencilValues {
    pub(super) read: [u8; 2],
    pub(super) write: [u8; 2],
    pub(super) reference: [u32; 2],
}

impl Default for StencilValues {
    fn default() -> Self {
        Self {
            read: [0xff; 2],
            write: [0xff; 2],
            reference: [0; 2],
        }
    }
}

impl StencilValues {
    /// The faces of an `NVNface` value as bits: bit 0 is the front face and bit 1 the back face.
    pub(super) fn face_bits(face: u32) -> u8 {
        Self::faces(face).fold(0, |m, i| m | 1 << i)
    }

    fn faces(face: u32) -> impl Iterator<Item = usize> {
        [(FRONT, 0), (BACK, 1)]
            .into_iter()
            .filter(move |(f, _)| face & f != 0)
            .map(|(_, i)| i)
    }

    pub(super) fn set_read(&mut self, face: u32, mask: u32) {
        Self::faces(face).for_each(|i| self.read[i] = mask as u8);
    }

    pub(super) fn set_write(&mut self, face: u32, mask: u32) {
        Self::faces(face).for_each(|i| self.write[i] = mask as u8);
    }

    pub(super) fn set_ref(&mut self, face: u32, value: u32) {
        Self::faces(face).for_each(|i| self.reference[i] = value);
    }
}

/// An `NVNdepthStencilState` with the stencil values of the settings.
pub(super) fn depth_stencil(words: &[u32], s: &StencilValues) -> DepthStencilDetail {
    let v = pair(words);
    let face = |func_at: u32, fail_at: u32, i: usize| StencilDetail {
        func: bits(v, func_at, 4),
        fail: bits(v, fail_at, 4),
        depth_fail: bits(v, fail_at + 4, 4),
        pass: bits(v, fail_at + 8, 4),
        read_mask: s.read[i],
        write_mask: s.write[i],
    };
    DepthStencilDetail {
        depth_test: bits(v, 0, 1) != 0,
        depth_write: bits(v, 1, 1) != 0,
        depth_func: bits(v, 2, 4),
        stencil_test: bits(v, 6, 1) != 0,
        front: face(7, 15, 0),
        back: face(11, 27, 1),
    }
}

/// The raster state that the objects and settings give.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Raster {
    pub(super) cull: u32,
    pub(super) front_face: u32,
    pub(super) polygon_mode: u32,
    pub(super) offset_on: bool,
    pub(super) offset: [f32; 3],
    pub(super) clamp: bool,
    pub(super) alpha_to_coverage: bool,
}

impl Raster {
    /// `NVNpolygonState`.
    pub(super) fn set_polygon(&mut self, words: &[u32]) {
        let w = u64::from(words.first().copied().unwrap_or(0));
        self.cull = bits(w, 0, 2);
        self.front_face = bits(w, 2, 1);
        self.polygon_mode = bits(w, 3, 2);
        self.offset_on = bits(w, 5, 3) != 0;
    }

    /// `NVNmultisampleState`: the third byte is the alpha to coverage enable.
    pub(super) fn set_multisample(&mut self, words: &[u32]) {
        self.alpha_to_coverage = words.first().is_some_and(|w| (*w >> 16) & 0xff != 0);
    }

    pub(super) fn detail(&self) -> RasterDetail {
        RasterDetail {
            cull: self.cull,
            front_face: self.front_face,
            polygon_mode: self.polygon_mode,
            depth_bias: (self.offset_on && self.offset != [0.0; 3]).then_some(self.offset),
            depth_clamp: self.clamp,
            alpha_to_coverage: self.alpha_to_coverage,
        }
    }
}

/// One vertex attribute from its `NVNvertexAttribState` word: format, offset and stream.
pub(super) fn attribute(word: u32) -> (u32, u32, u32) {
    let v = u64::from(word);
    (bits(v, 0, 8), bits(v, 8, 19), bits(v, 27, 5))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blend_object_gives_its_factors_to_its_target() {
        let v: u64 = 2 | 4 << 3 | 5 << 10 | 4 << 17 | 5 << 24 | 1 << 31 | 1 << 34;
        let mut b = Blends::default();
        b.set_blend(&[v as u32, (v >> 32) as u32]);
        b.set_color(&[0b100]);
        let d = b.detail();
        assert!(d[2].enable && !d[1].enable);
        assert_eq!((d[2].src_rgb, d[2].dst_rgb, d[2].op_rgb), (4, 5, 1));
        assert_eq!((d[2].src_alpha, d[2].dst_alpha, d[2].op_alpha), (4, 5, 1));
    }

    #[test]
    fn stencil_values_go_to_the_named_faces() {
        let mut s = StencilValues::default();
        s.set_ref(FRONT, 7);
        s.set_write(FRONT | BACK, 0x0f);
        assert_eq!(s.reference, [7, 0]);
        assert_eq!(s.write, [0x0f, 0x0f]);
    }

    #[test]
    fn a_depth_stencil_object_gives_its_bits() {
        let v: u64 = 1 | 1 << 1 | 3 << 2 | 1 << 6 | 8 << 7 | 2 << 15 | 3 << 19 | 4 << 23;
        let d = depth_stencil(&[v as u32, (v >> 32) as u32], &StencilValues::default());
        assert!(d.depth_test && d.depth_write && d.stencil_test);
        assert_eq!(
            (
                d.depth_func,
                d.front.func,
                d.front.fail,
                d.front.depth_fail,
                d.front.pass
            ),
            (3, 8, 2, 3, 4)
        );
    }
}
