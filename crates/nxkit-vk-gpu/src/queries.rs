//! Query results, written when the submit that wrote them completes (the reference layer's `query_record` and
//! `copy_queries`).
//!
//! A timestamp is the completion time of the submit in ns (`timestampPeriod` 1). An occlusion query is not counted
//! yet: it reports 1, so a game that culls by it draws everything rather than nothing. The writes of a submit are kept
//! in command order and applied at its completion, then the copies into buffers (`vkCmdCopyQueryPoolResults`) are
//! made from the values.

use std::collections::HashMap;

use nxkit_vk::cmd::Cmd;
use nxkit_vk::desc::{BufferRef, QueryPoolId};

/// `VK_QUERY_RESULT_64_BIT`.
const RESULT_64: u32 = 0x1;
/// `VK_QUERY_RESULT_WITH_AVAILABILITY_BIT`.
const WITH_AVAILABILITY: u32 = 0x4;

/// The value that an occlusion query reports.
pub(crate) const OCCLUSION_VISIBLE: u64 = 1;

/// One query write of a submit, in command order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum QueryOp {
    /// `vkCmdResetQueryPool`.
    Reset(QueryPoolId, u32, u32),
    /// `vkCmdWriteTimestamp`.
    Timestamp(QueryPoolId, u32),
    /// `vkCmdEndQuery` of an occlusion query.
    Occlusion(QueryPoolId, u32),
    /// `vkCmdCopyQueryPoolResults`.
    Copy {
        /// The pool.
        pool: QueryPoolId,
        /// The first query.
        first: u32,
        /// The queries.
        count: u32,
        /// The destination.
        dst: BufferRef,
        /// The bytes from one query to the next.
        stride: u64,
        /// `VkQueryResultFlags`.
        flags: u32,
    },
}

/// The query writes of a command list.
pub(crate) fn ops<'a>(cmds: impl IntoIterator<Item = &'a Cmd>) -> Vec<QueryOp> {
    cmds.into_iter()
        .filter_map(|c| match *c {
            Cmd::ResetQueryPool { pool, first, count } => Some(QueryOp::Reset(pool, first, count)),
            Cmd::WriteTimestamp { pool, query } => Some(QueryOp::Timestamp(pool, query)),
            Cmd::EndQuery { pool, query } => Some(QueryOp::Occlusion(pool, query)),
            Cmd::CopyQueryPoolResults {
                pool,
                first,
                count,
                dst,
                stride,
                flags,
            } => Some(QueryOp::Copy {
                pool,
                first,
                count,
                dst,
                stride,
                flags,
            }),
            _ => None,
        })
        .collect()
}

/// The bytes of one query result: the value as 4 or 8 bytes, then the availability word when it is asked for.
pub(crate) fn result_bytes(value: u64, flags: u32) -> Vec<u8> {
    let wide = flags & RESULT_64 != 0;
    let word = |v: u64| {
        if wide {
            v.to_le_bytes().to_vec()
        } else {
            (v.min(u64::from(u32::MAX)) as u32).to_le_bytes().to_vec()
        }
    };
    let mut out = word(value);
    if flags & WITH_AVAILABILITY != 0 {
        out.extend(word(1));
    }
    out
}

/// A write into memory that a copy makes: the memory, the offset and the bytes.
pub(crate) type MemoryWrite = (BufferRef, Vec<u8>);

/// Applies the writes of a submit that completed at `now_ns` to the pools, and gives the bytes that its copies write.
pub(crate) fn apply(
    pools: &mut HashMap<QueryPoolId, Vec<u64>>,
    ops: &[QueryOp],
    now_ns: u64,
) -> Vec<MemoryWrite> {
    let mut writes = Vec::new();
    let set = |pools: &mut HashMap<QueryPoolId, Vec<u64>>, p: QueryPoolId, q: u32, v: u64| {
        if let Some(slot) = pools.get_mut(&p).and_then(|v| v.get_mut(q as usize)) {
            *slot = v;
        }
    };
    for op in ops {
        match *op {
            QueryOp::Reset(p, first, count) => {
                for q in first..first.saturating_add(count) {
                    set(pools, p, q, 0);
                }
            }
            QueryOp::Timestamp(p, q) => set(pools, p, q, now_ns),
            QueryOp::Occlusion(p, q) => set(pools, p, q, OCCLUSION_VISIBLE),
            QueryOp::Copy {
                pool,
                first,
                count,
                dst,
                stride,
                flags,
            } => {
                let values = pools.get(&pool);
                for i in 0..count {
                    let v = values
                        .and_then(|v| v.get((first + i) as usize))
                        .copied()
                        .unwrap_or(0);
                    let at = BufferRef {
                        offset: dst.offset + u64::from(i) * stride,
                        ..dst
                    };
                    writes.push((at, result_bytes(v, flags)));
                }
            }
        }
    }
    writes
}

#[cfg(test)]
mod tests {
    use nxkit_vk::desc::MemoryId;

    use super::*;

    #[test]
    fn results_have_the_width_and_availability_asked_for() {
        assert_eq!(result_bytes(5, 0), vec![5, 0, 0, 0]);
        assert_eq!(result_bytes(5, RESULT_64), vec![5, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            result_bytes(u64::MAX, WITH_AVAILABILITY),
            vec![255, 255, 255, 255, 1, 0, 0, 0]
        );
    }

    #[test]
    fn writes_apply_in_order_and_copies_read_them() {
        let p = QueryPoolId(3);
        let mut pools = HashMap::from([(p, vec![7, 7, 7])]);
        let dst = BufferRef {
            memory: MemoryId(1),
            offset: 64,
            size: 64,
        };
        let cmds = [
            Cmd::ResetQueryPool {
                pool: p,
                first: 0,
                count: 3,
            },
            Cmd::WriteTimestamp { pool: p, query: 1 },
            Cmd::EndQuery { pool: p, query: 2 },
            Cmd::CopyQueryPoolResults {
                pool: p,
                first: 0,
                count: 3,
                dst,
                stride: 8,
                flags: RESULT_64,
            },
            Cmd::PipelineBarrier,
        ];
        let o = ops(&cmds);
        assert_eq!(o.len(), 4);
        let w = apply(&mut pools, &o, 1000);
        assert_eq!(pools.get(&p), Some(&vec![0, 1000, OCCLUSION_VISIBLE]));
        assert_eq!(w.len(), 3);
        assert_eq!(w[1].0.offset, 72);
        assert_eq!(w[1].1, 1000u64.to_le_bytes().to_vec());
    }
}
