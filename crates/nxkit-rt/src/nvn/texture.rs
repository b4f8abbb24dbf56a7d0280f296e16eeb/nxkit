//! The texture group: `nvnTexture*`, the texture builder, the texture and sampler pools and the texture views.
//!
//! This group exports nothing yet. It uses [`super::ctx::Gpu`] (`textures`, `with_front`) and [`super::abi`] for the guest
//! objects. The sizes of the objects are in [`super::abi::size`].

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    Vec::new()
}
