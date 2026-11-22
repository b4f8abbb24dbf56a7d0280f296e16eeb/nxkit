//! What the host wires for an Unreal main: the size of the arena of the engine heap and the handlers of the observation
//! gates `UE_REGION` and `UE_MALLOC` (R46). The host passes the facts of the run as plain values and registers the
//! handlers by their ids. It knows no Unreal name.
//!
//! The arena exists when the fixups of the main module name Unreal's FMallocBinned3 vtable (`UE_MALLOC_VT`), because
//! then the rewriter cut its constructor call and the object has no vtable of its own.

use std::sync::Arc;

use nxkit_abi::obs::{self, Handler};

use crate::settings::UeSettings;

/// The share of the alias region that the engine heap gets by default, in percent.
const HEAP_SHARE: u64 = 40;
/// The step of the arena's size.
const ARENA_STEP: u64 = 0x200_0000;
/// The fixups key of the FMallocBinned3 vtable.
const MALLOC_VT: &str = "UE_MALLOC_VT";
/// The fixups key of the slot of its name getter.
const MALLOC_SLOT: &str = "UE_MALLOC_SLOT";

/// The size of the arena that the kernel keeps for the engine heap at the end of the alias region of `alias` bytes: 0
/// unless the main module names Unreal's FMallocBinned3, else `NXKIT_UE_HEAP` or 40 % of the alias region, in 32 MiB
/// steps. `fixup` reads a value of the fixups of the main module.
pub fn arena_size(
    fixup: &dyn Fn(&str) -> Option<u64>,
    settings: &UeSettings,
    alias: u64,
) -> Result<u64, String> {
    if fixup(MALLOC_VT).is_none() {
        return Ok(0);
    }
    let size = match settings.heap.as_deref() {
        Some(v) => nxkit_abi::env::parse_size(v)
            .ok_or_else(|| format!("the size {v} of the Unreal heap is not a size"))?,
        None => alias / 100 * HEAP_SHARE,
    };
    let size = size & !(ARENA_STEP - 1);
    if size == 0 || size >= alias {
        return Err(format!(
            "the Unreal heap of {size:#x} bytes does not fit the alias region of {alias:#x} bytes"
        ));
    }
    eprintln!(
        "nxkit-ue: the Unreal heap gets {} MiB at the end of the alias region of {} MiB",
        size >> 20,
        alias >> 20
    );
    Ok(size)
}

/// The handlers of the observation gates of an Unreal main, by id. `fixup` reads a value of the fixups of the main
/// module at `main_base`, `alias_room` answers what the alias region has room for, `arena` is the arena the kernel
/// keeps (`None` when it keeps none) and `fatal` stops the game.
pub fn handlers(
    fixup: &dyn Fn(&str) -> Option<u64>,
    main_base: u64,
    alias_room: Arc<dyn Fn() -> u64 + Send + Sync>,
    arena: Option<(u64, u64)>,
    fatal: fn(&str) -> !,
) -> Result<Vec<(u8, Handler)>, String> {
    let mut out: Vec<(u8, Handler)> = vec![(
        obs::UE_REGION,
        Box::new(move |r, p| {
            crate::cap_region(r, p, alias_room());
        }),
    )];
    if let Some(vt) = fixup(MALLOC_VT) {
        let slot = fixup(MALLOC_SLOT).unwrap_or(0);
        let arena = arena.ok_or_else(|| {
            "the fixups name Unreal's FMallocBinned3 and the kernel keeps no arena for its heap"
                .to_string()
        })?;
        let installed = crate::install(main_base + vt, slot as u32, arena, fatal)
            .map_err(|e| format!("main: {e}: the allocator would have no vtable"))?;
        out.push((
            obs::UE_MALLOC,
            Box::new(move |r, p| installed.on_object(r, p)),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unreal(key: &str) -> Option<u64> {
        (key == MALLOC_VT).then_some(0x100)
    }

    fn other(_: &str) -> Option<u64> {
        None
    }

    #[test]
    fn the_arena_is_a_share_of_the_alias_region_for_an_unreal_main_only() {
        let alias = 0x1_0000_0000;
        let none = UeSettings::default();
        assert_eq!(arena_size(&other, &none, alias), Ok(0));
        assert_eq!(arena_size(&unreal, &none, alias), Ok(0x6600_0000));
        let set = UeSettings {
            heap: Some("512M".into()),
        };
        assert_eq!(arena_size(&unreal, &set, alias), Ok(512 << 20));
        let bad = UeSettings {
            heap: Some("lots".into()),
        };
        assert!(arena_size(&unreal, &bad, alias).is_err());
        let big = UeSettings {
            heap: Some("8G".into()),
        };
        assert!(arena_size(&unreal, &big, alias).is_err());
    }

    #[test]
    fn a_main_without_the_allocator_gets_the_region_gate_alone() {
        fn stop(m: &str) -> ! {
            let _ = m;
            std::process::abort()
        }
        let h = handlers(&other, 0, Arc::new(|| 0), None, stop).unwrap_or_default();
        assert_eq!(
            h.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [obs::UE_REGION]
        );
        assert!(
            handlers(&unreal, 0, Arc::new(|| 0), None, stop).is_err(),
            "no arena"
        );
    }
}
