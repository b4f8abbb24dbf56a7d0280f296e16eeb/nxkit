//! The texture pool: `nvnTexturePool*`.
//!
//! A registration builds the TIC of the texture, writes its 32 bytes into the pool memory of the game and tells the
//! front end. The id of the texture and its swizzle come from the guest object, so a registration looks nothing up by
//! address. The pool objects are in [`super::pool`].
//!
//! `nvnTexturePoolRegisterTexture` and `nvnTexturePoolRegisterImage` have the same arguments and the front end has one
//! kind of header for both. The front end cannot drop a registered slot, so a slot keeps its texture until the game
//! registers another one in it.

use std::sync::LazyLock;

use nxkit_abi::guest;

use crate::nvn::abi::{AbiResult, guard, log_once, magic, table};
use crate::nvn::ctx::gpu;

use super::pool::{self, DescPools};
use super::{Tex, view};

/// The magic of an initialised texture pool.
const TPL: u32 = magic(b'T', b'P', b'L', 0);

static POOLS: LazyLock<DescPools> = LazyLock::new(|| DescPools::new(TPL));

/// The first descriptor id of the texture pool at `pool`, for the command buffer.
pub(crate) fn pool_base(pool: u64) -> AbiResult<u32> {
    POOLS.base(pool)
}

extern "C" fn initialize(tp: u64, memory_pool: u64, off: i64, n: i32) -> u32 {
    guard("nvnTexturePoolInitialize", || {
        POOLS.initialize(
            "nvnTexturePoolInitialize",
            tp,
            (memory_pool, off),
            n,
            |base, count, host| {
                gpu("nvnTexturePoolInitialize").with_front(|f| f.add_texture_pool(base, count, host))
            },
        )
    })
}

extern "C" fn finalize(tp: u64) {
    guard("nvnTexturePoolFinalize", || POOLS.finalize(tp).map(|_| ()));
}

/// Registers the texture at `t` in the slot `id` of the pool at `tp`, seen through the view at `v`.
fn register(name: &'static str, tp: u64, id: i32, t: u64, v: u64) -> AbiResult<()> {
    let tex = Tex::load(t)?;
    let slot = POOLS.slot(tp, id)?;
    if tex.stencil || view::View::load(v)?.stencil() {
        log_once(
            "texture_stencil",
            format_args!(
                "{name}: a stencil view of a depth-stencil texture is registered as its depth view"
            ),
        );
    }
    let tv = view::texture_view(v, tex.swizzle)?;
    let g = gpu(name);
    let (registered, dropped) = g.with_front(|f| f.register_texture(slot.id, tex.id, tv).map(|r| (r, f.take_dropped())))?;
    for id in dropped {
        g.backend().texture_finalized(id);
    }
    guest::write(slot.header, &registered.header.to_bytes()?)?;
    Ok(())
}

extern "C" fn register_texture(tp: u64, id: i32, t: u64, v: u64) {
    guard("nvnTexturePoolRegisterTexture", || {
        register("nvnTexturePoolRegisterTexture", tp, id, t, v)
    });
}

extern "C" fn register_image(tp: u64, id: i32, t: u64, v: u64) {
    guard("nvnTexturePoolRegisterImage", || {
        register("nvnTexturePoolRegisterImage", tp, id, t, v)
    });
}

extern "C" fn get_memory_pool(tp: u64) -> u64 {
    guard("nvnTexturePoolGetMemoryPool", || pool::memory_pool_of(tp))
}

extern "C" fn get_memory_offset(tp: u64) -> i64 {
    guard("nvnTexturePoolGetMemoryOffset", || pool::memory_offset_of(tp))
}

extern "C" fn get_size(tp: u64) -> i32 {
    guard("nvnTexturePoolGetSize", || pool::size_of(tp))
}

extern "C" fn set_debug_label(_tp: u64, _label: u64) {}

/// The functions of the texture pool.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnTexturePoolInitialize" => initialize as extern "C" fn(u64, u64, i64, i32) -> u32,
        "nvnTexturePoolFinalize" => finalize as extern "C" fn(u64),
        "nvnTexturePoolRegisterTexture" => register_texture as extern "C" fn(u64, i32, u64, u64),
        "nvnTexturePoolRegisterImage" => register_image as extern "C" fn(u64, i32, u64, u64),
        "nvnTexturePoolGetMemoryPool" => get_memory_pool as extern "C" fn(u64) -> u64,
        "nvnTexturePoolGetMemoryOffset" => get_memory_offset as extern "C" fn(u64) -> i64,
        "nvnTexturePoolGetSize" => get_size as extern "C" fn(u64) -> i32,
        "nvnTexturePoolSetDebugLabel" => set_debug_label as extern "C" fn(u64, u64),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_exports_its_eight_functions() {
        assert_eq!(procs().len(), 8);
    }
}
