//! The process's handle table. A handle is `index | generation << 15`, so a closed handle is not valid again when
//! its slot is reused.

use crate::result::{KResult, KResultOr};
use crate::thread::{SyncObject, Thread, lock};
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

/// What a handle refers to.
#[derive(Clone)]
pub(crate) enum KObject {
    Thread(Arc<Thread>),
    EventWrite(Arc<Event>),
    EventRead(Arc<Event>),
}

struct Slot {
    generation: u32,
    object: Option<KObject>,
}

/// The table. Its lock is held only for one lookup, insert or removal.
pub(crate) struct HandleTable {
    slots: Mutex<Vec<Slot>>,
    capacity: usize,
}

impl HandleTable {
    pub(crate) fn new(capacity: usize) -> HandleTable {
        HandleTable {
            slots: Mutex::new(Vec::new()),
            capacity: capacity.min(INDEX_MASK as usize),
        }
    }

    pub(crate) fn insert(&self, object: KObject) -> KResultOr<u32> {
        let mut slots = lock(&self.slots);
        let index = match slots.iter().position(|s| s.object.is_none()) {
            Some(i) => i,
            None if slots.len() < self.capacity => {
                slots.push(Slot {
                    generation: 0,
                    object: None,
                });
                slots.len() - 1
            }
            None => return Err(KResult::OUT_OF_HANDLES),
        };
        let slot = &mut slots[index];
        slot.generation = slot.generation % GEN_MASK + 1;
        slot.object = Some(object);
        Ok(index as u32 | slot.generation << INDEX_BITS)
    }

    pub(crate) fn get(&self, handle: u32) -> Option<KObject> {
        let slots = lock(&self.slots);
        let slot = slots.get((handle & INDEX_MASK) as usize)?;
        let generation = handle >> INDEX_BITS;
        if handle >= CURRENT_THREAD || slot.generation != generation {
            return None;
        }
        slot.object.clone()
    }

    pub(crate) fn remove(&self, handle: u32) -> KResultOr<()> {
        let mut slots = lock(&self.slots);
        let generation = handle >> INDEX_BITS;
        match slots.get_mut((handle & INDEX_MASK) as usize) {
            Some(s)
                if handle < CURRENT_THREAD && s.generation == generation && s.object.is_some() =>
            {
                s.object = None;
                Ok(())
            }
            _ => Err(KResult::INVALID_HANDLE),
        }
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
        assert_eq!(t.remove(a), Ok(()));
        assert_eq!(t.remove(a), Err(KResult::INVALID_HANDLE));
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
