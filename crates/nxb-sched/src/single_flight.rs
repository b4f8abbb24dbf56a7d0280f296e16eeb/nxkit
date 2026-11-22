//! Single-flight by content key: equal work runs once per build (design-v2 8.4, decisions.md W11).
//!
//! Two ways to join a key:
//! - [`SingleFlight::claim`] (preferred inside the executor): the first caller becomes the leader and gets a
//!   [`Leader`] to complete. Later callers register a continuation and return at once, so no worker blocks.
//! - [`SingleFlight::get_or_run`]: blocking convenience for code outside the executor or for cheap work.
//!
//! Results are memoised for the life of the `SingleFlight`, keep `V` small (a digest or an `Arc`).
//! If a leader is dropped without completing (its task panicked), continuations get `None` and blocked callers
//! retry as the new leader.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

type Waiter<V> = Box<dyn FnOnce(Option<&V>) + Send>;

enum Slot<V> {
    Running(Vec<Waiter<V>>),
    Done(Arc<V>),
}

/// Single-flight table keyed by `K` (normally a `ContentKey`).
pub struct SingleFlight<K, V> {
    map: Mutex<BTreeMap<K, Slot<V>>>,
    cv: Condvar,
}

impl<K, V> std::fmt::Debug for SingleFlight<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingleFlight").finish_non_exhaustive()
    }
}

impl<K: Ord + Clone, V> Default for SingleFlight<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of [`SingleFlight::claim`].
pub enum Claim<'a, K: Ord + Clone, V> {
    /// You run the work and call [`Leader::complete`].
    Leader(Leader<'a, K, V>),
    /// Someone else runs (or ran) it, your continuation was called or will be.
    Joined,
}

impl<K: Ord + Clone, V> std::fmt::Debug for Claim<'_, K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Claim::Leader(_) => "Leader",
            Claim::Joined => "Joined",
        })
    }
}

/// The right and duty to complete a key.
pub struct Leader<'a, K: Ord + Clone, V> {
    sf: &'a SingleFlight<K, V>,
    key: Option<K>,
}

impl<K: Ord + Clone, V> SingleFlight<K, V> {
    /// An empty table.
    pub fn new() -> Self {
        SingleFlight {
            map: Mutex::new(BTreeMap::new()),
            cv: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<K, Slot<V>>> {
        self.map.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Join `key`. `on_ready` is called exactly once with the result (`None` if the leader abandoned the key), unless
    /// you become the leader: then `on_ready` is not stored and you get the [`Leader`].
    pub fn claim(
        &self,
        key: K,
        on_ready: impl FnOnce(Option<&V>) + Send + 'static,
    ) -> Claim<'_, K, V> {
        let mut map = self.lock();
        match map.get_mut(&key) {
            None => {
                map.insert(key.clone(), Slot::Running(Vec::new()));
                Claim::Leader(Leader {
                    sf: self,
                    key: Some(key),
                })
            }
            Some(Slot::Running(waiters)) => {
                waiters.push(Box::new(on_ready));
                Claim::Joined
            }
            Some(Slot::Done(v)) => {
                let v = v.clone();
                drop(map);
                on_ready(Some(&v));
                Claim::Joined
            }
        }
    }

    /// The finished result for `key`, if any.
    pub fn get(&self, key: &K) -> Option<Arc<V>> {
        match self.lock().get(key) {
            Some(Slot::Done(v)) => Some(v.clone()),
            _ => None,
        }
    }

    /// Blocking: return the result for `key`, running `f` if nobody has. Concurrent callers wait for the leader.
    pub fn get_or_run(&self, key: K, f: impl FnOnce() -> V) -> Arc<V> {
        let mut map = self.lock();
        loop {
            match map.get(&key) {
                Some(Slot::Done(v)) => return v.clone(),
                Some(Slot::Running(_)) => {
                    map = self.cv.wait(map).unwrap_or_else(PoisonError::into_inner);
                }
                None => {
                    map.insert(key.clone(), Slot::Running(Vec::new()));
                    drop(map);
                    let leader = Leader {
                        sf: self,
                        key: Some(key),
                    };
                    return leader.complete(f());
                }
            }
        }
    }

    /// Forget a finished key (frees memory, the next claim runs the work again).
    pub fn forget(&self, key: &K) {
        let mut map = self.lock();
        if matches!(map.get(key), Some(Slot::Done(_))) {
            map.remove(key);
        }
    }
}

impl<K: Ord + Clone, V> Leader<'_, K, V> {
    /// Publish the result: it is memoised, blocked callers wake, continuations run on this thread.
    pub fn complete(mut self, v: V) -> Arc<V> {
        let v = Arc::new(v);
        let waiters = match self.key.take() {
            Some(key) => {
                let mut map = self.sf.lock();
                let old = map.insert(key, Slot::Done(v.clone()));
                drop(map);
                self.sf.cv.notify_all();
                match old {
                    Some(Slot::Running(w)) => w,
                    _ => Vec::new(),
                }
            }
            None => Vec::new(),
        };
        for w in waiters {
            w(Some(&v));
        }
        v
    }
}

impl<K: Ord + Clone, V> Drop for Leader<'_, K, V> {
    fn drop(&mut self) {
        // Not completed (the leader's task panicked or returned early): release the key.
        if let Some(key) = self.key.take() {
            let mut map = self.sf.lock();
            let old = map.remove(&key);
            drop(map);
            self.sf.cv.notify_all();
            if let Some(Slot::Running(waiters)) = old {
                for w in waiters {
                    w(None);
                }
            }
        }
    }
}
