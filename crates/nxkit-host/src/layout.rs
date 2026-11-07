//! Where everything sits in the one reservation of guest memory (R18, R3).
//!
//! The modules keep the addresses of `game.json`. The host places its own regions behind the highest module:
//! the stack of the main guest thread, the thread-local blocks of all guest threads, the heap, the stack region and the
//! alias region.
//! The result is one range, which is the guest range of `nxkit_abi::guest` and the no-copy buffer of R3.

use crate::error::{HostError, Result};
use crate::manifest::Game;

/// The page size of Apple silicon.
pub const PAGE: u64 = 0x4000;
/// The alignment of the heap and alias regions. Horizon wants 2 MiB.
pub const REGION_ALIGN: u64 = 0x20_0000;
/// The number of guest threads that have a thread-local block at the same time.
pub const TLS_SLOTS: u64 = 1024;
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
    /// The stack of the main guest thread. Its lowest page is a guard page.
    pub main_stack: Range,
    /// The thread-local blocks, `TLS_SLOTS` of `TLS_SLOT_SIZE`.
    pub tls: Range,
    /// The heap region, for `SetHeapSize`.
    pub heap: Range,
    /// The stack region of the process. `MapMemory` takes its destination from it.
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

/// Plans the reservation for a game.
pub fn plan(game: &Game) -> Result<Layout> {
    let lo = game.modules.iter().map(|m| m.base).min().ok_or_else(overflow)?;
    let hi = game.modules.iter().map(|m| m.end()).max().ok_or_else(overflow)?;
    let stack_len = align_up(game.main_thread.stack_size, PAGE)
        .and_then(|s| s.checked_add(PAGE))
        .ok_or_else(overflow)?;
    let stack_start = align_up(hi, PAGE).ok_or_else(overflow)?;
    let tls_start = stack_start.checked_add(stack_len).ok_or_else(overflow)?;
    let tls_len = TLS_SLOTS * TLS_SLOT_SIZE;
    let heap_start = align_up(tls_start + tls_len, REGION_ALIGN).ok_or_else(overflow)?;
    let heap_len = align_up(game.memory.heap, REGION_ALIGN).ok_or_else(overflow)?;
    let stack_region_start = heap_start.checked_add(heap_len).ok_or_else(overflow)?;
    let stack_region_len = align_up(game.memory.stack, REGION_ALIGN).ok_or_else(overflow)?;
    let alias_start = stack_region_start.checked_add(stack_region_len).ok_or_else(overflow)?;
    let alias_len = align_up(game.memory.alias, REGION_ALIGN).ok_or_else(overflow)?;
    let end = alias_start.checked_add(alias_len).ok_or_else(overflow)?;
    Ok(Layout {
        all: Range { start: lo, len: end - lo },
        modules: Range { start: lo, len: hi - lo },
        main_stack: Range { start: stack_start, len: stack_len },
        tls: Range { start: tls_start, len: tls_len },
        heap: Range { start: heap_start, len: heap_len },
        stack: Range { start: stack_region_start, len: stack_region_len },
        alias: Range { start: alias_start, len: alias_len },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn game(stack: u64) -> Option<Game> {
        let src = format!(
            r#"{{"title_id": "1", "services": [], "romfs": "r",
            "main_thread": {{"priority": 44, "core": 0, "stack_size": {stack}}},
            "memory": {{"heap": "0x40000000", "alias": "0x20000000"}},
            "modules": [{{"name": "main", "base": "0x100000000", "size": "0x20000",
              "text": {{"offset": 0, "size": 16384}}, "rodata": {{"offset": 16384, "size": 16384}},
              "data": {{"offset": 49152, "size": 16384}}, "bss_size": 16384, "image": "i", "imports": "a",
              "exports": "b", "entry": 0, "key_page": 114688, "gates": "g"}}]}}"#
        );
        Game::parse(Path::new("/x"), &src).ok()
    }

    #[test]
    fn regions_follow_the_modules_without_overlap() {
        let g = game(0x10_0000);
        assert!(g.is_some());
        let Some(g) = g else { return };
        let l = plan(&g);
        assert!(l.is_ok());
        let Ok(l) = l else { return };
        assert_eq!(l.all.start, 0x1_0000_0000);
        assert_eq!(l.main_stack.start, 0x1_0002_0000);
        assert_eq!(l.main_stack.len, 0x10_0000 + PAGE);
        assert_eq!(l.tls.start, l.main_stack.end());
        assert_eq!(l.heap.start % REGION_ALIGN, 0);
        assert!(l.heap.start >= l.tls.end());
        assert_eq!(l.stack.start, l.heap.end());
        assert_eq!(l.alias.start, l.stack.end());
        assert_eq!(l.all.end(), l.alias.end());
    }
}
