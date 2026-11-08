//! The seam of the command buffer group, wired to the texture and sampler groups.
//!
//! [`install`](super::install) sets it once. The pool bases come from the descriptor pool objects in guest memory and
//! the GPU address of a texture from the guest `NVNtexture`, so none of the three answers takes a lock.

use super::abi::AbiResult;
use super::cmdbuf::{Seam, set_seam};
use super::{sampler, texture};

struct Wired;

impl Seam for Wired {
    fn texture_pool_base(&self, pool: u64) -> AbiResult<u32> {
        texture::pool_base(pool)
    }

    fn sampler_pool_base(&self, pool: u64) -> AbiResult<u32> {
        sampler::pool_base(pool)
    }

    fn texture_address(&self, tex: u64) -> AbiResult<u64> {
        let addr = texture::storage_address(tex)?;
        Ok(nxkit_nvn::decode::tag_address(
            addr,
            texture::texture_id(tex)?,
        ))
    }
}

static WIRED: Wired = Wired;

/// Sets the seam. A second call changes nothing.
pub(super) fn wire() {
    set_seam(&WIRED);
}
