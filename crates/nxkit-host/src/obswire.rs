//! The wiring of the observation gates: which subsystem answers which id (R46, rule 11).
//!
//! - The address region gates of the `sdk` (`REGION_ASK`, `REGION_GOT`, `REGION_FREE`) feed the kernel's table of
//!   the SDK's address regions (`nxkit-hos`).
//! - `UE_REGION` caps Unreal's engine region to what the kernel says the alias region has room for (`nxkit-ue`).
//! - `UE_MALLOC` gives Unreal's allocator object the vtable of the engine heap of `nxkit-ue`, over the arena the kernel
//!   keeps at the end of the alias region. The arena exists when the fixups of the main module name Unreal's
//!   FMallocBinned3 vtable (`UE_MALLOC_VT`), because then the rewriter cut its constructor call and the object has no
//!   vtable of its own.

use crate::error::{HostError, Result};
use crate::hostenv::HostEnv;
use crate::manifest::Game;
use crate::obs::ObsTable;
use nxkit_abi::obs;

/// The share of the alias region that the engine heap gets by default, in percent.
const UE_HEAP_SHARE: u64 = 40;
/// The step of the arena's size.
const ARENA_STEP: u64 = 0x200_0000;

/// A value of the fixups table of the main module (`# KEY\t0x...`).
fn main_fixup(game: &Game, key: &str) -> Option<u64> {
    let main = game.main_module().ok()?;
    let text = std::fs::read_to_string(game.path(&main.fixups)).ok()?;
    let prefix = format!("# {key}\t");
    let v = text.lines().find_map(|l| l.strip_prefix(prefix.as_str()))?;
    u64::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()
}

/// The size of the arena the kernel keeps for the engine heap at the end of the alias region of `alias` bytes: 0
/// unless the main module names Unreal's FMallocBinned3, else `NXKIT_UE_HEAP` or 40 % of the alias region, in 32 MiB
/// steps.
pub fn arena_size(game: &Game, env: &HostEnv, alias: u64) -> Result<u64> {
    if main_fixup(game, "UE_MALLOC_VT").is_none() {
        return Ok(0);
    }
    let size = match env.ue_heap.as_deref() {
        Some(v) => crate::manifest::parse_size(v).ok_or_else(|| {
            HostError::Manifest(format!("the size {v} of the Unreal heap is not a size"))
        })?,
        None => alias / 100 * UE_HEAP_SHARE,
    };
    let size = size & !(ARENA_STEP - 1);
    if size == 0 || size >= alias {
        return Err(HostError::Manifest(format!(
            "the Unreal heap of {size:#x} bytes does not fit the alias region of {alias:#x} bytes"
        )));
    }
    eprintln!(
        "nxkit-host: the Unreal heap gets {} MiB at the end of the alias region of {} MiB",
        size >> 20,
        alias >> 20
    );
    Ok(size)
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
    t.set(
        obs::UE_REGION,
        Box::new(move |r, p| {
            nxkit_ue::cap_region(r, p, k.alias_room());
        }),
    );
    if let Some(vt) = main_fixup(game, "UE_MALLOC_VT") {
        let slot = main_fixup(game, "UE_MALLOC_SLOT").unwrap_or(0);
        let base = game.main_module()?.base;
        let arena = kernel.arena().ok_or_else(|| {
            HostError::Load("the fixups name Unreal's FMallocBinned3 and the kernel keeps no arena for its heap".into())
        })?;
        let installed = nxkit_ue::install(base + vt, slot as u32, arena, crate::stop::fatal)
            .map_err(|e| {
                HostError::Load(format!("main: {e}: the allocator would have no vtable"))
            })?;
        t.set(
            obs::UE_MALLOC,
            Box::new(move |r, p| installed.on_object(r, p)),
        );
    }
    Ok(t)
}
