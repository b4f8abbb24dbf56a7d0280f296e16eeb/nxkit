//! The template cache of the walker.
//!
//! A game replays the recordings of its models in every frame. A walk reads each record again, follows the binds and
//! builds the same [`DrawDetail`] values. The cache keeps what the walk of a recording produced, and a later call of the
//! same handle plays that back.
//!
//! A [`Template`] is valid for the state that the recording inherits and reads. The walk tracks, for each recording, which
//! fields of the [`State`] it reads before it sets them (`needs`). The `key` holds their values at the entry. A call
//! matches a template when its state has the same values in those fields. A handle keeps up to [`MAX_PER_HANDLE`]
//! templates for different states.
//!
//! What a template holds, and how a hit keeps it exact:
//!
//! - The draws, clears, copies, dispatches and reports that the walk pushed, in the order of the walk, so the ordinals
//!   agree with the decoder. A draw is shared with the frame detail through an `Arc`.
//! - The state that the recording leaves behind (`delta`): the fields it set, with the values they have at its end.
//! - The sources of `BindTexturesDeferred`. A hit reads their values again. A draw whose texture came from a source
//!   that changed gets the new handle (`patches`), and the template keeps the new values.
//! - The inline `UpdateUniform` records. A hit writes their bytes into guest memory again, in order, as the walk does.
//! - The snapshots of uniform buffers. A snapshot depends on the memory that the game may rewrite without a record, so a
//!   hit looks at the current bytes and rebuilds a draw only when they differ.
//! - The generations of the programs that it looked up. `program_set` and `program_released` make it stale.
//!
//! A template is dropped when the game writes the memory of one of its records (`Walker::invalidate`). The spans of a
//! template include those of the recordings it calls, so a write to a callee drops its callers too.
//!
//! The recordings that [`Walker::submit`] is given are not cached. The game writes them anew in every frame, and the
//! recordings they call are the ones that repeat.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};

use nxkit_nvn::ext::Ext;
use nxkit_nvn::mem::GuestMem;
use nxkit_nvn::record::deferred_handle;

use super::objects::{Raster, StencilValues};
use super::{MAX_CALL_DEPTH, STREAMS, State, TEXTURE_SLOTS, Walker};
use crate::detail::{
    ClearDetail, CopyDetail, CounterReport, DispatchDetail, DrawDetail, Stage, TextureBind,
};
use crate::draws::FastMap;

mod bits;
mod build;
mod key;
mod play;
mod run;
mod store;
#[cfg(test)]
mod tests;
mod track;

use key::*;
use store::*;
use track::*;

pub(super) use bits::*;
pub(super) use store::Cache;
pub(crate) use store::Invalidations;
pub(super) use track::Track;

/// The most templates that one handle keeps. The least recently used goes first.
///
/// A model recording runs once per instance and pass, with another program, uniform buffer or vertex buffer each
/// time; BotW calls one recording with more than four states in a frame. The key digest keeps the search cheap.
pub(super) const MAX_PER_HANDLE: usize = 32;
/// The most handles with templates. The cache starts again when a new handle would pass it.
const MAX_HANDLES: usize = 1 << 16;
/// The value of `Track::tex_dep` for a slot that no deferred bind set.
const NO_DEP: u32 = u32::MAX;

/// Whether the unit tests run. A mismatch of the verification fails them, and only logs in the host.
fn under_test() -> bool {
    cfg!(test)
}

/// The bytes of a uniform buffer range, or none when no update touched it.
type Snapshot = Option<Arc<[u8]>>;

/// The uniform buffer ranges `(address, size)` of a template with their snapshots.
type RangeSnapshots = Vec<((u64, u32), Snapshot)>;

/// Whether two snapshots of a range are the same bytes.
fn same(a: &Snapshot, b: &Snapshot) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
        _ => false,
    }
}
