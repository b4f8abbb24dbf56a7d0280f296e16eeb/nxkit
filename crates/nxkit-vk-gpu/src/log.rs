//! Messages that are printed once per kind, as the reference layer's `RED1`: a draw loop cannot flood the log.

use std::collections::HashSet;
use std::fmt::Display;
use std::sync::{Mutex, PoisonError};

/// The most kinds that are remembered. Later kinds are printed with a note that the log is full, once.
const KINDS: usize = 4096;

/// The kinds that were printed. One per backend: no process-wide state.
#[derive(Default)]
pub(crate) struct OnceLog {
    seen: Mutex<HashSet<String>>,
}

impl OnceLog {
    /// Prints `[vk] <msg>` the first time `kind` comes. Gives whether it printed.
    pub(crate) fn once(&self, kind: impl Into<String>, msg: impl Display) -> bool {
        let kind = kind.into();
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        if seen.contains(&kind) {
            return false;
        }
        if seen.len() >= KINDS {
            if seen.insert(String::from("\0full")) {
                eprintln!("[vk] (more messages of new kinds are not printed)");
            }
            return false;
        }
        seen.insert(kind);
        eprintln!("[vk] {msg}");
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_prints_once() {
        let log = OnceLog::default();
        assert!(log.once("a", "first"));
        assert!(!log.once("a", "again"));
        assert!(log.once("b", "other"));
    }
}
