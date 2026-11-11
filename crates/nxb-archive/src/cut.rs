//! Batches cut by content. Bases are grouped by their vertex function (its library's bytes and entry name), the
//! groups ordered by their hash and cut into hash ranges ([`cut`]). Inserting or removing one base changes the batch
//! of its range only, so the store keeps every other batch.
//!
//! Grouping by vertex function matters for cost: `applegpu-nt` compiles each function once per run, so a batch of
//! bases that share vertex functions costs less (the reference title, 1000 bases: 291 vertex functions in pair order, 639 when
//! drawn at random, 10 % more CPU at random).

/// One base: its content hash, its vertex function's hash and its index in the script's render or compute list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Base {
    /// SHA-256 of what identifies the base (libraries' bytes, entries, descriptor).
    pub hash: [u8; 32],
    /// SHA-256 of its vertex function (library bytes, entry).
    pub group: [u8; 32],
    /// Index in the script's pipeline list.
    pub index: usize,
    /// A compute pipeline: `index` is in the compute list.
    pub compute: bool,
}

fn u64_of(h: &[u8; 32]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&h[..8]);
    u64::from_be_bytes(a)
}

/// Cuts `bases` into batches: the vertex-function hash space is split into a power of two of equal ranges (about
/// `bases / target` of them), one batch per non-empty range, a batch over `2 * target` split further. An insertion
/// changes its range's batch only, until the base count crosses a power of two (then every batch changes once).
pub fn cut(bases: Vec<Base>, target: usize) -> Vec<Vec<Base>> {
    let target = target.max(1);
    let ranges = (bases.len() / target).max(1).next_power_of_two();
    cut_ranges(bases, ranges, 2 * target)
}

/// The batches for `slots` parallel compiler runs: about four batches per slot, so the last wave is short (one batch
/// per slot left the last few batches running alone). The range count is `next_power_of_two(4 * slots)`
/// (more when `max_target` asks for it), independent of the base count: an inserted base changes only the batch of its
/// range. Batches over twice the average are split, so sizes stay within about 2x of each other.
pub fn cut_for_slots(bases: Vec<Base>, slots: usize, max_target: usize) -> Vec<Vec<Base>> {
    let n = bases.len();
    let ranges = (4 * slots.max(1))
        .max(n / max_target.max(1))
        .max(1)
        .next_power_of_two();
    let cap = (2 * n.div_ceil(ranges)).max(64);
    cut_ranges(bases, ranges, cap)
}

/// All of `bases` in one batch, in the order [`cut`] sorts them. The exact pipelines of a stream group are compiled
/// this way: the batch holds the group's states and nothing else, so it changes when one of them does.
pub fn whole(mut bases: Vec<Base>) -> Vec<Vec<Base>> {
    if bases.is_empty() {
        return Vec::new();
    }
    bases.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then(a.hash.cmp(&b.hash))
            .then(a.index.cmp(&b.index))
    });
    vec![bases]
}

fn cut_ranges(mut bases: Vec<Base>, ranges: usize, cap: usize) -> Vec<Vec<Base>> {
    bases.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then(a.hash.cmp(&b.hash))
            .then(a.index.cmp(&b.index))
    });
    let ranges = ranges.max(1) as u64;
    let shift = 64 - ranges.trailing_zeros();
    let range_of = |b: &Base| {
        if ranges == 1 {
            0
        } else {
            u64_of(&b.group) >> shift
        }
    };
    let mut out: Vec<Vec<Base>> = Vec::new();
    let mut cur: Vec<Base> = Vec::new();
    for b in bases {
        let boundary = cur.last().is_some_and(|l| range_of(l) != range_of(&b));
        if !cur.is_empty() && (boundary || cur.len() >= cap) {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(b);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bases(n: usize, salt: u8) -> Vec<Base> {
        (0..n)
            .map(|i| {
                let mut v = (i as u64).to_le_bytes().to_vec();
                v.push(salt);
                let g = nxb_model::sha256(&((i / 4) as u64).to_le_bytes()).0;
                Base {
                    hash: nxb_model::sha256(&v).0,
                    group: g,
                    index: i,
                    compute: false,
                }
            })
            .collect()
    }

    #[test]
    fn insertion_touches_at_most_three_batches() {
        let a = bases(20_000, 0);
        let before = cut(a.clone(), 1000);
        assert!(before.iter().all(|b| b.len() <= 2000));
        assert!(
            before.len() >= 16 && before.len() <= 40,
            "{} batches",
            before.len()
        );
        let total: usize = before.iter().map(Vec::len).sum();
        assert_eq!(total, 20_000);
        let mut b = a;
        b.extend(bases(1, 9).into_iter().map(|x| Base { index: 99_999, ..x }));
        let after = cut(b, 1000);
        let key = |v: &Vec<Base>| v.iter().map(|x| x.hash).collect::<Vec<_>>();
        let old: std::collections::BTreeSet<_> = before.iter().map(key).collect();
        let changed = after.iter().filter(|v| !old.contains(&key(v))).count();
        assert!(changed <= 3, "{changed} batches changed");
    }

    #[test]
    fn whole_is_one_sorted_batch_whatever_its_size() {
        let b = bases(5_000, 3);
        let one = whole(b.clone());
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].len(), 5_000);
        // the order does not depend on the order of the input
        let mut r = b;
        r.reverse();
        assert_eq!(whole(r), one);
        assert!(whole(Vec::new()).is_empty());
    }

    #[test]
    fn groups_stay_together() {
        let batches = cut(bases(8_000, 1), 500);
        let mut seen = std::collections::HashMap::new();
        for (i, b) in batches.iter().enumerate() {
            for x in b {
                seen.entry(x.group)
                    .or_insert_with(std::collections::BTreeSet::new)
                    .insert(i);
            }
        }
        let split = seen.values().filter(|s| s.len() > 1).count();
        assert!(
            split * 20 < seen.len(),
            "{split} of {} groups split",
            seen.len()
        );
    }

    /// Bases in groups of uneven size (1 to 40 bases per vertex function), as in a real game.
    fn uneven(n: usize, salt: u8) -> Vec<Base> {
        let mut out = Vec::new();
        let mut g = 0u64;
        while out.len() < n {
            let size = 1 + (nxb_model::sha256(&g.to_le_bytes()).0[0] as usize % 40);
            let group = nxb_model::sha256(&g.to_be_bytes()).0;
            for _ in 0..size.min(n - out.len()) {
                let mut v = (out.len() as u64).to_le_bytes().to_vec();
                v.push(salt);
                out.push(Base {
                    hash: nxb_model::sha256(&v).0,
                    group,
                    index: out.len(),
                    compute: false,
                });
            }
            g += 1;
        }
        out
    }

    #[test]
    fn about_four_batches_per_slot_balanced() {
        let b = cut_for_slots(uneven(18_300, 0), 12, 2000);
        let total: usize = b.iter().map(Vec::len).sum();
        assert_eq!(total, 18_300);
        assert!(b.len() >= 48 && b.len() <= 96, "{} batches", b.len());
        let avg = 18_300 / b.len();
        let max = b.iter().map(Vec::len).max().unwrap_or(0);
        assert!(max <= 3 * avg, "largest {max}, average {avg}");
    }

    #[test]
    fn slots_cut_stable_under_insertion_and_removal() {
        let a = uneven(18_300, 0);
        let before = cut_for_slots(a.clone(), 12, 2000);
        let key = |v: &Vec<Base>| v.iter().map(|x| x.hash).collect::<Vec<_>>();
        let old: std::collections::BTreeSet<_> = before.iter().map(key).collect();
        let mut ins = a.clone();
        ins.push(Base {
            index: 99_999,
            ..uneven(1, 7)[0]
        });
        let changed = cut_for_slots(ins, 12, 2000)
            .iter()
            .filter(|v| !old.contains(&key(v)))
            .count();
        assert!(changed <= 3, "{changed} batches changed by an insertion");
        let mut del = a;
        del.remove(5_000);
        let changed = cut_for_slots(del, 12, 2000)
            .iter()
            .filter(|v| !old.contains(&key(v)))
            .count();
        assert!(changed <= 3, "{changed} batches changed by a removal");
    }
}
