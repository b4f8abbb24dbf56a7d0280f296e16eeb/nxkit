//! The process's handle table. A handle is `index | generation << 15`, so a closed handle is not valid again when
//! its slot is reused.

use crate::result::{KResult, KResultOr};
use crate::thread::{SyncObject, Thread, lock};
use nxkit_abi::kernel::Session;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The pseudo handle of the calling thread.
pub const CURRENT_THREAD: u32 = 0xFFFF_8000;
/// The pseudo handle of the calling process.
pub const CURRENT_PROCESS: u32 = 0xFFFF_8001;

const INDEX_BITS: u32 = 15;
const INDEX_MASK: u32 = (1 << INDEX_BITS) - 1;
const GEN_MASK: u32 = 0x7FFF;

/// An event. `CreateEvent` makes a write end and a read end of the same object.
pub(crate) struct Event {
    pub(crate) sync: SyncObject,
}

/// Shared memory that a service made. Its pages are in the kernel's backing area.
pub(crate) struct SharedMemory {
    pub(crate) base: u64,
    pub(crate) size: u64,
}

/// Memory of the game that is lent to a service. Its pages stay in the guest's heap and are locked.
pub(crate) struct TransferMemory {
    pub(crate) addr: u64,
    pub(crate) size: u64,
}

/// What a handle refers to.
#[derive(Clone)]
pub(crate) enum KObject {
    Thread(Arc<Thread>),
    EventWrite(Arc<Event>),
    EventRead(Arc<Event>),
    Session(Arc<dyn Session>),
    SharedMemory(Arc<SharedMemory>),
    TransferMemory(Arc<TransferMemory>),
    ResourceLimit,
}

struct Slot {
    generation: u32,
    object: Option<KObject>,
}

#[derive(Default)]
struct Shard {
    slots: Vec<Slot>,
    free: Vec<usize>,
}

const SHARD_BITS: u32 = 4;
const SHARDS: usize = 1 << SHARD_BITS;

/// The table. The index of a handle holds its shard in the low bits, so a lookup locks one shard and the
/// threads of a title rarely share one. A lock is held only for one lookup, insert or removal.
pub(crate) struct HandleTable {
    shards: [Mutex<Shard>; SHARDS],
    next: AtomicUsize,
    used: AtomicUsize,
    capacity: usize,
}

impl HandleTable {
    pub(crate) fn new(capacity: usize) -> HandleTable {
        HandleTable {
            shards: std::array::from_fn(|_| Mutex::new(Shard::default())),
            next: AtomicUsize::new(0),
            used: AtomicUsize::new(0),
            capacity: capacity.min(INDEX_MASK as usize + 1),
        }
    }

    fn split(handle: u32) -> Option<(usize, usize, u32)> {
        if handle >= CURRENT_THREAD {
            return None;
        }
        let index = (handle & INDEX_MASK) as usize;
        Some((index % SHARDS, index / SHARDS, handle >> INDEX_BITS))
    }

    pub(crate) fn insert(&self, object: KObject) -> KResultOr<u32> {
        if self.used.fetch_add(1, Ordering::AcqRel) >= self.capacity {
            self.used.fetch_sub(1, Ordering::AcqRel);
            return Err(KResult::OUT_OF_HANDLES);
        }
        let first = self.next.fetch_add(1, Ordering::Relaxed);
        for n in 0..SHARDS {
            let shard_no = (first + n) % SHARDS;
            let mut shard = lock(&self.shards[shard_no]);
            let local = match shard.free.pop() {
                Some(i) => i,
                None if (shard.slots.len() * SHARDS + shard_no) <= INDEX_MASK as usize => {
                    shard.slots.push(Slot {
                        generation: 0,
                        object: None,
                    });
                    shard.slots.len() - 1
                }
                None => continue,
            };
            let slot = &mut shard.slots[local];
            slot.generation = slot.generation % GEN_MASK + 1;
            slot.object = Some(object);
            let generation = slot.generation;
            return Ok((local * SHARDS + shard_no) as u32 | generation << INDEX_BITS);
        }
        self.used.fetch_sub(1, Ordering::AcqRel);
        Err(KResult::OUT_OF_HANDLES)
    }

    pub(crate) fn get(&self, handle: u32) -> Option<KObject> {
        let (shard, local, generation) = Self::split(handle)?;
        let shard = lock(&self.shards[shard]);
        let slot = shard.slots.get(local)?;
        if slot.generation != generation {
            return None;
        }
        slot.object.clone()
    }

    /// Removes the handle and gives back the object it named.
    pub(crate) fn remove(&self, handle: u32) -> KResultOr<KObject> {
        let (shard_no, local, generation) = Self::split(handle).ok_or(KResult::INVALID_HANDLE)?;
        let mut shard = lock(&self.shards[shard_no]);
        let object = match shard.slots.get_mut(local) {
            Some(s) if s.generation == generation => s.object.take(),
            _ => None,
        };
        let object = object.ok_or(KResult::INVALID_HANDLE)?;
        shard.free.push(local);
        self.used.fetch_sub(1, Ordering::AcqRel);
        Ok(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> KObject {
        KObject::EventRead(Arc::new(Event {
            sync: SyncObject::default(),
        }))
    }

    #[test]
    fn a_reused_slot_does_not_validate_the_old_handle() {
        let t = HandleTable::new(4);
        let a = t.insert(event()).unwrap_or(0);
        assert!(t.get(a).is_some());
        assert!(t.remove(a).is_ok());
        assert_eq!(t.remove(a).err(), Some(KResult::INVALID_HANDLE));
        let b = t.insert(event()).unwrap_or(0);
        assert_ne!(a, b);
        assert!(t.get(a).is_none());
        assert!(t.get(b).is_some());
        assert!(t.get(CURRENT_THREAD).is_none());
    }

    #[test]
    fn a_full_table_gives_out_of_handles() {
        let t = HandleTable::new(2);
        assert!(t.insert(event()).is_ok());
        assert!(t.insert(event()).is_ok());
        assert_eq!(t.insert(event()).err(), Some(KResult::OUT_OF_HANDLES));
    }
}
