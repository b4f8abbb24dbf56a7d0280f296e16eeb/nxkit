//! The member pages of the mirror: page runs, the ranges a frame reads and their closure.

use super::*;

impl PageRuns {
    pub(super) fn contains(&self, p: u32) -> bool {
        let i = self.0.partition_point(|&(_, e)| e <= p);
        self.0.get(i).is_some_and(|&(s, _)| s <= p)
    }

    pub(super) fn add(&mut self, runs: &[(u32, u32)]) {
        if runs.is_empty() {
            return;
        }
        let mut v = std::mem::take(&mut self.0);
        v.extend_from_slice(runs);
        v.sort_unstable();
        let mut out: Vec<(u32, u32)> = Vec::with_capacity(v.len());
        for (s, e) in v {
            match out.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => out.push((s, e)),
            }
        }
        self.0 = out;
    }
}

impl Members<'_> {
    fn is(&self, p: u32) -> bool {
        self.bits
            .get(p as usize / 64)
            .is_some_and(|w| w >> (p % 64) & 1 != 0)
            || self.added.contains(p)
    }

    fn any(&self, first: u32, end: u32) -> bool {
        (first..end).any(|p| self.is(p))
    }

    fn all(&self, first: u32, end: u32) -> bool {
        (first..end).all(|p| self.is(p))
    }
}

/// How much of a vertex stream a draw reads, at most: the vertices of an arrays draw, the instances of an instanced one.
/// An indexed draw reads what its indices reach, which the host does not know here: a part of `UNKNOWN_BYTES`.
pub(super) fn stream_extent(s: &StreamDetail, args: &DrawArgs) -> u64 {
    let stride = u64::from(s.stride).max(16);
    let per_instance = |instances: u32, base: u32| {
        stride * (u64::from(base) + u64::from(instances)).div_ceil(u64::from(s.divisor.max(1)))
    };
    let want = match *args {
        DrawArgs::Arrays {
            first,
            count,
            instances,
            base_instance,
        } => {
            if s.divisor == 0 {
                stride * (u64::from(first) + u64::from(count))
            } else {
                per_instance(instances, base_instance)
            }
        }
        DrawArgs::Indexed {
            instances,
            base_instance,
            ..
        } if s.divisor != 0 => per_instance(instances, base_instance),
        _ => UNKNOWN_BYTES,
    };
    let bound = if s.size == 0 { UNKNOWN_BYTES } else { s.size };
    want.min(bound).max(1)
}

/// The ranges (GPU address, length) that the frame's GPU work reads through a binding: uniform and storage buffers,
/// vertex streams, indices, indirect arguments and copy sources. A binding that touches a member page makes the rest
/// of its range members too.
///
/// The third number is the kind of binding: [`UBO`], [`SBUF`], [`STREAM`], [`INDEX`], [`INDIRECT`] or [`COPY`].
pub(super) fn read_ranges(detail: &FrameDetail) -> Vec<(u64, u64, u8)> {
    let mut out: Vec<(u64, u64, u8)> = Vec::new();
    let mut seen = vec![(0u64, 0u64, 0u8); 4096];
    let mut add = |a: u64, len: u64, kind: u8| {
        if a == 0 || len == 0 {
            return;
        }
        let at = ((a >> 4) ^ (a >> 17) ^ len) as usize & 4095;
        if seen[at] == (a, len, kind) {
            return;
        }
        seen[at] = (a, len, kind);
        out.push((a, len, kind));
    };
    let mut prev = 0usize;
    for d in &detail.draws {
        let ptr = Arc::as_ptr(d) as usize;
        if ptr == prev {
            continue;
        }
        prev = ptr;
        for u in &d.ubos {
            add(u.address, u64::from(u.size), BIND_UBO);
        }
        for u in &d.sbufs {
            add(u.address, u64::from(u.size), BIND_SBUF);
        }
        for s in &d.streams {
            add(s.address, stream_extent(s, &d.args), BIND_STREAM);
        }
        match d.args {
            DrawArgs::Indexed {
                index_type,
                address,
                count,
                ..
            } => add(address, u64::from(count) << index_type.min(2), BIND_INDEX),
            DrawArgs::ArraysIndirect { indirect } => add(indirect, 16, BIND_INDIRECT),
            DrawArgs::IndexedIndirect {
                address, indirect, ..
            } => {
                add(address, UNKNOWN_BYTES, BIND_INDEX);
                add(indirect, 20, BIND_INDIRECT);
            }
            DrawArgs::MultiIndirectCount {
                index,
                indirect,
                count,
                max,
                stride,
            } => {
                if let Some((_, a)) = index {
                    add(a, UNKNOWN_BYTES, BIND_INDEX);
                }
                add(
                    indirect,
                    (u64::from(max) * stride.max(20)).min(RANGE_MAX_BYTES),
                    BIND_INDIRECT,
                );
                add(count, 4, BIND_INDIRECT);
            }
            DrawArgs::MeshTasksIndirect { indirect } => add(indirect, 8, BIND_INDIRECT),
            _ => {}
        }
    }
    for d in &detail.dispatches {
        for u in &d.ubos {
            add(u.address, u64::from(u.size), BIND_UBO);
        }
        for u in &d.sbufs {
            add(u.address, u64::from(u.size), BIND_SBUF);
        }
        if let Some(a) = d.indirect {
            add(a, 12, BIND_INDIRECT);
        }
    }
    for c in &detail.copies {
        match c.kind {
            CopyKind::BufferToBuffer => add(c.src_addr, c.bytes, BIND_COPY),
            CopyKind::BufferToTexture => add(c.buffer, UNKNOWN_BYTES, BIND_COPY),
            _ => {}
        }
    }
    out
}

/// The kinds of binding of [`read_ranges`], and their names for the report.
const BIND_UBO: u8 = 0;
const BIND_SBUF: u8 = 1;
const BIND_STREAM: u8 = 2;
const BIND_INDEX: u8 = 3;
const BIND_INDIRECT: u8 = 4;
pub(super) const BIND_COPY: u8 = 5;
pub(super) const BIND_NAMES: [&str; 6] = [
    "uniform",
    "storage",
    "vertex",
    "index",
    "indirect",
    "copy source",
];

/// The host address of the GPU range `[a, a + len)` and its length cut to the end of guest memory.
fn clamped_host(map: &AddressMap, a: u64, len: u64) -> Option<(u64, u64)> {
    let h = map.host_address(a, 1).ok()?;
    let room = map.host_base() + map.len() - h;
    Some((h, len.min(room)))
}

/// The most rounds of the closure: a range that is made whole can touch another binding, which makes its range whole in
/// the next round. The rounds stop when a round adds nothing.
const CLOSURE_ROUNDS: usize = 8;

/// The pages that become members because a binding of the frame touches a member (the written ranges included): rounds
/// until nothing is added, at most [`CLOSURE_ROUNDS`].
pub(super) fn closure(
    pages: &Pages,
    reads: &[(u64, u64, u8)],
    map: &AddressMap,
    members: &mut Members<'_>,
) -> u64 {
    // The page range of each binding, once. Members only grow, so a range that is whole stays whole and leaves the list;
    // the others are looked at again in the next round.
    let mut todo: Vec<(u32, u32)> = Vec::with_capacity(reads.len());
    for &(a, len, _) in reads {
        let len = len.min(RANGE_MAX_BYTES);
        // A range that reaches past the end of guest memory (a placeholder size) is cut to it.
        let Some((h, len)) = clamped_host(map, a, len) else {
            continue;
        };
        if let Some((first, end)) = pages.page_range(h, len)
            && !members.all(first, end)
        {
            todo.push((first, end));
        }
    }
    let page = pages.page_size();
    let mut added_pages = 0u64;
    for _ in 0..CLOSURE_ROUNDS {
        let mut new: Vec<(u32, u32)> = Vec::new();
        for &(first, end) in &todo {
            if members.any(first, end) && !members.all(first, end) {
                new.push((first, end));
            }
        }
        if new.is_empty() {
            break;
        }
        let mut budget = CLOSURE_MAX_BYTES.saturating_sub(added_pages * page);
        new.retain(|&(s, e)| {
            let bytes = u64::from(e - s) * page;
            let keep = bytes <= budget;
            if keep {
                budget -= bytes;
            }
            keep
        });
        let before: u64 = members.added.0.iter().map(|&(s, e)| u64::from(e - s)).sum();
        members.added.add(&new);
        let after: u64 = members.added.0.iter().map(|&(s, e)| u64::from(e - s)).sum();
        added_pages += after - before;
        if after == before {
            break;
        }
        todo.retain(|&(first, end)| !members.all(first, end));
    }
    added_pages
}
