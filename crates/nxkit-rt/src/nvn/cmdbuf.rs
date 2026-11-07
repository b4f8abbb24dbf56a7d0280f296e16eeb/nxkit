//! The command buffer group: `nvnCommandBuffer*`.
//!
//! The group answers 145 functions. The recording functions write their records into the command memory of the game
//! with the write position that is kept in the guest `NVNcommandBuffer`, so a call takes no lock, makes no registry
//! lookup and allocates nothing (R54). The records are the ones of [`nxkit_nvn::record`] and [`nxkit_nvn::ext`]
//! (R53).
//!
//! The group is split by what the functions do:
//!
//! - `rec`: the writer, the slow path and the memory callback.
//! - `life`: initialise, memory, recording, call and copy, the getters and the functions that do nothing.
//! - `bind`: program, buffers, textures, images, samplers, vertex input, state objects and the descriptor pools.
//! - `draw`: draws, dispatches and transform feedback.
//! - `xfer`: render targets, clears, copies, downsample, discards, barrier, sync, counters, push constants and inline
//!   uniform data.
//! - `set`: viewport, scissor, depth range, stencil values and the scalar settings.
//! - `seam`: what the group needs from the texture, sampler and state groups.
//!
//! The memory callback of a command buffer runs with no lock held. It may call `nvnCommandBufferAddCommandMemory`.

mod args;
mod bind;
mod draw;
mod life;
mod rec;
mod seam;
mod set;
mod xfer;

use std::sync::OnceLock;

pub(crate) use seam::{Seam, set_seam};

/// What a title needs the command buffer group to do differently. The host fills it from the needs of the title
/// (`game.json`). The default is the strict behaviour of the SDK.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandSettings {
    /// A `BindVertexBuffers` of one null range past the last vertex stream does nothing instead of ending the game.
    /// This is the need `vertex_stream_null_bind`.
    pub ignore_null_stream_binds: bool,
}

static SETTINGS: OnceLock<CommandSettings> = OnceLock::new();

/// Sets the settings of the group. False when they are set already. Call it before the game runs.
pub fn set_command_settings(s: CommandSettings) -> bool {
    SETTINGS.set(s).is_ok()
}

/// The settings. A load, no lock.
fn settings() -> CommandSettings {
    SETTINGS.get().copied().unwrap_or_default()
}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    let mut all = life::procs();
    all.extend(bind::procs());
    all.extend(draw::procs());
    all.extend(xfer::procs());
    all.extend(set::procs());
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_group_answers_every_name_of_its_row_once() {
        let mut have: Vec<_> = procs().into_iter().map(|(n, _)| n).collect();
        have.sort_unstable();
        let mut want: Vec<_> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "cmdbuf")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        want.sort_unstable();
        assert_eq!(have, want);
    }

    #[test]
    fn the_settings_are_strict_by_default() {
        assert!(!CommandSettings::default().ignore_null_stream_binds);
    }
}
