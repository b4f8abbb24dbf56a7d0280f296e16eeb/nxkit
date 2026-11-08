//! The descriptor pool objects of the texture pool and the sampler pool.
//!
//! An `NVNtexturePool` and an `NVNsamplerPool` are 0x20 bytes in the headers. The host uses the first 0x1C bytes of them
//! as plain data and writes no byte past them:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | the guest address of the `NVNmemoryPool` |
//! | 8 | the offset in the memory pool (i64) |
//! | 0x10 | the number of descriptors (i32) |
//! | 0x14 | the magic while initialised (`TPL\0` or `SPL\0`) |
//! | 0x18 | the first id of the pool in the id space of its kind (u32) |
//!
//! A texture id or a sampler id that a game passes is an index in the pool that the command buffer has bound. The front
//! end keeps one id space for each kind, so every pool gets a range of it: its base is the end of the previous range
//! rounded up to 256. A registration goes to `base + id`, and a command buffer that binds a pool adds its base to the
//! handles that it records. [`DescPools::base`] gives the base.
//!
//! A pool that is initialised again at the same address with the same size and memory keeps its range. Any other change
//! takes a new range, because the front end cannot resize or drop a range.
//!
//! The allocator is one atomic counter. A registration takes no lock of its own.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::nvn::abi::{AbiResult, Obj, Registry, other, used};
use crate::nvn::pool as memory_pool;

/// The bytes of one descriptor in a pool.
pub(in crate::nvn) const HEADER_BYTES: u64 = nxkit_nvn::tic::HEADER_BYTES as u64;

const OFF_POOL: u64 = 0;
const OFF_OFF: u64 = 8;
const OFF_N: u64 = 0x10;
const OFF_MAGIC: u64 = 0x14;
const OFF_BASE: u64 = 0x18;

/// The range of ids of one pool object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Range {
    base: u32,
    count: u32,
    host: u64,
}

/// Where the descriptor of one slot is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::nvn) struct Slot {
    /// The id in the id space of the kind, which is the base of the pool plus the id of the game.
    pub(in crate::nvn) id: u32,
    /// The host address of the 32 bytes of the descriptor.
    pub(in crate::nvn) header: u64,
}

/// The pools of one kind.
#[derive(Debug)]
pub(in crate::nvn) struct DescPools {
    magic: u32,
    next: AtomicU32,
    ranges: Registry<Range>,
}

impl DescPools {
    /// An empty set of pools. `magic` marks an initialised pool object.
    pub(in crate::nvn) fn new(magic: u32) -> Self {
        Self {
            magic,
            next: AtomicU32::new(0),
            ranges: Registry::new(),
        }
    }

    /// `nvn*PoolInitialize`. `add` makes the range known to the front end. It gets the base, the count and the host
    /// address of the descriptors, and it runs only when the pool takes a new range. The result is the `NVNboolean`.
    /// A pool that cannot be built is logged and gives 0.
    pub(in crate::nvn) fn initialize(
        &self,
        name: &str,
        obj: u64,
        memory: (u64, i64),
        n: i32,
        add: impl FnOnce(u32, u32, u64) -> AbiResult<()>,
    ) -> AbiResult<u32> {
        let (mem_pool, off) = memory;
        if n <= 0 || off < 0 || mem_pool == 0 {
            eprintln!(
                "[RED] NVN {name}: bad arguments (pool {mem_pool:#x}, offset {off:#x}, {n} descriptors)"
            );
            return Ok(0);
        }
        let count = n as u32;
        let bytes = u64::from(count) * HEADER_BYTES;
        let p = memory_pool::view(mem_pool)?;
        if p.mem == 0 {
            eprintln!(
                "[RED] NVN {name}: the memory pool has no storage (pool {mem_pool:#x}, flags {:#x})",
                p.flags
            );
            return Ok(0);
        }
        if (off as u64)
            .checked_add(bytes)
            .is_none_or(|end| end > p.size)
        {
            eprintln!(
                "[RED] NVN {name}: {count} descriptors at offset {off:#x} do not fit the memory pool of {:#x} bytes",
                p.size
            );
            return Ok(0);
        }
        let host = p.mem + off as u64;
        let base = match self.ranges.get(obj) {
            Some(r) if r.count == count && r.host == host => r.base,
            _ => {
                let base = self
                    .next
                    .fetch_add(count.div_ceil(256).saturating_mul(256), Ordering::Relaxed);
                add(base, count, host)?;
                self.ranges.insert(obj, Range { base, count, host });
                base
            }
        };
        let o = Obj(obj);
        o.zero(used::DESCRIPTOR_POOL)?;
        o.set_u64(OFF_POOL, mem_pool)?;
        o.set_i64(OFF_OFF, off)?;
        o.set_i32(OFF_N, n)?;
        o.set_u32(OFF_MAGIC, self.magic)?;
        o.set_u32(OFF_BASE, base)?;
        Ok(1)
    }

    /// `nvn*PoolFinalize`. Gives the range of the pool when it was initialised, so the caller can release what it
    /// registered in it. The range stays reserved for a later initialise of the same pool.
    pub(in crate::nvn) fn finalize(&self, obj: u64) -> AbiResult<Option<(u32, u32)>> {
        let o = Obj(obj);
        if o.u32(OFF_MAGIC)? != self.magic {
            return Ok(None);
        }
        let range = (o.u32(OFF_BASE)?, o.u32(OFF_N)? as u32);
        o.set_u32(OFF_MAGIC, 0)?;
        Ok(Some(range))
    }

    /// The first id of the pool in the id space of its kind.
    pub(in crate::nvn) fn base(&self, obj: u64) -> AbiResult<u32> {
        let o = Obj(obj);
        if o.u32(OFF_MAGIC)? != self.magic {
            return Err(other(format!(
                "the descriptor pool at {obj:#x} is not initialised"
            )));
        }
        o.u32(OFF_BASE)
    }

    /// The slot `id` of the pool at `obj`.
    pub(in crate::nvn) fn slot(&self, obj: u64, id: i32) -> AbiResult<Slot> {
        let base = self.base(obj)?;
        let o = Obj(obj);
        let n = o.i32(OFF_N)?;
        if !(0..n).contains(&id) {
            return Err(other(format!(
                "the descriptor id {id} is outside the pool of {n} descriptors"
            )));
        }
        let mem = memory_pool::view(o.u64(OFF_POOL)?)?.mem;
        if mem == 0 {
            return Err(other(
                "the memory pool of the descriptor pool has no storage",
            ));
        }
        Ok(Slot {
            id: base + id as u32,
            header: mem + o.i64(OFF_OFF)? as u64 + id as u64 * HEADER_BYTES,
        })
    }
}

/// `nvn*PoolGetMemoryPool`.
pub(in crate::nvn) fn memory_pool_of(obj: u64) -> AbiResult<u64> {
    Obj(obj).u64(OFF_POOL)
}

/// `nvn*PoolGetMemoryOffset`.
pub(in crate::nvn) fn memory_offset_of(obj: u64) -> AbiResult<i64> {
    Obj(obj).i64(OFF_OFF)
}

/// `nvn*PoolGetSize`.
pub(in crate::nvn) fn size_of(obj: u64) -> AbiResult<i32> {
    Obj(obj).i32(OFF_N)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::{magic, testing};

    fn memory_pool_object(len: u64) -> u64 {
        let mem = testing::alloc(len);
        let pool = testing::alloc(0x100);
        Obj(pool).set_u64(8, mem).expect("mem");
        Obj(pool).set_u64(16, len).expect("size");
        pool
    }

    #[test]
    fn pools_take_consecutive_ranges_rounded_to_256() {
        let pools = DescPools::new(magic(b'T', b'P', b'L', 0));
        let mp = memory_pool_object(0x10000);
        let (a, b) = (testing::alloc(0x20), testing::alloc(0x20));
        let mut added = Vec::new();
        let mut add = |base, count, _host| {
            added.push((base, count));
            Ok(())
        };
        assert_eq!(
            pools.initialize("t", a, (mp, 0), 300, &mut add).expect("a"),
            1
        );
        assert_eq!(
            pools
                .initialize("t", b, (mp, 0x4000), 16, &mut add)
                .expect("b"),
            1
        );
        assert_eq!(added, vec![(0, 300), (512, 16)]);
        assert_eq!(pools.base(b).expect("base"), 512);
    }

    #[test]
    fn a_pool_initialised_again_with_the_same_shape_keeps_its_range() {
        let pools = DescPools::new(magic(b'S', b'P', b'L', 0));
        let mp = memory_pool_object(0x10000);
        let a = testing::alloc(0x20);
        let mut added = 0;
        let mut add = |_, _, _| {
            added += 1;
            Ok(())
        };
        pools
            .initialize("t", a, (mp, 0), 64, &mut add)
            .expect("first");
        assert_eq!(pools.finalize(a).expect("finalize"), Some((0, 64)));
        assert!(pools.base(a).is_err());
        pools
            .initialize("t", a, (mp, 0), 64, &mut add)
            .expect("second");
        assert_eq!(pools.base(a).expect("base"), 0);
        assert_eq!(added, 1);
    }

    #[test]
    fn a_slot_is_the_id_plus_the_base_and_its_header_is_32_bytes_apart() {
        let pools = DescPools::new(magic(b'T', b'P', b'L', 0));
        let mp = memory_pool_object(0x10000);
        let mem = Obj(mp).u64(8).expect("mem");
        let (a, b) = (testing::alloc(0x20), testing::alloc(0x20));
        pools
            .initialize("t", a, (mp, 0), 8, |_, _, _| Ok(()))
            .expect("a");
        pools
            .initialize("t", b, (mp, 0x1000), 8, |_, _, _| Ok(()))
            .expect("b");
        assert_eq!(
            pools.slot(b, 3).expect("slot"),
            Slot {
                id: 256 + 3,
                header: mem + 0x1000 + 3 * 32
            }
        );
        assert!(pools.slot(b, 8).is_err());
    }

    #[test]
    fn a_pool_that_does_not_fit_its_memory_does_not_initialise() {
        let pools = DescPools::new(magic(b'T', b'P', b'L', 0));
        let mp = memory_pool_object(0x1000);
        let a = testing::alloc(0x20);
        assert_eq!(
            pools
                .initialize("t", a, (mp, 0), 1000, |_, _, _| Ok(()))
                .expect("init"),
            0
        );
    }
}
