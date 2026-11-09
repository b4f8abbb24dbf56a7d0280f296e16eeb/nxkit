//! A small job system on `std::thread` (R10).
//!
//! The frame is cut into chunks of passes. Each chunk goes into its own Metal command buffer. [`run_all`] runs one
//! closure for each chunk on up to `threads` threads and returns the results in the order of the chunks. The caller
//! thread takes part, so one thread means no other thread at all. The other threads come from a pool that lives for
//! the whole process and runs at the user-interactive QoS class. Threads made for each call started so late under load
//! that the caller ran most chunks alone, one after the other.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

fn worker<I, T>(
    queue: &Mutex<VecDeque<(usize, I)>>,
    out: &Mutex<Vec<Option<T>>>,
    f: &(impl Fn(I) -> T + Sync),
) {
    loop {
        let next = queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        let Some((i, item)) = next else { return };
        let r = f(item);
        if let Some(slot) = out
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(i)
        {
            *slot = Some(r);
        }
    }
}

/// Runs `f` for each item on up to `threads` threads: the caller and threads of the process's encode pool
/// (`nxkit_metal::thread::Pool`). The results come back in the order of the items.
pub(crate) fn run_all<I: Send, T: Send>(
    threads: usize,
    items: Vec<I>,
    f: impl Fn(I) -> T + Sync,
) -> Vec<T> {
    let n = items.len();
    if threads <= 1 || n <= 1 {
        return items.into_iter().map(f).collect();
    }
    let queue = Mutex::new(items.into_iter().enumerate().collect::<VecDeque<_>>());
    let out: Mutex<Vec<Option<T>>> = Mutex::new((0..n).map(|_| None).collect());
    let task = || worker(&queue, &out, &f);
    nxkit_metal::thread::Pool::global().run(threads.min(n) - 1, &task);
    out.into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .into_iter()
        .flatten()
        .collect()
}

/// Cuts `weights` (one per pass) into at most `parts` runs of neighbouring passes with similar total weight.
/// Returns the end index of each run.
pub(crate) fn split(weights: &[u32], parts: usize) -> Vec<usize> {
    let total: u64 = weights.iter().map(|w| u64::from(*w)).sum();
    let parts = parts.max(1).min(weights.len().max(1));
    let target = total.div_ceil(parts as u64).max(1);
    let mut ends = Vec::with_capacity(parts);
    let mut acc = 0u64;
    for (i, w) in weights.iter().enumerate() {
        acc += u64::from(*w);
        if acc >= target && ends.len() + 1 < parts {
            ends.push(i + 1);
            acc = 0;
        }
    }
    if ends.last().copied() != Some(weights.len()) {
        ends.push(weights.len());
    }
    ends
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_keep_the_order_of_the_items() {
        let out = run_all(4, (0..50u32).collect(), |i| i * 2);
        assert_eq!(out, (0..50u32).map(|i| i * 2).collect::<Vec<_>>());
    }

    #[test]
    fn the_pool_serves_many_calls_with_borrows() {
        for round in 0..200u32 {
            let base = [round; 8];
            let out = run_all(6, (0..8usize).collect(), |i| base[i] + i as u32);
            assert_eq!(out, (0..8u32).map(|i| round + i).collect::<Vec<_>>());
        }
    }

    #[test]
    fn split_covers_all_passes_in_order() {
        let ends = split(&[5, 1, 1, 8, 1, 1, 1, 9], 3);
        assert_eq!(ends.last(), Some(&8));
        assert!(ends.windows(2).all(|w| w[0] < w[1]));
        assert!(ends.len() <= 3);
        assert_eq!(split(&[], 4), vec![0]);
    }
}
