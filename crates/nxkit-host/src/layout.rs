//! Where everything sits in the one reservation of guest memory (R18, R3).
//!
//! The modules keep the addresses of `game.json`. The host places its own regions behind the highest module:
//! the thread-local blocks of all guest threads, the area behind shared memory, the heap, the stack region (which
//! starts with the stack of the main guest thread) and the alias region.
//! The result is one range, which is the guest range of `nxkit_abi::guest` and the no-copy buffer of R3.

use crate::error::{HostError, Result};
use crate::manifest::Game;

/// The page size of Apple silicon.
pub const PAGE: u64 = 0x4000;
/// The alignment of the heap and alias regions. Horizon wants 2 MiB.
pub const REGION_ALIGN: u64 = 0x20_0000;
/// The end of the 39-bit address space that the kernel models. The whole reservation lies below it.
pub const SPACE_END: u64 = 1 << 39;
/// The address that the system gives below `SPACE_END` in a macOS process (measured 2026-11-08).
pub const FALLBACK_BASE: u64 = 0x70_0000_0000;
/// The number of guest threads that have a thread-local block at the same time.
pub const TLS_SLOTS: u64 = 1024;
/// The size of the area that backs the shared memory objects of the guest.
pub const SHARED_LEN: u64 = 0x400_0000;
/// The size of one thread-local block: the thread-local region, the thread object and the ELF TLS area.
pub const TLS_SLOT_SIZE: u64 = 0x4000;

/// A range of guest addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// The first address.
    pub start: u64,
    /// The length in bytes.
    pub len: u64,
}

impl Layout {
    /// True when the whole reservation ends at or below the end of the 39-bit address space.
    pub fn fits_address_space(&self) -> bool {
        self.all.end() <= SPACE_END
    }
}

impl Range {
    /// The first address after the range.
    pub fn end(&self) -> u64 {
        self.start + self.len
    }
}

/// The map of the reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The whole reservation.
    pub all: Range,
    /// The span of the modules, from the lowest base to the end of the highest module.
    pub modules: Range,
    /// The stack of the main guest thread, at the start of the stack region. Its lowest page is a guard page.
    pub main_stack: Range,
    /// The thread-local blocks, `TLS_SLOTS` of `TLS_SLOT_SIZE`.
    pub tls: Range,
    /// The backing of the shared memory objects.
    pub shared: Range,
    /// The heap region, for `SetHeapSize`.
    pub heap: Range,
    /// The stack region of the process. It holds the main stack, and `MapMemory` takes its destination from it.
    pub stack: Range,
    /// The alias region, for `MapMemory`.
    pub alias: Range,
}

fn align_up(v: u64, a: u64) -> Option<u64> {
    v.checked_add(a - 1).map(|x| x & !(a - 1))
}

fn overflow() -> HostError {
    HostError::Memory("the guest address map overflows 64 bits".into())
}

/// Plans the reservation for a game whose modules sit where `game` says.
///
/// The host regions start at `floor` or behind the highest module, whichever is higher. When dyld mapped the
/// library, `floor` is the end of everything that dyld mapped, which is a bit behind the last module.
pub fn plan(game: &Game, floor: u64) -> Result<Layout> {
    let lo = game.modules.iter().map(|m| m.start()).min().ok_or_else(overflow)?;
    let hi = game.modules.iter().map(|m| m.end()).max().ok_or_else(overflow)?;
    let stack_len = align_up(game.main_thread.stack_size, PAGE)
        .and_then(|s| s.checked_add(PAGE))
        .ok_or_else(overflow)?;
    let tls_start = align_up(hi.max(floor), PAGE).ok_or_else(overflow)?;
    let tls_len = TLS_SLOTS * TLS_SLOT_SIZE;
    let shared_start = tls_start.checked_add(tls_len).ok_or_else(overflow)?;
    let heap_start = align_up(shared_start.checked_add(SHARED_LEN).ok_or_else(overflow)?, REGION_ALIGN)
        .ok_or_else(overflow)?;
    let heap_len = align_up(game.memory.heap, REGION_ALIGN).ok_or_else(overflow)?;
    let stack_region_start = heap_start.checked_add(heap_len).ok_or_else(overflow)?;
    let stack_region_len = game
        .memory
        .stack
        .checked_add(stack_len)
        .and_then(|v| align_up(v, REGION_ALIGN))
        .ok_or_else(overflow)?;
    let alias_start = stack_region_start.checked_add(stack_region_len).ok_or_else(overflow)?;
    let alias_len = align_up(game.memory.alias, REGION_ALIGN).ok_or_else(overflow)?;
    let end = alias_start.checked_add(alias_len).ok_or_else(overflow)?;
    Ok(Layout {
        all: Range { start: lo, len: end - lo },
        modules: Range { start: lo, len: hi - lo },
        main_stack: Range { start: stack_region_start, len: stack_len },
        tls: Range { start: tls_start, len: tls_len },
        shared: Range { start: shared_start, len: SHARED_LEN },
        heap: Range { start: heap_start, len: heap_len },
        stack: Range { start: stack_region_start, len: stack_region_len },
        alias: Range { start: alias_start, len: alias_len },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::sample;
    use crate::module::samples::{FAR, NEAR};
    use std::path::Path;

    fn game(module: &str) -> Option<Game> {
        Game::parse(Path::new("/x"), &sample("", module)).ok()
    }

    #[test]
    fn regions_follow_the_modules_without_overlap() {
        let g = game(NEAR);
        assert!(g.is_some());
        let Some(g) = g else { return };
        let l = plan(&g, 0);
        assert!(l.is_ok());
        let Ok(l) = l else { return };
        assert_eq!(l.all.start, 0x1_0000_0000);
        assert_eq!(l.tls.start, 0x1_0002_0000);
        assert_eq!(l.shared.start, l.tls.end());
        assert_eq!(l.main_stack.len, 0x10_0000 + PAGE);
        assert_eq!(l.heap.start % REGION_ALIGN, 0);
        assert!(l.heap.start >= l.shared.end());
        assert_eq!(l.stack.start, l.heap.end());
        assert_eq!(l.main_stack.start, l.stack.start);
        assert!(l.main_stack.end() <= l.stack.end());
        assert_eq!(l.alias.start, l.stack.end());
        assert_eq!(l.all.end(), l.alias.end());
    }

    #[test]
    fn a_far_module_starts_at_its_patch_area() {
        let g = game(FAR);
        assert!(g.is_some());
        let Some(g) = g else { return };
        let l = plan(&g, 0);
        assert!(l.is_ok());
        let Ok(l) = l else { return };
        assert_eq!(l.all.start, 0x100_0004_0000 - 0x2_0000);
    }

    #[test]
    fn the_floor_moves_the_host_regions_up() {
        let g = game(NEAR);
        assert!(g.is_some());
        let Some(g) = g else { return };
        let l = plan(&g, 0x1_0010_0000);
        assert!(l.is_ok());
        let Ok(l) = l else { return };
        assert_eq!(l.tls.start, 0x1_0010_0000);
    }
}
