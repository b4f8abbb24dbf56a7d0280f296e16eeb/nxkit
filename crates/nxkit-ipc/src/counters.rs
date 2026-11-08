//! The count of every command per interface and command ID (R16).
//!
//! A command takes the read lock of the map to find its counter and then adds one with an atomic. The write lock is
//! taken once for each interface and command ID the first time it is seen. No lock is held while a service handles a
//! command.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

/// How often one command of one interface was called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Count {
    /// The name of the interface, for example `IFileSystem`.
    pub interface: &'static str,
    /// The CMIF command ID.
    pub command: u32,
    /// The number of calls.
    pub count: u64,
}

/// The counters of one server.
#[derive(Default)]
pub(crate) struct Counters {
    map: RwLock<HashMap<(&'static str, u32), Arc<AtomicU64>>>,
}

impl Counters {
    /// Counts one call of `command` of `interface`.
    pub(crate) fn hit(&self, interface: &'static str, command: u32) {
        let key = (interface, command);
        {
            let map = self.map.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(c) = map.get(&key) {
                c.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        let mut map = self.map.write().unwrap_or_else(PoisonError::into_inner);
        map.entry(key).or_default().fetch_add(1, Ordering::Relaxed);
    }

    /// Every counter, sorted by interface and command ID.
    pub(crate) fn snapshot(&self) -> Vec<Count> {
        let map = self.map.read().unwrap_or_else(PoisonError::into_inner);
        let mut all: Vec<Count> = map
            .iter()
            .map(|(&(interface, command), c)| Count {
                interface,
                command,
                count: c.load(Ordering::Relaxed),
            })
            .collect();
        all.sort_by(|a, b| (a.interface, a.command).cmp(&(b.interface, b.command)));
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_per_interface_and_command() {
        let c = Counters::default();
        c.hit("IFile", 0);
        c.hit("IFile", 0);
        c.hit("IFile", 1);
        c.hit("IFileSystem", 0);
        let s = c.snapshot();
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].interface, s[0].command, s[0].count), ("IFile", 0, 2));
        assert_eq!((s[1].interface, s[1].command, s[1].count), ("IFile", 1, 1));
        assert_eq!(
            (s[2].interface, s[2].command, s[2].count),
            ("IFileSystem", 0, 1)
        );
    }
}
