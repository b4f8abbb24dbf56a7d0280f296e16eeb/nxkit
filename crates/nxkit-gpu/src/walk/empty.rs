//! The detail of a draw that cannot run.

use std::sync::Arc;

use crate::detail::{DrawArgs, DrawDetail, ProgramRef};

/// The empty shader key of a draw that cannot run, shared so that such a draw allocates nothing.
fn empty_key() -> Arc<str> {
    static EMPTY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| Arc::from("")).clone()
}

/// The detail of a draw that cannot run. It keeps the ordinals of the draws after it.
pub(crate) fn empty_draw() -> DrawDetail {
    DrawDetail {
        push: None,
        program: ProgramRef {
            vertex: empty_key(),
            fragment: None,
            task: None,
            mesh: None,
            geometry: None,
        },
        blend: Default::default(),
        write_masks: [0xf; 8],
        depth_stencil: Default::default(),
        raster: Default::default(),
        topology: 4,
        attributes: Vec::new(),
        streams: Vec::new(),
        args: DrawArgs::Arrays {
            first: 0,
            count: 0,
            instances: 0,
            base_instance: 0,
        },
        ubos: Vec::new(),
        sbufs: Vec::new(),
        textures: Vec::new(),
        samplers: Vec::new(),
        images: Vec::new(),
        dynamic: Default::default(),
        feedback_input: false,
    }
}
