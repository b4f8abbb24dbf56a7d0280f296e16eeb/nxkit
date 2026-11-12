//! `PoolSet`: the current snapshot of the pools and the registration that changes it (copy on write).

use super::*;

struct State<B: Backing> {
    current: Arc<PoolSnapshot<B>>,
    generation: u64,
    next_id: u64,
}

/// The current snapshot and the way to change it.
///
/// `register` and `unregister` run on the game thread. They hold the lock of the set for the whole change (a few
/// microseconds, most of it making the buffer), and `current` takes it only to clone an `Arc`.
pub struct PoolSet<B: Backing = GuestMemory> {
    state: Mutex<State<B>>,
    /// `None` in the whole mode: `register` and `unregister` change nothing.
    maker: Option<Box<dyn MakeBacking<B>>>,
    pinner: Arc<Pinner>,
    yard: Arc<Yard<B>>,
    /// A guest address `va` is the host address `va - va_base + host_base`.
    va_base: u64,
    host_base: u64,
}

fn round_down(v: u64) -> u64 {
    v & !(PAGE - 1)
}

impl<B: Backing> PoolSet<B> {
    /// A set that makes one backing per pool with `maker`. A guest address `va` is the host address
    /// `va - va_base + host_base`; both bases must be multiples of [`PAGE`].
    pub fn new(
        va_base: u64,
        host_base: u64,
        maker: impl MakeBacking<B> + 'static,
        pins: Arc<dyn PagePins>,
    ) -> Self {
        debug_assert!(va_base.is_multiple_of(PAGE) && host_base.is_multiple_of(PAGE));
        let mut set = Self::with_snapshot(PoolSnapshot::empty(), pins, va_base, host_base);
        set.maker = Some(Box::new(maker));
        set
    }

    /// The whole mode: one entry over `[base, base + len)` and the given backing. There are no pins and no makes.
    /// `register` makes no backing and no pin and changes nothing: it returns the id of the single entry (0), because
    /// every address of the range resolves there already. `unregister` returns false. The generation stays 0.
    pub fn whole(base: u64, len: u64, mem: B, pins: Arc<dyn PagePins>) -> Self {
        let snap = PoolSnapshot::whole(base, len, mem);
        Self::with_snapshot(snap, pins, base, base)
    }

    fn with_snapshot(
        snap: PoolSnapshot<B>,
        pins: Arc<dyn PagePins>,
        va_base: u64,
        host_base: u64,
    ) -> Self {
        let pinner = Arc::new(Pinner {
            pins,
            count: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            alive: AtomicU64::new(0),
            alive_bytes: AtomicU64::new(0),
        });
        let yard = Arc::new(Yard {
            items: Mutex::new(Vec::new()),
            pinner: Arc::clone(&pinner),
        });
        Self {
            state: Mutex::new(State {
                current: Arc::new(snap),
                generation: 0,
                next_id: 1,
            }),
            maker: None,
            pinner,
            yard,
            va_base,
            host_base,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<B>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The snapshot now. Clone-cheap; hold it for as long as a frame needs it.
    pub fn current(&self) -> Arc<PoolSnapshot<B>> {
        Arc::clone(&self.lock().current)
    }

    /// Registers the pool `[start, start + len)` (guest addresses) and returns its id. The pools it overlaps leave the
    /// snapshot: the game finalized them, or it reuses the address. The buffer covers the range rounded out to
    /// [`PAGE`]. If the buffer cannot be made, the pool is registered without one, a line says why and `resolve`
    /// gives [`Miss::NoBuffer`]. In the whole mode nothing changes (see [`PoolSet::whole`]).
    pub fn register(&self, start: u64, len: u64, flags: u32) -> Result<u64, BadRange> {
        let bad = BadRange { start, len };
        let end = start.checked_add(len).filter(|_| len > 0).ok_or(bad)?;
        let mut st = self.lock();
        let Some(maker) = &self.maker else {
            return Ok(st.current.entries.first().map_or(0, |e| e.id));
        };
        let r_start = round_down(start);
        let r_end = end.checked_add(PAGE - 1).ok_or(bad)? & !(PAGE - 1);
        let host_start = r_start
            .checked_sub(self.va_base)
            .and_then(|o| o.checked_add(self.host_base))
            .ok_or(bad)?;
        let r_len = r_end - r_start;
        self.pinner.pin(host_start, r_len);
        let mem = match maker.make(host_start, r_len) {
            Ok(mem) => Some(mem),
            Err(why) => {
                self.pinner.unpin(host_start, r_len);
                eprintln!(
                    "nxkit-gpu pools: no buffer for the pool {start:#x}..{end:#x} (rounded {r_start:#x}..{r_end:#x}): {why}"
                );
                None
            }
        };
        let life = mem.is_some().then(|| {
            self.pinner.alive.fetch_add(1, Ordering::Relaxed);
            self.pinner.alive_bytes.fetch_add(r_len, Ordering::Relaxed);
            Life {
                yard: Arc::downgrade(&self.yard),
                pinner: Arc::clone(&self.pinner),
            }
        });
        let id = st.next_id;
        st.next_id += 1;
        let entry = PoolEntry {
            start,
            end,
            id,
            buf: Arc::new(PoolBuffer {
                mem,
                va_start: r_start,
                len: r_len,
                host_start,
                flags,
                life,
            }),
        };
        let old = &st.current.entries;
        let lo = old.partition_point(|e| e.end <= start);
        let hi = old.partition_point(|e| e.start < end).max(lo);
        let mut entries = Vec::with_capacity(old.len() - (hi - lo) + 1);
        entries.extend_from_slice(&old[..lo]);
        entries.push(entry);
        entries.extend_from_slice(&old[hi..]);
        self.publish(&mut st, entries);
        Ok(id)
    }

    /// The pool that starts at `start` is gone. Returns false when no pool starts there.
    pub fn unregister(&self, start: u64) -> bool {
        self.remove(|e| e.start == start)
    }

    /// The pool with this id is gone. Returns false when it is not in the current snapshot.
    pub fn unregister_id(&self, id: u64) -> bool {
        self.remove(|e| e.id == id)
    }

    fn remove(&self, hit: impl Fn(&PoolEntry<B>) -> bool) -> bool {
        let mut st = self.lock();
        if self.maker.is_none() {
            return false;
        }
        let Some(i) = st.current.entries.iter().position(hit) else {
            return false;
        };
        let mut entries = st.current.entries.clone();
        entries.remove(i);
        self.publish(&mut st, entries);
        true
    }

    /// Replaces the snapshot. The old one may drop here, under the lock of the state. That is safe: its backings can
    /// only reach the graveyard, whose lock is a leaf, or (if the graveyard is gone) drop and unpin in place.
    fn publish(&self, st: &mut State<B>, entries: Vec<PoolEntry<B>>) {
        st.generation += 1;
        st.current = Arc::new(PoolSnapshot {
            generation: st.generation,
            entries,
            overlay: None,
        });
    }

    /// Drops the backings in the graveyard and unpins their ranges. The encoder calls it at the start of a frame.
    pub fn drain_graveyard(&self) -> Drained {
        self.yard.drain()
    }

    /// The counters for the memory report.
    pub fn stats(&self) -> PoolStats {
        let snap = self.current();
        let (mut live, mut live_bytes, mut owned, mut owned_bytes) = (0, 0, 0, 0);
        for e in snap.entries.iter().filter(|e| e.buf.mem.is_some()) {
            live += 1;
            live_bytes += e.buf.len;
            if e.buf.life.is_some() {
                owned += 1;
                owned_bytes += e.buf.len;
            }
        }
        let alive = self.pinner.alive.load(Ordering::Relaxed);
        let alive_bytes = self.pinner.alive_bytes.load(Ordering::Relaxed);
        let (graveyard, graveyard_bytes) = {
            let items = self.yard.lock();
            (items.len() as u64, items.iter().map(|d| d.len).sum())
        };
        PoolStats {
            live,
            live_bytes,
            retired: alive.saturating_sub(owned),
            retired_bytes: alive_bytes.saturating_sub(owned_bytes),
            graveyard,
            graveyard_bytes,
            pins: self.pinner.count.load(Ordering::Relaxed),
            pin_bytes: self.pinner.bytes.load(Ordering::Relaxed),
        }
    }
}

/// The set with Metal buffers.
pub type MetalPoolSet = PoolSet<GuestMemory>;
