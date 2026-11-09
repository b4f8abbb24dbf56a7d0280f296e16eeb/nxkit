//! The memory pool group: `nvnMemoryPool*` and the pool builder.
//!
//! A pool is memory that the game owns. The game gives its address and size to the builder, and the host never
//! allocates for it. A guest address is a host address (R18), so [`Backend::register_pool`] gets the range as it is.
//!
//! The `NVNmemoryPool` is 0x100 bytes in guest memory. The host uses the first 0x18 bytes as plain data and writes no more:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | magic `POOL` while initialised |
//! | 4 | `NVNmemoryPoolFlags` |
//! | 8 | the host address of the storage, 0 for a virtual pool that has no mapping yet |
//! | 16 | the size in bytes |
//!
//! The offsets 4, 8 and 16 are the contract with the other groups, which read a pool with [`view`].
//!
//! The pool builder is 0x40 bytes in the headers. The host writes the first 0x1C bytes: device at 0, storage at 8,
//! size at 16, flags (u32) at 24.
//!
//! A virtual pool (flag 0x200) has no storage of its own. `nvnMemoryPoolMapVirtual` points it at a part of a physical
//! pool, and only a mapping that keeps the offsets of the physical pool is possible (the virtual pool is then a window
//! on the physical one). A request that needs a second, different base ends the game with the name of the function.
//! A sparse pool with holes and a virtual pool without a physical one are not implemented.

use nxkit_abi::guest;
use nxkit_nvn::backend::PoolInfo;

use super::abi::{AbiResult, Obj, guard, log_once, magic, other, ret_bool, table, used};
use super::ctx::gpu;

/// The magic of an initialised pool.
pub(super) const POOL: u32 = magic(b'P', b'O', b'O', b'L');
/// `NVN_MEMORY_POOL_FLAGS_CPU_NO_ACCESS_BIT`.
pub(crate) const FLAG_CPU_NO_ACCESS: u32 = 0x1;
/// `NVN_MEMORY_POOL_FLAGS_VIRTUAL_BIT`.
pub(crate) const FLAG_VIRTUAL: u32 = 0x200;

const OFF_FLAGS: u64 = 4;
const OFF_MEM: u64 = 8;
const OFF_SIZE: u64 = 16;
const B_DEV: u64 = 0;
const B_MEM: u64 = 8;
const B_SIZE: u64 = 16;
const B_FLAGS: u64 = 24;
/// The size of one `NVNmappingRequest`.
const REQUEST_SIZE: u64 = 0x28;

/// What a pool in guest memory says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PoolView {
    /// The host address of the storage. 0 when a virtual pool has no mapping yet.
    pub(crate) mem: u64,
    /// The size in bytes.
    pub(crate) size: u64,
    /// The `NVNmemoryPoolFlags`.
    pub(crate) flags: u32,
}

impl PoolView {
    /// True when the CPU may not touch the pool.
    pub(crate) fn cpu_no_access(self) -> bool {
        self.flags & FLAG_CPU_NO_ACCESS != 0
    }

    /// True for a virtual pool.
    pub(crate) fn is_virtual(self) -> bool {
        self.flags & FLAG_VIRTUAL != 0
    }
}

/// Reads the pool at the guest address `pool`.
pub(crate) fn view(pool: u64) -> AbiResult<PoolView> {
    let o = Obj(pool);
    Ok(PoolView {
        mem: o.u64(OFF_MEM)?,
        size: o.u64(OFF_SIZE)?,
        flags: o.u32(OFF_FLAGS)?,
    })
}

pub(crate) extern "C" fn builder_set_defaults(b: u64) {
    guard("nvnMemoryPoolBuilderSetDefaults", || {
        Obj(b).zero(used::POOL_BUILDER)
    });
}

pub(crate) extern "C" fn builder_set_device(b: u64, dev: u64) {
    guard("nvnMemoryPoolBuilderSetDevice", || {
        Obj(b).set_u64(B_DEV, dev)
    });
}

pub(crate) extern "C" fn builder_set_storage(b: u64, mem: u64, len: u64) {
    guard("nvnMemoryPoolBuilderSetStorage", || {
        Obj(b).set_u64(B_MEM, mem)?;
        Obj(b).set_u64(B_SIZE, len)
    });
}

pub(crate) extern "C" fn builder_set_flags(b: u64, flags: i32) {
    guard("nvnMemoryPoolBuilderSetFlags", || {
        Obj(b).set_u32(B_FLAGS, flags as u32)
    });
}

pub(crate) extern "C" fn pool_initialize(p: u64, b: u64) -> u32 {
    guard("nvnMemoryPoolInitialize", || {
        let bo = Obj(b);
        let (mem, len, flags) = (bo.u64(B_MEM)?, bo.u64(B_SIZE)?, bo.u32(B_FLAGS)?);
        let o = Obj(p);
        o.zero(used::POOL)?;
        o.set_u32(0, POOL)?;
        o.set_u32(OFF_FLAGS, flags)?;
        o.set_u64(OFF_MEM, mem)?;
        o.set_u64(OFF_SIZE, len)?;
        let virt = flags & FLAG_VIRTUAL != 0;
        if mem != 0 && !virt {
            guest::check(mem, len)?;
            gpu("nvnMemoryPoolInitialize")
                .backend()
                .register_pool(PoolInfo {
                    host: mem,
                    len,
                    flags,
                })?;
        } else if mem == 0 && !virt {
            eprintln!(
                "nxkit-rt: warning: NVN nvnMemoryPoolInitialize: a pool that is not virtual has no storage (flags {flags:#x}, size {len:#x})"
            );
        }
        Ok(1)
    })
}

pub(crate) extern "C" fn pool_finalize(p: u64) {
    guard("nvnMemoryPoolFinalize", || {
        let v = view(p)?;
        if v.mem != 0 && !v.is_virtual() {
            gpu("nvnMemoryPoolFinalize")
                .backend()
                .unregister_pool(v.mem);
        }
        Obj(p).set_u32(0, 0)
    });
}

extern "C" fn pool_set_debug_label(_p: u64, _label: u64) {}

extern "C" fn pool_map(p: u64) -> u64 {
    guard("nvnMemoryPoolMap", || {
        let v = view(p)?;
        Ok(if v.cpu_no_access() { 0 } else { v.mem })
    })
}

extern "C" fn pool_flush_mapped_range(p: u64, off: i64, n: u64) {
    guard("nvnMemoryPoolFlushMappedRange", || {
        let v = view(p)?;
        if v.mem == 0 || v.is_virtual() {
            return Ok(());
        }
        let start = v.mem.wrapping_add(off as u64);
        gpu("nvnMemoryPoolFlushMappedRange").guest_wrote_deferred(start, n);
        Ok(())
    });
}

extern "C" fn pool_invalidate_mapped_range(_p: u64, _off: i64, _n: u64) {}

extern "C" fn pool_get_buffer_address(p: u64) -> u64 {
    guard("nvnMemoryPoolGetBufferAddress", || {
        let v = view(p)?;
        if v.mem == 0 {
            log_once(
                "nvnMemoryPoolGetBufferAddress",
                "nvnMemoryPoolGetBufferAddress: the pool has no storage, the address is 0",
            );
            return Ok(0);
        }
        gpu("nvnMemoryPoolGetBufferAddress").gpu_address(v.mem, 0)
    })
}

extern "C" fn pool_get_size(p: u64) -> u64 {
    guard("nvnMemoryPoolGetSize", || Obj(p).u64(OFF_SIZE))
}

extern "C" fn pool_get_flags(p: u64) -> u32 {
    guard("nvnMemoryPoolGetFlags", || Obj(p).u32(OFF_FLAGS))
}

/// One `NVNmappingRequest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Request {
    phys: u64,
    poff: i64,
    voff: i64,
    size: u64,
}

fn read_request(at: Obj) -> AbiResult<Request> {
    Ok(Request {
        phys: at.u64(0)?,
        poff: at.i64(8)?,
        voff: at.i64(16)?,
        size: at.u64(24)?,
    })
}

/// What a request does to a virtual pool of `pool_size` bytes whose storage is at `current`.
#[derive(Debug, PartialEq, Eq)]
enum Mapping {
    /// The request is out of range of one of the pools.
    OutOfRange,
    /// The request needs a second base.
    Sparse { current: u64, wanted: u64 },
    /// The storage of the virtual pool starts here.
    Base(u64),
}

fn classify(r: &Request, pool_size: u64, phys: PoolView, current: u64) -> Mapping {
    if r.voff < 0
        || r.poff < 0
        || r.voff as u64 + r.size > pool_size
        || r.poff as u64 + r.size > phys.size
    {
        return Mapping::OutOfRange;
    }
    let base = phys
        .mem
        .wrapping_add(r.poff as u64)
        .wrapping_sub(r.voff as u64);
    if current != 0 && current != base {
        Mapping::Sparse {
            current,
            wanted: base,
        }
    } else {
        Mapping::Base(base)
    }
}

extern "C" fn pool_map_virtual(p: u64, n: i32, requests: u64) -> u32 {
    guard("nvnMemoryPoolMapVirtual", || {
        let o = Obj(p);
        let pool_size = o.u64(OFF_SIZE)?;
        for i in 0..n.max(0) as u64 {
            let r = read_request(Obj(requests).at(i * REQUEST_SIZE))?;
            if r.phys == 0 {
                continue;
            }
            let phys = view(r.phys)?;
            if phys.mem == 0 {
                eprintln!(
                    "nxkit-rt: warning: NVN nvnMemoryPoolMapVirtual: the physical pool {:#x} has no storage",
                    r.phys
                );
                return Ok(ret_bool(false));
            }
            match classify(&r, pool_size, phys, o.u64(OFF_MEM)?) {
                Mapping::OutOfRange => {
                    eprintln!(
                        "nxkit-rt: warning: NVN nvnMemoryPoolMapVirtual: request out of range (virtual {:#x}+{:#x} of {pool_size:#x}, physical {:#x} of {:#x})",
                        r.voff, r.size, r.poff, phys.size
                    );
                    return Ok(ret_bool(false));
                }
                Mapping::Sparse { current, wanted } => {
                    return Err(other(format!(
                        "a sparse virtual pool is not implemented: pool {p:#x} maps to {current:#x} and a request wants {wanted:#x}"
                    )));
                }
                Mapping::Base(base) => o.set_u64(OFF_MEM, base)?,
            }
        }
        Ok(ret_bool(true))
    })
}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnMemoryPoolBuilderSetDevice" => builder_set_device as extern "C" fn(u64, u64),
        "nvnMemoryPoolBuilderSetDefaults" => builder_set_defaults as extern "C" fn(u64),
        "nvnMemoryPoolBuilderSetStorage" => builder_set_storage as extern "C" fn(u64, u64, u64),
        "nvnMemoryPoolBuilderSetFlags" => builder_set_flags as extern "C" fn(u64, i32),
        "nvnMemoryPoolInitialize" => pool_initialize as extern "C" fn(u64, u64) -> u32,
        "nvnMemoryPoolFinalize" => pool_finalize as extern "C" fn(u64),
        "nvnMemoryPoolSetDebugLabel" => pool_set_debug_label as extern "C" fn(u64, u64),
        "nvnMemoryPoolMap" => pool_map as extern "C" fn(u64) -> u64,
        "nvnMemoryPoolFlushMappedRange" => pool_flush_mapped_range as extern "C" fn(u64, i64, u64),
        "nvnMemoryPoolInvalidateMappedRange" => pool_invalidate_mapped_range as extern "C" fn(u64, i64, u64),
        "nvnMemoryPoolGetBufferAddress" => pool_get_buffer_address as extern "C" fn(u64) -> u64,
        "nvnMemoryPoolMapVirtual" => pool_map_virtual as extern "C" fn(u64, i32, u64) -> u32,
        "nvnMemoryPoolGetSize" => pool_get_size as extern "C" fn(u64) -> u64,
        "nvnMemoryPoolGetFlags" => pool_get_flags as extern "C" fn(u64) -> u32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phys(mem: u64, size: u64) -> PoolView {
        PoolView {
            mem,
            size,
            flags: 0,
        }
    }

    #[test]
    fn a_request_gives_the_base_that_keeps_the_offsets_of_the_physical_pool() {
        let r = Request {
            phys: 1,
            poff: 0x2000,
            voff: 0x1000,
            size: 0x1000,
        };
        assert_eq!(
            classify(&r, 0x10000, phys(0x8000_0000, 0x8000), 0),
            Mapping::Base(0x8000_1000)
        );
    }

    #[test]
    fn a_second_request_with_the_same_base_is_accepted_and_another_base_is_sparse() {
        let a = Request {
            phys: 1,
            poff: 0,
            voff: 0,
            size: 0x1000,
        };
        let b = Request {
            phys: 1,
            poff: 0x1000,
            voff: 0x1000,
            size: 0x1000,
        };
        let c = Request {
            phys: 1,
            poff: 0x4000,
            voff: 0x1000,
            size: 0x1000,
        };
        let p = phys(0x8000_0000, 0x8000);
        let Mapping::Base(base) = classify(&a, 0x4000, p, 0) else {
            panic!("a is not a base");
        };
        assert_eq!(classify(&b, 0x4000, p, base), Mapping::Base(base));
        assert_eq!(
            classify(&c, 0x4000, p, base),
            Mapping::Sparse {
                current: base,
                wanted: base + 0x3000
            }
        );
    }

    #[test]
    fn a_request_past_either_pool_is_out_of_range() {
        let p = phys(0x8000_0000, 0x2000);
        let past_virtual = Request {
            phys: 1,
            poff: 0,
            voff: 0x3000,
            size: 0x2000,
        };
        let past_physical = Request {
            phys: 1,
            poff: 0x1000,
            voff: 0,
            size: 0x2000,
        };
        let negative = Request {
            phys: 1,
            poff: 0,
            voff: -1,
            size: 1,
        };
        assert_eq!(classify(&past_virtual, 0x4000, p, 0), Mapping::OutOfRange);
        assert_eq!(classify(&past_physical, 0x4000, p, 0), Mapping::OutOfRange);
        assert_eq!(classify(&negative, 0x4000, p, 0), Mapping::OutOfRange);
    }

    #[test]
    fn the_flags_tell_cpu_access_and_virtual_pools() {
        let v = PoolView {
            mem: 1,
            size: 1,
            flags: FLAG_CPU_NO_ACCESS | FLAG_VIRTUAL,
        };
        assert!(v.cpu_no_access());
        assert!(v.is_virtual());
        assert!(!phys(1, 1).cpu_no_access());
    }

    #[test]
    fn the_group_exports_every_name_of_its_row() {
        let got: std::collections::HashSet<&str> = procs().iter().map(|(n, _)| *n).collect();
        let want: std::collections::HashSet<&str> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "pool")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        assert_eq!(got, want);
    }
}
