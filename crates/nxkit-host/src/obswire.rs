//! The wiring of the observation gates: which subsystem answers which id (R46, rule 11).
//!
//! - The address region gates of the `sdk` (`REGION_ASK`, `REGION_GOT`, `REGION_FREE`) feed the kernel's table of
//!   the SDK's address regions (`nxkit-hos`).
//! - `nxkit-ue` answers the gates of an engine heap and an engine region. The host gives it the fixups of the main
//!   module, the kernel's answers and its settings, and registers the handlers it returns by their ids.

use std::sync::Arc;

use crate::error::{HostError, Result};
use crate::manifest::Game;
use crate::obs::ObsTable;
use nxkit_abi::obs;

/// The size of the arena that the kernel keeps at the end of the alias region of `alias` bytes for an engine heap (0
/// for none).
pub fn arena_size(game: &Game, ue: &nxkit_ue::settings::UeSettings, alias: u64) -> Result<u64> {
    nxkit_ue::wire::arena_size(&|k| game.main_fixup(k), ue, alias).map_err(HostError::Manifest)
}

/// The handlers of the observation gates of `game`. The kernel is started and the modules are relocated.
pub fn table(game: &Game) -> Result<ObsTable> {
    let mut t = ObsTable::default();
    let Some(kernel) = nxkit_rt::kernel() else {
        return Ok(t);
    };
    let k = kernel.clone();
    t.set(obs::REGION_ASK, Box::new(move |r, _| k.region_ask(r[2])));
    let k = kernel.clone();
    t.set(
        obs::REGION_GOT,
        Box::new(move |r, p| {
            if let Some(&a) = r.get(p as usize) {
                k.region_got(a);
            }
        }),
    );
    let k = kernel.clone();
    t.set(obs::REGION_FREE, Box::new(move |r, _| k.region_free(r[1])));
    let k = kernel.clone();
    let handlers = nxkit_ue::wire::handlers(
        &|key| game.main_fixup(key),
        game.main_module()?.base,
        Arc::new(move || k.alias_room()),
        kernel.arena(),
        crate::stop::fatal,
    )
    .map_err(HostError::Load)?;
    for (id, h) in handlers {
        t.set(id, h);
    }
    Ok(t)
}
