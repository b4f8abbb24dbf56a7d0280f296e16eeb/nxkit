//! The extension records: what each of them changes in the state of the walker, and the detail of draws and clears.

use std::sync::Arc;

use nxkit_nvn::ext::{DrawKind, Ext, ExtCode, Setting, StateKind};
use nxkit_nvn::mem::GuestMem;

use super::cache::{self, sc};
use super::objects::{self, Blends, StencilValues, write_masks};
use super::programs::Kind;
use super::{
    ATTRIBUTES, MAX_COLOR, MESH, MESH_TEXTURES, STREAMS, State, TASK, UBOS, Walker, bound_count,
    empty_draw, images_into, sbufs_into, texture_count, textures_into,
};
use crate::detail::{
    AttributeDetail, ClearDetail, CopyDetail, CopyKind, DrawArgs, DrawDetail, DynamicDetail,
    ProgramRef, Stage, StreamDetail, UboDetail,
};

/// The `NVNshaderStage` values that have uniform buffers here, as the slot in `State::ubo`. The recorder writes the
/// task and mesh stages as 3 and 4, the stages of tessellation, which the encoder does not run.
fn ubo_stage(stage: u16) -> Option<usize> {
    match stage {
        0 => Some(0),
        1 => Some(1),
        5 => Some(2),
        3 => Some(TASK),
        4 => Some(MESH),
        _ => None,
    }
}

/// The `NVNshaderStageBits` of the NVN2 task and mesh stages and the program slots they go to.
const MESH_STAGE_BITS: [(u32, usize); 2] = [(6, TASK), (7, MESH)];

fn f(x: &Ext, i: usize) -> f32 {
    f32::from_bits(x.word(i))
}

/// The `nvnCommandBufferClearColor` value of a clear. Integer clears give their value as a number.
fn clear_value(kind: u16, bits: u32) -> f32 {
    match kind {
        1 => bits as i32 as f32,
        2 => bits as f32,
        _ => f32::from_bits(bits),
    }
}

/// Bytes of a push constant block (the translator's `PUSH_SIZE`).
const PUSH_BYTES: usize = 0x100;

mod copy;
mod draw;
mod records;
#[cfg(test)]
mod tests;
mod uniform;
