//! The inline uniform updates and the snapshots of uniform buffers.

use super::*;

impl Walker {
    /// The command writes its bytes into the buffer when the GPU runs it. The walker writes them now and remembers the
    /// range, so that the draws that bind it get the bytes they saw.
    pub(super) fn update_uniform<M: GuestMem>(&mut self, mem: &mut M, x: &Ext) {
        self.track
            .update(x, self.out.draws.len(), self.out.dispatches.len());
        let mut bytes = std::mem::take(&mut self.update_bytes);
        self.write_update(mem, x, &mut bytes);
        self.update_bytes = bytes;
    }

    /// Writes the bytes of an update record through `bytes` and records the range.
    pub(in crate::walk) fn write_update<M: GuestMem>(
        &mut self,
        mem: &mut M,
        x: &Ext,
        bytes: &mut Vec<u8>,
    ) {
        let (buffer, offset, len) = (x.word64(0), x.word64(4), usize::from(x.arg));
        let at = buffer.saturating_add(offset);
        bytes.clear();
        bytes.extend(
            x.words()
                .get(6..)
                .unwrap_or(&[])
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .take(len),
        );
        let Ok(host) = self.map.host_address(at, bytes.len() as u64) else {
            return;
        };
        if let Some(log) = &mut self.undo {
            let mut old = vec![0u8; bytes.len()];
            if mem.read(host, &mut old).is_ok() {
                log.push((host, old));
            }
        }
        if mem.write(host, bytes).is_err() {
            return;
        }
        let end = at + bytes.len() as u64;
        self.snaps
            .get_mut()
            .retain(|(a, s), _| !(*a < end && at < *a + u64::from(*s)));
        match self.updated.last_mut() {
            Some((s, e)) if at <= *e && *s <= end => {
                *s = (*s).min(at);
                *e = (*e).max(end);
            }
            _ => {
                if self.updated.len() >= 512 {
                    self.updated.remove(0);
                }
                self.updated.push((at, end));
            }
        }
    }

    /// The uniform buffers of the bank (0 vertex, 1 fragment, 2 compute) that are bound, added to `out`.
    pub(in crate::walk) fn ubos_into<M: GuestMem>(
        &self,
        mem: &M,
        st: &State,
        slot: usize,
        stage: Stage,
        out: &mut Vec<UboDetail>,
    ) {
        out.extend(
            st.ubo[slot]
                .iter()
                .enumerate()
                .filter(|(_, (a, s))| *a != 0 && *s != 0)
                .map(|(i, (a, s))| UboDetail {
                    stage,
                    slot: i as u32,
                    address: *a,
                    size: *s,
                    snapshot: self.snapshot(mem, *a, *s),
                }),
        );
    }

    pub(in crate::walk) fn ubos<M: GuestMem>(
        &self,
        mem: &M,
        st: &State,
        slot: usize,
        stage: Stage,
    ) -> Vec<UboDetail> {
        let mut out = Vec::with_capacity(bound_count(&st.ubo[slot]));
        self.ubos_into(mem, st, slot, stage, &mut out);
        out
    }

    /// The bytes of a buffer that an update touched, as the guest memory holds them now.
    pub(in crate::walk) fn snapshot<M: GuestMem>(
        &self,
        mem: &M,
        addr: u64,
        size: u32,
    ) -> Option<Arc<[u8]>> {
        let t = self.prof.start();
        let r = self.snapshot_of(mem, addr, size);
        self.prof.lap(&self.prof.snap_ns, t);
        r
    }

    fn snapshot_of<M: GuestMem>(&self, mem: &M, addr: u64, size: u32) -> Option<Arc<[u8]>> {
        let p = &self.prof;
        p.count(&p.snaps, 1);
        let key = (addr, size);
        if let Some(snapshot) = self.snaps.borrow().get(&key) {
            p.count(&p.snap_hits, 1);
            return snapshot.clone();
        }
        let end = addr + u64::from(size);
        // A partial update does not reduce the bound range. Its unchanged tail can contain constants that the
        // shader still reads. Copying only the updated prefix makes those reads escape the scratch allocation.
        // With the overlap on, every bound block gets its copy: the CPU may write it for the next frame while the GPU
        // still reads this one.
        if !self.copy_all && !self.updated.iter().any(|(s, e)| addr < *e && *s < end) {
            // Most bound blocks have no inline updates. Remember that result as well: scanning the update
            // history for every UBO of every draw otherwise dominates the command walker on large frames.
            self.snaps.borrow_mut().insert(key, None);
            p.count(&p.snap_none, 1);
            return None;
        }
        let host = self.map.host_address(addr, u64::from(size)).ok()?;
        let earlier = self.frame_snaps.borrow().get(&key).cloned();
        let snap = match earlier {
            Some(earlier) => {
                // The range was copied before in this frame (another submit). Read the bytes aside: when they are the
                // same, the draws share the buffer and the upload of the frame sees one block.
                let mut tmp = self.snap_tmp.borrow_mut();
                tmp.resize(size as usize, 0);
                mem.read(host, &mut tmp[..]).ok()?;
                if tmp[..] == earlier[..] {
                    p.count(&p.snap_shared, 1);
                    self.snaps.borrow_mut().insert(key, Some(earlier.clone()));
                    return Some(earlier);
                }
                let mut buffer = self.snap_buffer(size);
                Arc::get_mut(&mut buffer)?.copy_from_slice(&tmp);
                buffer
            }
            None => {
                let mut buffer = self.snap_buffer(size);
                mem.read(host, Arc::get_mut(&mut buffer)?).ok()?;
                buffer
            }
        };
        p.count(&p.snap_copied, 1);
        p.count(&p.snap_bytes, u64::from(size));
        self.keep_snap_buffer(&snap);
        self.frame_snaps.borrow_mut().insert(key, snap.clone());
        self.snaps.borrow_mut().insert(key, Some(snap.clone()));
        Some(snap)
    }

    /// A buffer of `size` bytes for a snapshot that nothing else holds. The buffers of snapshots whose frames were
    /// encoded and dropped come back, so a large uniform buffer costs a copy, not a fresh allocation and its page faults.
    /// The pool of a size is a queue in the order of issue. Frames are dropped in that order, so the oldest buffer is
    /// the one that is free first, and one look at the front decides.
    fn snap_buffer(&self, size: u32) -> Arc<[u8]> {
        {
            let mut pool = self.snap_pool.borrow_mut();
            let ring = pool.entry(size).or_default();
            if ring
                .front()
                .is_some_and(|b| Arc::strong_count(b) == 1 && Arc::weak_count(b) == 0)
                && let Some(b) = ring.pop_front()
            {
                return b;
            }
        }
        let p = &self.prof;
        let t = p.start();
        let b: Arc<[u8]> = vec![0u8; size as usize].into();
        p.lap(&p.alloc_snap_ns, t);
        p.count(&p.snap_fresh, 1);
        b
    }

    /// Keeps a reference to a snapshot buffer, so it comes back once the frame that holds it is dropped.
    fn keep_snap_buffer(&self, b: &Arc<[u8]>) {
        const KEEP: usize = 2048;
        let mut pool = self.snap_pool.borrow_mut();
        let ring = pool.entry(b.len() as u32).or_default();
        if ring.len() < KEEP {
            ring.push_back(b.clone());
        }
    }
}
