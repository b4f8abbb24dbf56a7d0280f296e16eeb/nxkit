//! The event group: `nvnEvent*` and the event builder.
//!
//! An event is a 32-bit counter in a memory pool that both the CPU and the GPU can write. `nvnEventSignal` writes the
//! word in guest memory at once. A GPU signal (`nvnCommandBufferSignalEvent`) reaches the word when the GPU finished
//! the work before it, as on the console (the backend writes it at the completion of the frame, `nxkit-gpu`
//! `backend::events`), so `nvnEventGetValue` reads guest memory and a game that polls the counter sees the GPU's value
//! once the results are ready. The queue gate keeps the counters that the GPU signals leave for the order of the
//! queues (`ctx::gate`).
//!
//! The `NVNevent` is 0x40 bytes in guest memory. The host uses the first 0x20 bytes as plain data and writes no more:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | magic `EVNT` while initialised |
//! | 8 | the host address of the counter (pool storage + offset) |
//! | 16 | the guest address of the `NVNmemoryPool` |
//! | 24 | the offset in the pool (i64) |
//!
//! The event builder is 0x20 bytes in the headers. The host writes the first 0x10 bytes: pool at 0, offset at 8.

use super::abi::{AbiResult, Obj, guard, magic, table, used};
use super::pool;

/// The magic of an initialised event.
const EVNT: u32 = magic(b'E', b'V', b'N', b'T');
/// `NVN_EVENT_SIGNAL_MODE_ADD`. Any other mode writes.
const MODE_ADD: i32 = 1;

const OFF_VAL: u64 = 8;
const OFF_POOL: u64 = 16;
const OFF_OFF: u64 = 24;
const B_POOL: u64 = 0;
const B_OFF: u64 = 8;

extern "C" fn builder_set_defaults(b: u64) {
    guard("nvnEventBuilderSetDefaults", || {
        Obj(b).zero(used::EVENT_BUILDER)
    });
}

extern "C" fn builder_set_storage(b: u64, pool: u64, off: i64) {
    guard("nvnEventBuilderSetStorage", || {
        Obj(b).set_u64(B_POOL, pool)?;
        Obj(b).set_i64(B_OFF, off)
    });
}

extern "C" fn builder_get_memory_pool(b: u64) -> u64 {
    guard("nvnEventBuilderGetMemoryPool", || Obj(b).u64(B_POOL))
}

extern "C" fn builder_get_memory_offset(b: u64) -> i64 {
    guard("nvnEventBuilderGetMemoryOffset", || Obj(b).i64(B_OFF))
}

extern "C" fn event_initialize(e: u64, b: u64) -> u32 {
    guard("nvnEventInitialize", || {
        let (pool_addr, off) = (Obj(b).u64(B_POOL)?, Obj(b).i64(B_OFF)?);
        let mem = if pool_addr == 0 {
            0
        } else {
            pool::view(pool_addr)?.mem
        };
        if mem == 0 {
            eprintln!("nxkit-rt: warning: NVN nvnEventInitialize: the event has no storage");
            return Ok(0);
        }
        let o = Obj(e);
        o.zero(used::EVENT)?;
        o.set_u32(0, EVNT)?;
        o.set_u64(OFF_VAL, mem.wrapping_add(off as u64))?;
        o.set_u64(OFF_POOL, pool_addr)?;
        o.set_i64(OFF_OFF, off)?;
        Ok(1)
    })
}

extern "C" fn event_finalize(e: u64) {
    guard("nvnEventFinalize", || Obj(e).set_u32(0, 0));
}

/// The new value of the counter after a signal.
fn signalled(mode: i32, old: u32, value: u32) -> u32 {
    if mode == MODE_ADD {
        old.wrapping_add(value)
    } else {
        value
    }
}

fn signal(e: u64, mode: i32, value: u32) -> AbiResult<()> {
    let addr = Obj(e).u64(OFF_VAL)?;
    let word = Obj(addr);
    let old = word.u32(0)?;
    word.set_u32(0, signalled(mode, old, value))?;
    // A queue that waits for this counter may run now.
    match super::ctx::installed() {
        Some(g) => g.event_signalled(addr),
        None => Ok(()),
    }
}

extern "C" fn event_signal(e: u64, mode: i32, value: u32) {
    guard("nvnEventSignal", || signal(e, mode, value));
}

extern "C" fn event_get_value(e: u64) -> u32 {
    guard("nvnEventGetValue", || Obj(Obj(e).u64(OFF_VAL)?).u32(0))
}

extern "C" fn event_get_memory_pool(e: u64) -> u64 {
    guard("nvnEventGetMemoryPool", || Obj(e).u64(OFF_POOL))
}

extern "C" fn event_get_memory_offset(e: u64) -> i64 {
    guard("nvnEventGetMemoryOffset", || Obj(e).i64(OFF_OFF))
}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnEventBuilderSetDefaults" => builder_set_defaults as extern "C" fn(u64),
        "nvnEventBuilderSetStorage" => builder_set_storage as extern "C" fn(u64, u64, i64),
        "nvnEventBuilderGetMemoryPool" => builder_get_memory_pool as extern "C" fn(u64) -> u64,
        "nvnEventBuilderGetMemoryOffset" => builder_get_memory_offset as extern "C" fn(u64) -> i64,
        "nvnEventInitialize" => event_initialize as extern "C" fn(u64, u64) -> u32,
        "nvnEventFinalize" => event_finalize as extern "C" fn(u64),
        "nvnEventGetValue" => event_get_value as extern "C" fn(u64) -> u32,
        "nvnEventSignal" => event_signal as extern "C" fn(u64, i32, u32),
        "nvnEventGetMemoryPool" => event_get_memory_pool as extern "C" fn(u64) -> u64,
        "nvnEventGetMemoryOffset" => event_get_memory_offset as extern "C" fn(u64) -> i64,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::testing;

    #[test]
    fn add_adds_with_wrap_and_every_other_mode_writes() {
        assert_eq!(signalled(MODE_ADD, 5, 3), 8);
        assert_eq!(signalled(MODE_ADD, u32::MAX, 2), 1);
        assert_eq!(signalled(0, 5, 3), 3);
    }

    #[test]
    fn an_event_signals_the_word_it_points_at() {
        let word = testing::alloc(8);
        let event = testing::alloc(crate::nvn::abi::size::EVENT);
        Obj(event).set_u64(OFF_VAL, word).expect("val");
        event_signal(event, 0, 10);
        assert_eq!(event_get_value(event), 10);
        event_signal(event, MODE_ADD, 5);
        assert_eq!(event_get_value(event), 15);
    }

    #[test]
    fn the_group_exports_every_name_of_its_row() {
        let got: std::collections::HashSet<&str> = procs().iter().map(|(n, _)| *n).collect();
        let want: std::collections::HashSet<&str> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "event")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        assert_eq!(got, want);
    }
}
