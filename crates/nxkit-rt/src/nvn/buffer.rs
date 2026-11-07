//! The buffer group: `nvnBuffer*` and the buffer builder.
//!
//! A buffer is a range of a memory pool. The `NVNbuffer` is 0x30 bytes of plain data in guest memory:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | magic `BUF\0` while initialised |
//! | 8 | the guest address of the `NVNmemoryPool` |
//! | 0x18 | the offset in the pool (i64) |
//! | 0x20 | the size (u64) |
//!
//! The offsets 0x18 and 0x20 are a contract with the games. The ring allocators of some engines write them
//! directly, and the host reads them at every call, so the host keeps no copy.
//!
//! The buffer builder is 0x40 bytes: device at 0, pool at 8, offset at 16, size at 24.
//!
//! The GPU address of a buffer is `GPU address of (pool storage + offset)`. A buffer over a pool without storage does not
//! initialise (the result is 0). A buffer that reaches past the end of its pool is logged once, because a game may
//! create it and never use the part outside the pool.

use super::abi::{AbiResult, Obj, guard, log_once, magic, size, table};
use super::ctx::gpu;
use super::pool::{self, PoolView};

/// The magic of an initialised buffer.
const BUF: u32 = magic(b'B', b'U', b'F', 0);

const OFF_POOL: u64 = 8;
const OFF_OFF: u64 = 0x18;
const OFF_SIZE: u64 = 0x20;
const B_DEV: u64 = 0;
const B_POOL: u64 = 8;
const B_OFF: u64 = 16;
const B_SIZE: u64 = 24;

/// What a buffer in guest memory says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BufferView {
    /// The guest address of the memory pool.
    pub(crate) pool: u64,
    /// The offset in the pool.
    pub(crate) offset: i64,
    /// The size in bytes.
    pub(crate) size: u64,
}

/// Reads the buffer at the guest address `buf`.
pub(crate) fn view(buf: u64) -> AbiResult<BufferView> {
    let o = Obj(buf);
    Ok(BufferView {
        pool: o.u64(OFF_POOL)?,
        offset: o.i64(OFF_OFF)?,
        size: o.u64(OFF_SIZE)?,
    })
}

/// The pool of a buffer, with the host address of the first byte of the buffer.
/// The address is 0 when the pool has no storage.
pub(crate) fn storage(buf: u64) -> AbiResult<(PoolView, u64)> {
    let b = view(buf)?;
    let p = pool::view(b.pool)?;
    let host = if p.mem == 0 {
        0
    } else {
        p.mem.wrapping_add(b.offset as u64)
    };
    Ok((p, host))
}

extern "C" fn builder_set_defaults(b: u64) {
    guard("nvnBufferBuilderSetDefaults", || {
        let dev = Obj(b).u64(B_DEV)?;
        Obj(b).zero(size::BUFFER_BUILDER)?;
        Obj(b).set_u64(B_DEV, dev)
    });
}

extern "C" fn builder_set_device(b: u64, dev: u64) {
    guard("nvnBufferBuilderSetDevice", || Obj(b).set_u64(B_DEV, dev));
}

extern "C" fn builder_set_storage(b: u64, pool: u64, off: i64, len: u64) {
    guard("nvnBufferBuilderSetStorage", || {
        let o = Obj(b);
        o.set_u64(B_POOL, pool)?;
        o.set_i64(B_OFF, off)?;
        o.set_u64(B_SIZE, len)
    });
}

extern "C" fn builder_get_memory_pool(b: u64) -> u64 {
    guard("nvnBufferBuilderGetMemoryPool", || Obj(b).u64(B_POOL))
}

extern "C" fn builder_get_memory_offset(b: u64) -> i64 {
    guard("nvnBufferBuilderGetMemoryOffset", || Obj(b).i64(B_OFF))
}

extern "C" fn builder_get_size(b: u64) -> u64 {
    guard("nvnBufferBuilderGetSize", || Obj(b).u64(B_SIZE))
}

extern "C" fn buffer_initialize(buf: u64, b: u64) -> u32 {
    guard("nvnBufferInitialize", || {
        let bo = Obj(b);
        let (pool, off, len) = (bo.u64(B_POOL)?, bo.i64(B_OFF)?, bo.u64(B_SIZE)?);
        if pool == 0 {
            eprintln!("[RED] NVN nvnBufferInitialize: the builder has no memory pool");
            return Ok(0);
        }
        let p = pool::view(pool)?;
        if p.mem == 0 {
            eprintln!(
                "[RED] NVN nvnBufferInitialize: the pool has no storage (pool {pool:#x}, flags {:#x}, offset {off:#x}, size {len:#x})",
                p.flags
            );
            return Ok(0);
        }
        if (off as u64).wrapping_add(len) > p.size {
            log_once(
                "nvnBufferInitialize-range",
                format_args!(
                    "nvnBufferInitialize: a buffer reaches past the end of its pool (offset {off:#x}, size {len:#x}, pool {:#x}), only an access outside the pool is an error",
                    p.size
                ),
            );
        }
        let o = Obj(buf);
        o.zero(size::BUFFER)?;
        o.set_u32(0, BUF)?;
        o.set_u64(OFF_POOL, pool)?;
        o.set_i64(OFF_OFF, off)?;
        o.set_u64(OFF_SIZE, len)?;
        Ok(1)
    })
}

extern "C" fn buffer_finalize(buf: u64) {
    guard("nvnBufferFinalize", || Obj(buf).set_u32(0, 0));
}

extern "C" fn buffer_set_debug_label(_buf: u64, _label: u64) {}

extern "C" fn buffer_get_address(buf: u64) -> u64 {
    guard("nvnBufferGetAddress", || {
        let (_, host) = storage(buf)?;
        gpu("nvnBufferGetAddress").gpu_address(host, 0)
    })
}

extern "C" fn buffer_map(buf: u64) -> u64 {
    guard("nvnBufferMap", || {
        let (p, host) = storage(buf)?;
        Ok(if p.cpu_no_access() { 0 } else { host })
    })
}

extern "C" fn buffer_flush_mapped_range(_buf: u64, _off: i64, _n: u64) {}

extern "C" fn buffer_invalidate_mapped_range(_buf: u64, _off: i64, _n: u64) {}

extern "C" fn buffer_get_memory_pool(buf: u64) -> u64 {
    guard("nvnBufferGetMemoryPool", || Obj(buf).u64(OFF_POOL))
}

extern "C" fn buffer_get_memory_offset(buf: u64) -> i64 {
    guard("nvnBufferGetMemoryOffset", || Obj(buf).i64(OFF_OFF))
}

extern "C" fn buffer_get_size(buf: u64) -> u64 {
    guard("nvnBufferGetSize", || Obj(buf).u64(OFF_SIZE))
}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnBufferBuilderSetDevice" => builder_set_device as extern "C" fn(u64, u64),
        "nvnBufferBuilderSetDefaults" => builder_set_defaults as extern "C" fn(u64),
        "nvnBufferBuilderSetStorage" => builder_set_storage as extern "C" fn(u64, u64, i64, u64),
        "nvnBufferBuilderGetMemoryPool" => builder_get_memory_pool as extern "C" fn(u64) -> u64,
        "nvnBufferBuilderGetMemoryOffset" => builder_get_memory_offset as extern "C" fn(u64) -> i64,
        "nvnBufferBuilderGetSize" => builder_get_size as extern "C" fn(u64) -> u64,
        "nvnBufferInitialize" => buffer_initialize as extern "C" fn(u64, u64) -> u32,
        "nvnBufferFinalize" => buffer_finalize as extern "C" fn(u64),
        "nvnBufferSetDebugLabel" => buffer_set_debug_label as extern "C" fn(u64, u64),
        "nvnBufferMap" => buffer_map as extern "C" fn(u64) -> u64,
        "nvnBufferGetAddress" => buffer_get_address as extern "C" fn(u64) -> u64,
        "nvnBufferFlushMappedRange" => buffer_flush_mapped_range as extern "C" fn(u64, i64, u64),
        "nvnBufferInvalidateMappedRange" => buffer_invalidate_mapped_range as extern "C" fn(u64, i64, u64),
        "nvnBufferGetMemoryPool" => buffer_get_memory_pool as extern "C" fn(u64) -> u64,
        "nvnBufferGetMemoryOffset" => buffer_get_memory_offset as extern "C" fn(u64) -> i64,
        "nvnBufferGetSize" => buffer_get_size as extern "C" fn(u64) -> u64,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::testing;

    #[test]
    fn a_buffer_reads_its_pool_and_range_from_the_fixed_offsets() {
        let base = testing::alloc(0x40);
        Obj(base).set_u64(OFF_POOL, 0xAAA).expect("pool");
        Obj(base).set_i64(OFF_OFF, 0x1000).expect("off");
        Obj(base).set_u64(OFF_SIZE, 0x200).expect("size");
        assert_eq!(
            view(base).expect("view"),
            BufferView {
                pool: 0xAAA,
                offset: 0x1000,
                size: 0x200
            }
        );
    }

    #[test]
    fn the_group_exports_every_name_of_its_row() {
        let got: std::collections::HashSet<&str> = procs().iter().map(|(n, _)| *n).collect();
        let want: std::collections::HashSet<&str> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "buffer")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        assert_eq!(got, want);
    }
}
