//! Committing a batch: the split into groups, the commit of a prefix ahead of the rest, and the submit.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use objc2_metal::MTL4CommandBuffer;

use super::{Ahead, Batch, Callback, Ended, Entry, Split, SplitState, SubmittedBatch};
use crate::error::Result;
use crate::metrics::CommandBufferStats;
use crate::table::Keep;

impl Batch {
    fn split(&mut self) -> Arc<Split> {
        if let Some(s) = &self.split {
            return s.clone();
        }
        let mut bounds: Vec<usize> = self.early.iter().map(|e| e.0).collect();
        bounds.push(self.buffers.len());
        bounds.sort_unstable();
        bounds.dedup();
        let callbacks: Vec<(usize, Option<Callback>)> = std::mem::take(&mut self.early)
            .into_iter()
            .map(|(count, f)| {
                (
                    bounds.iter().position(|b| *b == count).unwrap_or(0),
                    Some(f),
                )
            })
            .collect();
        let split = Arc::new(Split {
            number: self.number,
            ctx: self.ctx.clone(),
            done: self.done.clone(),
            metrics: self.metrics.clone(),
            remaining: AtomicUsize::new(bounds.len()),
            state: Mutex::new(SplitState {
                finished: vec![false; bounds.len()],
                callbacks,
                start: 0.0,
                end: 0.0,
            }),
            bounds,
        });
        self.split = Some(split.clone());
        split
    }

    /// For a batch with a prefix: the handle that commits the first group while the rest of the batch is encoded. Every
    /// command buffer of the group must be handed to `Ahead::finish` by the thread that encoded it (the first one,
    /// `buffers()[0]`, included), and nothing may be encoded into them afterwards. [`submit`](Self::submit) then commits
    /// only the other groups. None when the batch has no prefix.
    pub fn commit_ahead(&mut self) -> Option<Arc<Ahead>> {
        if let Some(a) = &self.ahead {
            return Some(a.clone());
        }
        if self.early.is_empty() && self.split.is_none() {
            return None;
        }
        let split = self.split();
        if split.bounds.len() < 2 {
            return None;
        }
        let n = split.bounds[0];
        self.done.set_pending();
        let ahead = Arc::new(Ahead {
            split,
            scratch: self.scratch.clone(),
            t0: self.t0,
            slots: Mutex::new((0..n).map(|_| None).collect()),
            left: AtomicUsize::new(n),
            committed: AtomicBool::new(false),
        });
        self.ahead = Some(ahead.clone());
        Some(ahead)
    }

    /// Streams the batch: the first `count` command buffers are committed as soon as each one is ended (by
    /// `Ahead::finish`, on the thread that encoded it) while the other command buffers are still being encoded, so
    /// the GPU starts after that prefix and not after the whole batch. The order of the command buffers stays the order of
    /// the slice. Returns None when `count` is 0 or not smaller than the number of command buffers, or when the groups are
    /// fixed already. Every command buffer of the prefix must be handed to `Ahead::finish`, and nothing may be encoded
    /// into them afterwards; [`submit`](Self::submit) then commits only the rest. The commit costs the GPU a short pause at
    /// the boundary (about 0.2 ms).
    pub fn stream_prefix(&mut self, count: usize) -> Option<Arc<Ahead>> {
        if !self.on_prefix_complete(count, || {}) {
            return None;
        }
        self.commit_ahead()
    }

    /// Ends every command buffer and commits them in order. Returns at once.
    ///
    /// One commit for the whole batch: committed one by one, the GPU paused 0.2 to 1 ms at every command buffer
    /// boundary (Breath of the Wild, six command buffers a frame). A batch with prefixes
    /// ([`on_prefix_complete`](Self::on_prefix_complete)) is committed in as many groups as it has prefixes plus one.
    pub fn submit(mut self) -> Result<SubmittedBatch> {
        let cpu_encode_ns = u64::try_from(self.t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let number = self.number;
        if self.buffers.is_empty() {
            self.done.set_pending();
            self.done.add(
                &CommandBufferStats {
                    frame: number,
                    cpu_encode_ns,
                    ..Default::default()
                },
                true,
            );
            return Ok(SubmittedBatch {
                number,
                done: self.done.clone(),
            });
        }
        let has_ahead = self.ahead.is_some();
        let split = self.split();
        if !has_ahead {
            self.done.set_pending();
        }
        let ahead = self.ahead.clone();
        let ahead_done = ahead
            .as_ref()
            .is_some_and(|a| a.committed.load(Ordering::Acquire));
        // The command buffers of the groups still to commit, as ended.
        let mut ended: Vec<Entry> = Vec::with_capacity(self.buffers.len());
        for (i, cb) in self.buffers.iter_mut().enumerate() {
            let from_ahead = ahead.as_ref().and_then(|a| {
                a.slots
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_mut(i)
                    .and_then(Option::take)
            });
            if ahead_done && i < split.bounds[0] {
                continue;
            }
            match from_ahead {
                Some(e) => ended.push(e),
                None => {
                    cb.raw().endCommandBuffer();
                    ended.push((
                        Ended(NonNull::from(&**cb.raw())),
                        vec![Keep::new(cb.take_retained())],
                    ));
                }
            }
        }
        let mut at = if ahead_done { split.bounds[0] } else { 0 };
        let mut entries = ended.into_iter();
        for (g, &end) in split.bounds.iter().enumerate() {
            if ahead_done && g == 0 {
                continue;
            }
            let group: Vec<Entry> = entries.by_ref().take(end - at).collect();
            at = end;
            split.commit(g, group, self.scratch.clone(), cpu_encode_ns);
        }
        Ok(SubmittedBatch {
            number,
            done: self.done.clone(),
        })
    }
}
