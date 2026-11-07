//! The command buffer group: `nvnCommandBuffer*`.
//!
//! This group exports nothing yet. It records through the front end ([`super::ctx::Gpu::with_front`]) with the number
//! of [`super::ctx::Gpu::command_buffer_index`]. The memory callback of a command buffer runs with the lock of the
//! front end held, so it must not call back into [`super::ctx::Gpu`].

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    Vec::new()
}
