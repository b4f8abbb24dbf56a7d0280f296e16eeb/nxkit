//! The staging buffers of compressed uploads. A compressed level is deswizzled into a buffer and then copied into the
//! texture. A texture needs one buffer for all its levels (each level at its own offset), and the buffers are reused:
//! a buffer is free again when nothing but the pool holds it, which is when the command buffer that retained it has
//! been recycled.

use std::sync::Arc;

/// The alignment of the offset of each level in a staging buffer.
pub(super) const LEVEL_ALIGN: u64 = 256;

/// The smallest staging buffer.
const MIN_BYTES: u64 = 256 * 1024;

/// Up to this size a buffer is rounded to a power of two, above it to whole MiB.
const POW2_UP_TO: u64 = 4 * 1024 * 1024;

/// The most buffers the pool keeps. A request that finds the pool full of busy buffers gets one that the pool does
/// not keep.
const MAX_POOLED: usize = 8;

/// The offset of each level in one staging buffer and the bytes the buffer needs, for levels of `sizes` bytes.
pub(super) fn level_offsets(sizes: &[u64]) -> (Vec<u64>, u64) {
    let mut at = 0;
    let offsets = sizes
        .iter()
        .map(|s| {
            let o = at;
            at += s.next_multiple_of(LEVEL_ALIGN);
            o
        })
        .collect();
    (offsets, at)
}

/// The size of the buffer made for a request of `need` bytes.
pub(super) fn buffer_size(need: u64) -> u64 {
    if need <= POW2_UP_TO {
        need.max(MIN_BYTES).next_power_of_two()
    } else {
        need.next_multiple_of(1024 * 1024)
    }
}

/// A pool of buffers that are handed out as `Arc`s. A buffer is free when the pool holds the only reference.
pub(super) struct Pool<T> {
    buffers: Vec<(Arc<T>, u64)>,
}

impl<T> Pool<T> {
    pub(super) const fn new() -> Self {
        Self {
            buffers: Vec::new(),
        }
    }

    /// A buffer of at least `need` bytes: the smallest free one that fits, or a new one that `make` builds with
    /// [`buffer_size`] bytes. None when `make` fails.
    pub(super) fn acquire(
        &mut self,
        need: u64,
        make: impl FnOnce(u64) -> Option<Arc<T>>,
    ) -> Option<Arc<T>> {
        let best = self
            .buffers
            .iter()
            .filter(|(b, len)| *len >= need && Arc::strong_count(b) == 1)
            .min_by_key(|(_, len)| *len)
            .map(|(b, _)| b.clone());
        if let Some(b) = best {
            return Some(b);
        }
        let size = buffer_size(need);
        let made = make(size)?;
        if self.buffers.len() >= MAX_POOLED {
            self.buffers
                .retain(|(b, len)| *len >= need || Arc::strong_count(b) > 1);
        }
        if self.buffers.len() < MAX_POOLED {
            self.buffers.push((made.clone(), size));
        }
        Some(made)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buffers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(size: u64) -> Option<Arc<u64>> {
        Some(Arc::new(size))
    }

    #[test]
    fn levels_sit_at_aligned_offsets() {
        let (o, total) = level_offsets(&[1000, 300, 16]);
        assert_eq!(o, vec![0, 1024, 1536]);
        assert_eq!(total, 1792);
        assert_eq!(level_offsets(&[]), (vec![], 0));
    }

    #[test]
    fn sizes_round_up() {
        assert_eq!(buffer_size(1), MIN_BYTES);
        assert_eq!(buffer_size(300_000), 512 * 1024);
        assert_eq!(buffer_size(4 * 1024 * 1024), 4 * 1024 * 1024);
        assert_eq!(buffer_size(5 * 1024 * 1024 + 1), 6 * 1024 * 1024);
    }

    #[test]
    fn a_free_buffer_is_reused() {
        let mut p = Pool::new();
        let a = p.acquire(1000, make).unwrap();
        let a_ptr = Arc::as_ptr(&a);
        drop(a);
        let b = p.acquire(2000, make).unwrap();
        assert_eq!(Arc::as_ptr(&b), a_ptr);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn a_busy_buffer_is_not_reused() {
        let mut p = Pool::new();
        let a = p.acquire(1000, make).unwrap();
        let b = p.acquire(1000, make).unwrap();
        assert_ne!(Arc::as_ptr(&a), Arc::as_ptr(&b));
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn the_smallest_fitting_free_buffer_wins() {
        let mut p = Pool::new();
        let big = p.acquire(3 * 1024 * 1024, make).unwrap();
        let small = p.acquire(1000, make).unwrap();
        let small_ptr = Arc::as_ptr(&small);
        drop((big, small));
        assert_eq!(Arc::as_ptr(&p.acquire(1000, make).unwrap()), small_ptr);
    }

    #[test]
    fn a_too_small_buffer_grows_the_pool() {
        let mut p = Pool::new();
        drop(p.acquire(1000, make));
        let big = p.acquire(2 * 1024 * 1024, make).unwrap();
        assert_eq!(*big, 2 * 1024 * 1024);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn a_full_pool_of_busy_buffers_gives_unpooled_ones() {
        let mut p = Pool::new();
        let held: Vec<_> = (0..MAX_POOLED)
            .map(|_| p.acquire(1000, make).unwrap())
            .collect();
        let extra = p.acquire(1000, make).unwrap();
        assert_eq!(p.len(), MAX_POOLED);
        assert_eq!(Arc::strong_count(&extra), 1);
        drop(held);
    }

    #[test]
    fn a_failed_make_gives_none() {
        let mut p: Pool<u64> = Pool::new();
        assert!(p.acquire(10, |_| None).is_none());
        assert_eq!(p.len(), 0);
    }
}
