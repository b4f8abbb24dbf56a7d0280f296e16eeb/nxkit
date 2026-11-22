//! What a patch answers at a hook point. The default answer, all zero, changes nothing. When several patches are
//! loaded, the host merges their answers in load order with [`PassAnswer::merge`] and [`DrawAnswer::merge`].

use crate::types::{BlendState, MAX_COLORS, MAX_FORCED};

/// [`PassAnswer::flags`]: leave the pass out. Its draws are not encoded and count as skipped.
pub const PASS_SKIP: u32 = 1;
/// [`PassAnswer::flags`]: start the pass with a barrier that waits for all earlier work of the frame.
pub const PASS_BARRIER: u32 = 2;
/// [`PassAnswer::flags`]: change the load action of the colour slots in `load_slots`.
pub const PASS_LOAD: u32 = 4;
/// [`PassAnswer::flags`]: mark the pass as the focus pass of the capture (`NXKIT_DIAG=lum` captures its stored
/// colour targets). Without that analysis the mark does nothing.
pub const PASS_CAPTURE: u32 = 8;

/// The load actions of [`PassAnswer::load`].
pub const LOAD_KEEP: u32 = 0;
/// Clear to [`PassAnswer::clear`].
pub const LOAD_CLEAR: u32 = 1;
/// Do not care about the old content.
pub const LOAD_DONT_CARE: u32 = 2;

/// The answer to a render pass.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PassAnswer {
    /// The `PASS_*` bits.
    pub flags: u32,
    /// With [`PASS_LOAD`]: the colour slots that change, bit 0 for slot 0.
    pub load_slots: u32,
    /// With [`PASS_LOAD`]: [`LOAD_KEEP`] (load the old content), [`LOAD_CLEAR`] or [`LOAD_DONT_CARE`].
    pub load: u32,
    /// With [`LOAD_CLEAR`]: the clear colour.
    pub clear: [f32; 4],
}

impl PassAnswer {
    /// Adds the answer of a later patch. Flags join. A later load wins.
    pub fn merge(&mut self, later: &PassAnswer) {
        self.flags |= later.flags;
        if later.flags & PASS_LOAD != 0 {
            self.load_slots = later.load_slots;
            self.load = later.load;
            self.clear = later.clear;
        }
    }
}

/// [`DrawAnswer::flags`]: leave the draw out.
pub const DRAW_SKIP: u32 = 1;
/// [`DrawAnswer::flags`]: replace the blend state of the colour slots in `blend_slots`.
pub const DRAW_BLEND: u32 = 2;
/// [`DrawAnswer::flags`]: bind other textures in the first `forced_count` entries of `forced`.
pub const DRAW_TEXTURE: u32 = 4;
/// [`DrawAnswer::flags`]: replace the stencil reference with `stencil_ref`.
pub const DRAW_STENCIL_REF: u32 = 8;

/// One texture slot of a draw that the patch binds to another texture.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ForcedTexture {
    /// The stage, as in `TexBind::stage`.
    pub stage: u32,
    /// The slot, as in `TexBind::slot`.
    pub slot: u32,
    /// The texture to bind. It must be a texture that a draw of this frame binds too, because only the textures
    /// that the frame resolved are known to the encoder. The draw keeps the sampler of the texture it takes.
    pub tex_id: u32,
}

/// The answer to a draw.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DrawAnswer {
    /// The `DRAW_*` bits.
    pub flags: u32,
    /// With [`DRAW_BLEND`]: the colour slots that change, bit 0 for slot 0.
    pub blend_slots: u32,
    /// With [`DRAW_BLEND`]: the new blend state of each slot in `blend_slots`.
    pub blend: [BlendState; MAX_COLORS],
    /// With [`DRAW_TEXTURE`]: the number of valid entries in `forced`.
    pub forced_count: u32,
    /// With [`DRAW_TEXTURE`]: the slots to bind to other textures.
    pub forced: [ForcedTexture; MAX_FORCED],
    /// With [`DRAW_STENCIL_REF`]: the stencil reference of the front and the back face.
    pub stencil_ref: [u32; 2],
}

impl DrawAnswer {
    /// Adds the answer of a later patch. Flags join. A later blend of a slot, a later force of a slot and a later
    /// stencil reference win. Forced slots past [`MAX_FORCED`] are dropped.
    pub fn merge(&mut self, later: &DrawAnswer) {
        self.flags |= later.flags;
        if later.flags & DRAW_BLEND != 0 {
            for slot in 0..MAX_COLORS {
                if later.blend_slots & (1 << slot) != 0 {
                    self.blend[slot] = later.blend[slot];
                    self.blend_slots |= 1 << slot;
                }
            }
        }
        if later.flags & DRAW_TEXTURE != 0 {
            for f in later.forced_list() {
                let n = self.forced_count as usize;
                if let Some(old) = self.forced[..n.min(MAX_FORCED)]
                    .iter_mut()
                    .find(|o| o.stage == f.stage && o.slot == f.slot)
                {
                    *old = *f;
                } else if n < MAX_FORCED {
                    self.forced[n] = *f;
                    self.forced_count += 1;
                }
            }
        }
        if later.flags & DRAW_STENCIL_REF != 0 {
            self.stencil_ref = later.stencil_ref;
        }
    }

    /// Asks to bind `tex_id` in a slot. Returns false when the answer is full.
    pub fn force_texture(&mut self, stage: u32, slot: u32, tex_id: u32) -> bool {
        let n = self.forced_count as usize;
        if n >= MAX_FORCED {
            return false;
        }
        self.forced[n] = ForcedTexture {
            stage,
            slot,
            tex_id,
        };
        self.forced_count += 1;
        self.flags |= DRAW_TEXTURE;
        true
    }

    /// Asks to replace the blend state of colour slot `slot`.
    pub fn set_blend(&mut self, slot: usize, state: BlendState) {
        if slot < MAX_COLORS {
            self.blend[slot] = state;
            self.blend_slots |= 1 << slot;
            self.flags |= DRAW_BLEND;
        }
    }

    /// Asks to leave the draw out.
    pub fn skip(&mut self) {
        self.flags |= DRAW_SKIP;
    }

    /// The valid forced textures.
    pub fn forced_list(&self) -> &[ForcedTexture] {
        &self.forced[..(self.forced_count as usize).min(MAX_FORCED)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_joins_flags_and_later_wins() {
        let mut a = DrawAnswer::default();
        a.set_blend(
            0,
            BlendState {
                enable: 1,
                ..Default::default()
            },
        );
        a.force_texture(1, 0, 5);
        let mut b = DrawAnswer::default();
        b.skip();
        b.set_blend(0, BlendState::default());
        b.set_blend(2, BlendState::default());
        b.force_texture(1, 0, 9);
        b.force_texture(1, 1, 7);
        b.flags |= DRAW_STENCIL_REF;
        b.stencil_ref = [3, 4];
        a.merge(&b);
        assert_eq!(
            a.flags,
            DRAW_SKIP | DRAW_BLEND | DRAW_TEXTURE | DRAW_STENCIL_REF
        );
        assert_eq!(a.blend_slots, 0b101);
        assert_eq!(a.blend[0].enable, 0);
        assert_eq!(a.forced_list().len(), 2);
        assert_eq!(a.forced_list()[0].tex_id, 9);
        assert_eq!(a.stencil_ref, [3, 4]);
    }

    #[test]
    fn force_stops_when_full() {
        let mut a = DrawAnswer::default();
        for i in 0..MAX_FORCED as u32 {
            assert!(a.force_texture(1, i, 1));
        }
        assert!(!a.force_texture(1, 99, 1));
    }

    #[test]
    fn default_answer_changes_nothing() {
        let mut a = PassAnswer::default();
        a.merge(&PassAnswer::default());
        assert_eq!(a, PassAnswer::default());
        let mut p = PassAnswer {
            flags: PASS_LOAD,
            load_slots: 1,
            load: LOAD_CLEAR,
            clear: [1.0, 0.0, 0.0, 1.0],
        };
        let q = PassAnswer {
            flags: PASS_SKIP,
            ..Default::default()
        };
        p.merge(&q);
        assert_eq!(p.flags, PASS_LOAD | PASS_SKIP);
        assert_eq!(p.load, LOAD_CLEAR);
    }
}
