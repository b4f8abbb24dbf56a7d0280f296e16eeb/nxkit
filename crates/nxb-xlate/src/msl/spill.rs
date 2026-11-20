//! Compute threadgroup memory past what Metal gives (`TG_MAX`, 32 KB), as an emitter choice.
//!
//! Apple GPUs refuse a pipeline that declares more threadgroup memory than that, and Ampere gives up to 100 KB. A kernel
//! over the limit takes its arrays from a device buffer the host binds at `TG_SLOT`: one slice of the arrays' total per
//! workgroup, at the workgroup's index in the dispatch modulo `TG_GROUPS`. The threadgroup barriers order device memory
//! too and the threadgroup atomics become device atomics. The reflection tells the host the bytes of one slice as
//! `tg_spill`.

use super::abi::{TG_GROUPS, TG_MAX, TG_SLOT};
use super::emit::R;
use super::io::Builtin;
use super::types::{msl_type, scalar};
use crate::props::MemoryDef;
use std::collections::BTreeMap;
use std::fmt::Write;

/// The name of the workgroup index parameter the spill adds when the kernel has none.
const TGID: &str = "dm_tgid";

/// The device layout of the shared memories of a kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spill {
    /// The bytes of one workgroup's slice.
    pub total: i64,
    /// The byte offset of each memory in the slice, by memory id.
    pub offsets: BTreeMap<i32, i64>,
}

/// Bytes of a shared memory in the slice, rounded up to 16.
fn bytes(m: &MemoryDef) -> i64 {
    (4 * i64::from(m.array_length.max(1)) + 15) & !15
}

/// The slice bytes of all shared memories.
pub fn total_bytes(mems: &[MemoryDef]) -> i64 {
    mems.iter().map(bytes).sum()
}

/// The layout when the shared memories are over the limit, None when Metal's own threadgroup memory is enough.
pub fn plan(mems: &[MemoryDef]) -> Option<Spill> {
    let total = total_bytes(mems);
    if total <= i64::from(TG_MAX) {
        return None;
    }
    let mut at = 0;
    let mut offsets = BTreeMap::new();
    for m in mems {
        offsets.insert(m.id, at);
        at += bytes(m);
    }
    Some(Spill { total, offsets })
}

impl Spill {
    /// The helper parameter of a memory: a reference to its device array.
    pub fn param(m: &MemoryDef) -> R<String> {
        let t = msl_type(scalar(m.ty))?;
        Ok(format!(
            "device {t} (&sm{})[{}]",
            m.id,
            m.array_length.max(1)
        ))
    }

    /// The entry parameters the spill adds. The workgroup index is the kernel's own when it has one.
    pub fn entry_params(&self, builtins: &[Builtin]) -> Vec<String> {
        let mut p = Vec::new();
        if !builtins
            .iter()
            .any(|b| b.attr == "threadgroup_position_in_grid")
        {
            p.push(format!("uint3 {TGID} [[threadgroup_position_in_grid]]"));
        }
        p.push("uint3 dm_ntg [[threadgroups_per_grid]]".into());
        p.push(format!("device uchar* dm_tg [[buffer({TG_SLOT})]]"));
        p
    }

    /// The declarations at the top of the kernel: the slice base and a reference per memory.
    pub fn decls(&self, mems: &[MemoryDef], builtins: &[Builtin]) -> R<String> {
        let gid = builtins
            .iter()
            .find(|b| b.attr == "threadgroup_position_in_grid")
            .map_or(TGID, |b| b.name);
        let mut t = String::new();
        let _ = writeln!(
            t,
            "    ulong dm_tgbase = ulong((({gid}.z * dm_ntg.y + {gid}.y) * dm_ntg.x + {gid}.x) % {TG_GROUPS}u) * {}ul;",
            self.total
        );
        for m in mems {
            let ty = msl_type(scalar(m.ty))?;
            let n = m.array_length.max(1);
            let off = self.offsets.get(&m.id).copied().unwrap_or(0);
            let _ = writeln!(
                t,
                "    device {ty} (&sm{})[{n}] = *(device {ty} (*)[{n}])(dm_tg + dm_tgbase + {off});",
                m.id
            );
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::agg::AggType;

    fn mem(id: i32, n: i32) -> MemoryDef {
        MemoryDef {
            id,
            name: format!("sm{id}"),
            ty: AggType(AggType::U32.0 | AggType::ARRAY.0),
            array_length: n,
            zero_init: false,
        }
    }

    #[test]
    fn small_memory_stays_in_the_threadgroup() {
        assert!(plan(&[mem(0, 8192)]).is_none());
        assert_eq!(total_bytes(&[mem(0, 8192)]), 0x8000);
    }

    #[test]
    fn offsets_are_16_byte_aligned() {
        let s = plan(&[mem(0, 12288), mem(1, 3)]).expect("spill");
        assert_eq!(s.total, 48 * 1024 + 16);
        assert_eq!(s.offsets.get(&0), Some(&0));
        assert_eq!(s.offsets.get(&1), Some(&(48 * 1024)));
    }

    #[test]
    fn the_workgroup_index_is_reused() {
        let s = plan(&[mem(0, 12288)]).expect("spill");
        let wg = Builtin {
            name: "gl_WorkGroupID",
            ty: "uint3",
            attr: "threadgroup_position_in_grid",
        };
        assert_eq!(s.entry_params(&[wg]).len(), 2);
        assert_eq!(s.entry_params(&[]).len(), 3);
        let d = s.decls(&[mem(0, 12288)], &[wg]).expect("decls");
        assert!(d.contains("gl_WorkGroupID.z * dm_ntg.y"), "{d}");
        assert!(d.contains("device uint (&sm0)[12288]"), "{d}");
    }
}
