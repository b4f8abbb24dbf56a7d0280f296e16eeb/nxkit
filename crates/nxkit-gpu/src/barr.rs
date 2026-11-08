//! The `barr` diagnostic: it compares the textures that the draws of a pass bind with the textures that the barrier
//! planner thinks the pass reads. A bound texture that the planner does not know gets no barrier against its writer.

use std::collections::HashMap;
use std::sync::Mutex;

use nxkit_nvn::resource::TexId;

static HANDLES: Mutex<Option<HashMap<u64, TexId>>> = Mutex::new(None);

/// Remembers which texture a handle resolved to.
pub(crate) fn note(handle: u64, id: TexId) {
    if let Ok(mut m) = HANDLES.lock() {
        m.get_or_insert_with(HashMap::new).insert(handle, id);
    }
}

/// The texture that a handle resolved to.
pub(crate) fn texture_of(handle: u64) -> Option<TexId> {
    HANDLES.lock().ok()?.as_ref()?.get(&handle).copied()
}
