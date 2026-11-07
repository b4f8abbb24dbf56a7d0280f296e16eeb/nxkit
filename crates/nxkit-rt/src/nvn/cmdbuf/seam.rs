//! What the command buffer group needs from the texture, sampler and state groups.
//!
//! The groups are filled by different parts of the host. The command buffer group reads three things from them: the id
//! base of a descriptor pool, the GPU address of a texture and, for the draw state that the pass analysis follows,
//! three bits of the guest state objects. This module names them. The integrator sets one [`Seam`] with
//! [`set_seam`], and the texture group gives the pool bases. Until then a call that needs a pool base ends the game
//! with the name of the missing piece (R15).

use std::sync::OnceLock;

use crate::nvn::abi::{AbiResult, other};
use crate::nvn::ctx::gpu;

/// The functions of the other groups.
pub(crate) trait Seam: Sync {
    /// The id base of the texture pool at the guest address `pool`: the number of the first descriptor of the pool in
    /// the descriptor table.
    fn texture_pool_base(&self, pool: u64) -> AbiResult<u32>;

    /// The id base of the sampler pool at the guest address `pool`.
    fn sampler_pool_base(&self, pool: u64) -> AbiResult<u32>;

    /// The GPU address of the texture at the guest address `tex`. This default takes the lock of the front end. A
    /// texture group that keeps the GPU address in the guest `NVNtexture` answers without a lock.
    fn texture_address(&self, tex: u64) -> AbiResult<u64> {
        let g = gpu("nvnCommandBuffer");
        let id = g
            .textures
            .get(tex)
            .ok_or_else(|| other(format!("no texture is initialised at {tex:#x}")))?;
        Ok(g.lock().texture_address(id)?)
    }
}

struct Unwired;

impl Seam for Unwired {
    fn texture_pool_base(&self, _pool: u64) -> AbiResult<u32> {
        Err(other("the texture group gives no pool base yet"))
    }

    fn sampler_pool_base(&self, _pool: u64) -> AbiResult<u32> {
        Err(other("the sampler group gives no pool base yet"))
    }
}

static SEAM: OnceLock<&'static dyn Seam> = OnceLock::new();

/// Sets the seam. False when one is set already.
#[allow(dead_code, reason = "the integrator of the texture and sampler groups calls it")]
pub(crate) fn set_seam(seam: &'static dyn Seam) -> bool {
    SEAM.set(seam).is_ok()
}

/// The seam. A load, no lock.
pub(super) fn seam() -> &'static dyn Seam {
    SEAM.get().copied().unwrap_or(&Unwired)
}

/// Checks that `len` bytes at the host address `host` are guest memory that the front end knows. Takes the lock of the
/// front end, so only the calls that add memory use it.
pub(super) fn validate_region(host: u64, len: u64) -> AbiResult<()> {
    gpu("nvnCommandBufferAddCommandMemory")
        .gpu_address(host, len)
        .map(|_| ())
}

/// The draw state bit of a colour state (`NVNcolorState`): 1 when blending is on for any target.
pub(super) fn color_patch(words: &[u32]) -> u8 {
    u8::from(words.first().is_some_and(|w| w & 0xff != 0))
}

/// The colour write mask of a channel mask state (`NVNchannelMaskState`): bit n is set when target n writes a
/// channel. The state holds four bits (r, g, b, a) per target.
pub(super) fn channel_mask_patch(words: &[u32]) -> u8 {
    let w = words.first().copied().unwrap_or(0);
    (0..8).fold(0u8, |m, i| m | (u8::from((w >> (4 * i)) & 0xf != 0) << i))
}

/// The draw state bits of a depth stencil state (`NVNdepthStencilState`): bit 0 is the depth test, bit 1 the depth
/// write and bit 2 the stencil test.
pub(super) fn depth_stencil_patch(words: &[u32]) -> u8 {
    let w = words.first().copied().unwrap_or(0);
    ((w & 1) | (((w >> 1) & 1) << 1) | (((w >> 6) & 1) << 2)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_object_gives_its_draw_state_bits() {
        assert_eq!(color_patch(&[0, 0]), 0);
        assert_eq!(color_patch(&[1]), 1);
        assert_eq!(channel_mask_patch(&[0x0000_0f0f]), 0b0000_0101);
        assert_eq!(depth_stencil_patch(&[0b100_0011, 0]), 0b111);
        assert_eq!(depth_stencil_patch(&[0b10, 0]), 0b010);
    }
}
