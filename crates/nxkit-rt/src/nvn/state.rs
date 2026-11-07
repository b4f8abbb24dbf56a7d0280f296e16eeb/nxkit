//! The state group: the blend, depth, polygon, multisample, channel mask, color and vertex state objects.
//!
//! This group exports nothing yet. A state object is plain data in guest memory, as in the other groups
//! ([`super::abi::Obj`]), and the command buffer group reads it when a game binds the state.

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    Vec::new()
}
