//! A table of open objects by handle, without one lock for all of them.
//!
//! The table has 16 shards. A handle names its shard, its slot and a generation, so a handle of a closed object never
//! reaches the object that took its slot. The lookup takes the lock of one shard for the time of a clone of an `Arc`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

const SHARDS: usize = 16;
const GEN_BITS: u32 = 20;

/// The guard of a mutex. A thread that panicked while it held the lock left nothing half-written, so the data is
/// used as it is.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Slot<T> {
    generation: u32,
    item: Option<Arc<T>>,
}

struct Shard<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

/// The open objects of one kind.
pub(crate) struct Table<T> {
    shards: Vec<Mutex<Shard<T>>>,
    next: AtomicUsize,
}

impl<T> Table<T> {
    /// An empty table.
    pub(crate) fn new() -> Table<T> {
        Table {
            shards: (0..SHARDS).map(|_| Mutex::new(Shard { slots: Vec::new(), free: Vec::new() })).collect(),
            next: AtomicUsize::new(0),
        }
    }

    /// Stores `item` and returns its handle. A handle is never zero and its low 4 bits are clear, as a pointer to a
    /// heap object of the game's SDK is.
    pub(crate) fn insert(&self, item: Arc<T>) -> u64 {
        let shard = self.next.fetch_add(1, Ordering::Relaxed) % SHARDS;
        let mut s = lock(&self.shards[shard]);
        let (index, generation) = match s.free.pop() {
            Some(i) => {
                let slot = &mut s.slots[i as usize];
                slot.item = Some(item);
                (i, slot.generation)
            }
            None => {
                s.slots.push(Slot { generation: 1, item: Some(item) });
                ((s.slots.len() - 1) as u32, 1)
            }
        };
        let raw = (u64::from(generation) << 28) | (u64::from(index) << 4) | shard as u64;
        raw << 4
    }

    fn locate(handle: u64) -> Option<(usize, usize, u32)> {
        if handle == 0 || handle & 0xF != 0 {
            return None;
        }
        let raw = handle >> 4;
        let shard = (raw & 0xF) as usize;
        let index = ((raw >> 4) & 0xFF_FFFF) as usize;
        let generation = (raw >> 28) as u32;
        (generation != 0 && generation < (1 << GEN_BITS)).then_some((shard, index, generation))
    }

    /// The object of `handle`, if it is open.
    pub(crate) fn get(&self, handle: u64) -> Option<Arc<T>> {
        let (shard, index, generation) = Self::locate(handle)?;
        let s = lock(&self.shards[shard]);
        let slot = s.slots.get(index)?;
        if slot.generation != generation {
            return None;
        }
        slot.item.clone()
    }

    /// Takes the object of `handle` out of the table.
    pub(crate) fn remove(&self, handle: u64) -> Option<Arc<T>> {
        let (shard, index, generation) = Self::locate(handle)?;
        let mut s = lock(&self.shards[shard]);
        let slot = s.slots.get_mut(index)?;
        if slot.generation != generation {
            return None;
        }
        let item = slot.item.take()?;
        slot.generation = if slot.generation + 1 >= (1 << GEN_BITS) { 1 } else { slot.generation + 1 };
        s.free.push(index as u32);
        Some(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_finds_its_object_until_it_is_removed() {
        let t = Table::new();
        let h = t.insert(Arc::new(7u32));
        assert_ne!(h, 0);
        assert_eq!(h & 0xF, 0);
        assert_eq!(t.get(h).as_deref(), Some(&7));
        assert_eq!(t.remove(h).as_deref(), Some(&7));
        assert!(t.get(h).is_none());
        assert!(t.remove(h).is_none());
    }

    #[test]
    fn a_reused_slot_does_not_answer_to_the_old_handle() {
        let t = Table::new();
        let handles: Vec<u64> = (0..SHARDS as u32).map(|i| t.insert(Arc::new(i))).collect();
        let old = handles[0];
        assert!(t.remove(old).is_some());
        let fresh: Vec<u64> = (0..SHARDS as u32).map(|i| t.insert(Arc::new(100 + i))).collect();
        assert!(t.get(old).is_none());
        assert!(fresh.iter().all(|h| t.get(*h).is_some()));
    }

    #[test]
    fn handles_that_were_never_given_are_refused() {
        let t: Table<u32> = Table::new();
        assert!(t.get(0).is_none());
        assert!(t.get(0x1234_5678).is_none());
        assert!(t.get(16).is_none());
    }
}
