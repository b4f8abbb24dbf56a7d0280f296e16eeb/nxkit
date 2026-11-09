//! The pool line of the memory report: `nxkit-mem pools`. It says how much guest memory the GPU must reach if there
//! were one no-copy buffer for each memory pool of the game and not one over the whole range. The record is
//! `nxkit_nvn::pools`. All sizes are MiB, and the arithmetic is in pure functions.

use nxkit_nvn::backend::PoolInfo;
use nxkit_nvn::pools::{FLAG_GPU_NO_ACCESS, PoolSnapshot};

use crate::layout::{Layout, PAGE};

const MIB: u64 = 1024 * 1024;
const KIB: u64 = 1024;

/// The upper bounds of the size classes, in bytes. A pool above the last bound is in the class after it.
const CLASSES: [u64; 4] = [64 * KIB, MIB, 16 * MIB, 256 * MIB];

/// A pool rounded out to pages: the start down, the end up.
fn rounded(p: &PoolInfo, page: u64) -> (u64, u64) {
    let end = p.host.saturating_add(p.len);
    (
        p.host / page * page,
        end.div_ceil(page).saturating_mul(page),
    )
}

/// The ranges merged: sorted, and overlapping or touching ranges made one.
fn union(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.retain(|r| r.0 < r.1);
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (s, e) in ranges {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// The pools rounded out to pages, merged.
fn page_union(pools: &[PoolInfo], page: u64) -> Vec<(u64, u64)> {
    union(pools.iter().map(|p| rounded(p, page)).collect())
}

fn total(ranges: &[(u64, u64)]) -> u64 {
    ranges.iter().map(|r| r.1 - r.0).sum()
}

/// The bytes of merged ranges that lie in `[start, end)`.
fn within(ranges: &[(u64, u64)], start: u64, end: u64) -> u64 {
    ranges
        .iter()
        .map(|r| r.1.min(end).saturating_sub(r.0.max(start)))
        .sum()
}

/// The pools whose start or end is not a multiple of `page`.
fn unaligned(pools: &[PoolInfo], page: u64) -> usize {
    pools
        .iter()
        .filter(|p| p.host % page != 0 || p.host.saturating_add(p.len) % page != 0)
        .count()
}

/// The index of the size class of a pool of `len` bytes: 0 up to 64 KiB, then 1 MiB, 16 MiB, 256 MiB, larger.
fn class(len: u64) -> usize {
    CLASSES
        .iter()
        .position(|&b| len <= b)
        .unwrap_or(CLASSES.len())
}

fn classes(pools: &[PoolInfo]) -> [usize; 5] {
    let mut n = [0; 5];
    for p in pools {
        n[class(p.len)] += 1;
    }
    n
}

/// The bytes of the pools without and with `GPU_NO_ACCESS`.
fn gpu_split(pools: &[PoolInfo]) -> (u64, u64) {
    pools.iter().fold((0, 0), |(gpu, cpu), p| {
        if p.flags & FLAG_GPU_NO_ACCESS == 0 {
            (gpu + p.len, cpu)
        } else {
            (gpu, cpu + p.len)
        }
    })
}

/// The pool line for a snapshot. `l` splits the union by region.
pub fn pool_line(l: &Layout, s: &PoolSnapshot) -> String {
    line_with(l, s, PAGE)
}

fn line_with(l: &Layout, s: &PoolSnapshot, page: u64) -> String {
    let u = page_union(&s.pools, page);
    let (heap, alias) = (
        within(&u, l.heap.start, l.heap.end()),
        within(&u, l.alias.start, l.alias.end()),
    );
    let all = total(&u);
    let (gpu, cpu) = gpu_split(&s.pools);
    let n = classes(&s.pools);
    format!(
        "nxkit-mem pools (MiB): n {} made {} gone {} bytes {} pages {} unaligned {} largest {} gpu {} cpu-only {} | \
         heap {} alias {} other {} | sizes <=64K {} <=1M {} <=16M {} <=256M {} larger {}",
        s.pools.len(),
        s.made,
        s.gone,
        s.pools.iter().map(|p| p.len).sum::<u64>() / MIB,
        all / MIB,
        unaligned(&s.pools, page),
        s.pools.iter().map(|p| p.len).max().unwrap_or(0) / MIB,
        gpu / MIB,
        cpu / MIB,
        heap / MIB,
        alias / MIB,
        all.saturating_sub(heap).saturating_sub(alias) / MIB,
        n[0],
        n[1],
        n[2],
        n[3],
        n[4],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Range;

    const P: u64 = 0x4000;

    fn pool(host: u64, len: u64, flags: u32) -> PoolInfo {
        PoolInfo { host, len, flags }
    }

    #[test]
    fn a_pool_rounds_out_to_pages() {
        assert_eq!(rounded(&pool(0x4010, 0x20, 0), P), (0x4000, 0x8000));
        assert_eq!(rounded(&pool(0x4000, 0x4000, 0), P), (0x4000, 0x8000));
        assert_eq!(rounded(&pool(0x7fff, 2, 0), P), (0x4000, 0xc000));
    }

    #[test]
    fn overlapping_and_touching_ranges_count_once() {
        let u = union(vec![
            (0x8000, 0xc000),
            (0, 0x4000),
            (0x4000, 0x6000),
            (0xa000, 0x10000),
        ]);
        assert_eq!(u, vec![(0, 0x6000), (0x8000, 0x10000)]);
        assert_eq!(total(&u), 0xe000);
        assert!(union(vec![(5, 5)]).is_empty());
    }

    #[test]
    fn pools_in_one_page_share_it() {
        let pools = [pool(0x4000, 0x100, 0), pool(0x4200, 0x100, 0)];
        assert_eq!(total(&page_union(&pools, P)), P);
    }

    #[test]
    fn the_union_splits_by_region() {
        let u = vec![(0x1000, 0x3000), (0x5000, 0x9000)];
        assert_eq!(within(&u, 0x2000, 0x6000), 0x2000);
        assert_eq!(within(&u, 0x3000, 0x5000), 0);
        assert_eq!(within(&u, 0, 0x10000), 0x6000);
    }

    #[test]
    fn unaligned_counts_start_and_end() {
        let pools = [
            pool(0x4000, 0x4000, 0),
            pool(0x4001, 0x3fff, 0),
            pool(0x4000, 0x100, 0),
        ];
        assert_eq!(unaligned(&pools, P), 2);
    }

    #[test]
    fn size_classes_include_their_upper_bound() {
        assert_eq!(class(1), 0);
        assert_eq!(class(64 * KIB), 0);
        assert_eq!(class(64 * KIB + 1), 1);
        assert_eq!(class(MIB), 1);
        assert_eq!(class(16 * MIB), 2);
        assert_eq!(class(256 * MIB), 3);
        assert_eq!(class(256 * MIB + 1), 4);
        let pools = [
            pool(0, 10, 0),
            pool(0, MIB, 0),
            pool(0, MIB, 0),
            pool(0, 300 * MIB, 0),
        ];
        assert_eq!(classes(&pools), [1, 2, 0, 0, 1]);
    }

    #[test]
    fn the_gpu_flag_splits_the_bytes() {
        let pools = [pool(0, 100, 0x1), pool(0, 50, FLAG_GPU_NO_ACCESS | 0x1)];
        assert_eq!(gpu_split(&pools), (100, 50));
    }

    #[test]
    fn the_line_names_every_part() {
        let r = |start: u64, len: u64| Range { start, len };
        let l = Layout {
            all: r(0, 0x5000_0000),
            modules: r(0, 0x1000_0000),
            main_stack: r(0x3000_0000, 0x10_0000),
            tls: r(0x1000_0000, 0x40_0000),
            shared: r(0x1040_0000, 0x400_0000),
            heap: r(0x1100_0000, 0x1f00_0000),
            stack: r(0x3000_0000, 0x1000_0000),
            alias: r(0x4000_0000, 0x1000_0000),
        };
        let s = PoolSnapshot {
            pools: vec![
                pool(0x1100_0000, 0x100_0000, 0),
                pool(0x4000_0000, 0x200_0000, FLAG_GPU_NO_ACCESS),
                pool(0x1000_0000, 0x4000, 0),
            ],
            made: 5,
            gone: 2,
        };
        let line = pool_line(&l, &s);
        for part in [
            "nxkit-mem pools",
            "n 3 made 5 gone 2",
            "bytes 48",
            "pages 48",
            "largest 32",
            "gpu 16",
            "cpu-only 32",
            "heap 16 alias 32 other 0",
            "<=64K 1 <=1M 0 <=16M 1 <=256M 1 larger 0",
        ] {
            assert!(line.contains(part), "{part} missing in {line}");
        }
    }
}
