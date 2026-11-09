//! The live memory pools of the game, for the memory report.
//!
//! `nxkit-rt` records a pool here when `nvnMemoryPoolInitialize` registers it with the backend, and drops it when
//! `nvnMemoryPoolFinalize` unregisters it. Virtual pools and pools without storage are never registered, so they are
//! not here. The record is read only: nothing in the front end or the backend looks at it. The host reads it with
//! [`snapshot`] to print `nxkit-mem pools`.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use crate::backend::PoolInfo;

/// `NVN_MEMORY_POOL_FLAGS_GPU_NO_ACCESS_BIT`: the GPU does not read or write the pool.
pub const FLAG_GPU_NO_ACCESS: u32 = 0x8;

static LIVE: Mutex<BTreeMap<u64, PoolInfo>> = Mutex::new(BTreeMap::new());
static MADE: AtomicU64 = AtomicU64::new(0);
static GONE: AtomicU64 = AtomicU64::new(0);

/// The pools now and the counters since the start of the process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PoolSnapshot {
    /// The live pools, by start address.
    pub pools: Vec<PoolInfo>,
    /// The pools made since the start.
    pub made: u64,
    /// The pools finalized since the start.
    pub gone: u64,
}

fn live() -> std::sync::MutexGuard<'static, BTreeMap<u64, PoolInfo>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A pool was registered. A second pool at the same start replaces the first, as the backend does.
pub fn made(pool: PoolInfo) {
    live().insert(pool.host, pool);
    MADE.fetch_add(1, Relaxed);
}

/// The pool at `host` was unregistered. A start that is not recorded changes nothing.
pub fn gone(host: u64) {
    if live().remove(&host).is_some() {
        GONE.fetch_add(1, Relaxed);
    }
}

/// The live pools and the counters.
pub fn snapshot() -> PoolSnapshot {
    let pools = live().values().copied().collect();
    PoolSnapshot {
        pools,
        made: MADE.load(Relaxed),
        gone: GONE.load(Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_comes_and_goes() {
        // the tests of the crate share the statics, so the addresses are far from any other test's
        let host = 0x7e57_0000_0000;
        let before = snapshot();
        made(PoolInfo {
            host,
            len: 0x4000,
            flags: 0x5,
        });
        let s = snapshot();
        assert!(s.pools.iter().any(|p| p.host == host && p.len == 0x4000));
        assert!(s.made > before.made);
        gone(host);
        gone(host);
        let s = snapshot();
        assert!(s.pools.iter().all(|p| p.host != host));
        assert!(s.gone > before.gone);
    }
}
