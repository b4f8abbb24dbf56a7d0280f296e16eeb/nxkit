//! The sampler group: `nvnSampler*` and the sampler builder.
//!
//! This group exports nothing yet. It registers samplers in the descriptor pools of the front end
//! ([`super::ctx::Gpu::with_front`]) and tells the backend through `Backend::sampler_registered`.

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    Vec::new()
}
