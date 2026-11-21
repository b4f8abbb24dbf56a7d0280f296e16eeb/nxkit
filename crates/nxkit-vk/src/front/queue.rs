//! The queue: submits, waits, fences, semaphores, events and query pools.
//!
//! Every queue is one serial queue. A submit gets the next serial; a fence given to it is done when that serial has
//! completed, and so is an event that a command sets and a query that a command writes. Semaphores order nothing: the
//! submits run in order, so every wait is already met.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::backend::{CommandBufferSnapshot, SetContents, Submission};
use crate::cmd::Cmd;
use crate::consts::query_type;
use crate::desc::{QueryPoolId, QueryType};
use crate::mem::Mem;
use crate::result::{Status, VkError, VkResult};

use super::Vk;
use super::state::{QueryPool, Signal, State};

/// `VK_QUERY_RESULT_PARTIAL_BIT`.
const PARTIAL: u32 = 0x8;

/// The slice of a wait between two looks at the fences, so a fence that another thread submits meanwhile is seen.
const WAIT_SLICE: Duration = Duration::from_millis(100);

/// The values of a range of queries: None for one that is not available.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueryResults(pub Vec<Option<u64>>);

/// What a submit changes in the state once it has its serial.
fn note_submit(s: &mut State, cmds: &[Cmd], serial: u64) {
    for c in cmds {
        let (pool, range) = match c {
            Cmd::ResetQueryPool { pool, first, count } => {
                (*pool, *first..first.saturating_add(*count))
            }
            Cmd::BeginQuery { pool, query, .. }
            | Cmd::EndQuery { pool, query }
            | Cmd::WriteTimestamp { pool, query } => (*pool, *query..query + 1),
            Cmd::SetEvent(e) => {
                if let Some(ev) = s.events.get_mut(*e) {
                    *ev = Signal::Pending(serial);
                }
                continue;
            }
            Cmd::ResetEvent(e) => {
                if let Some(ev) = s.events.get_mut(*e) {
                    *ev = Signal::Unsignaled;
                }
                continue;
            }
            _ => continue,
        };
        if let Some(q) = s.query_pools.get_mut(pool.0) {
            for i in range {
                if let Some(t) = q.tickets.get_mut(i as usize) {
                    *t = Some(serial);
                }
            }
        }
    }
}

/// Adds the sets and secondary command buffers that `cmds` name to the submission, two levels deep.
fn gather(s: &State, cmds: &[Cmd], sub: &mut Submission, depth: u32) {
    for c in cmds {
        match c {
            Cmd::BindDescriptorSets { sets, .. } => {
                for &h in sets {
                    if let Some(set) = s.sets.get(h) {
                        sub.sets.entry(h).or_insert_with(|| SetContents {
                            layout: set.layout.clone(),
                            descriptors: set.descriptors.clone(),
                        });
                    }
                }
            }
            Cmd::ExecuteCommands(cbs) if depth < 2 => {
                for &h in cbs {
                    if sub.secondaries.contains_key(&h) {
                        continue;
                    }
                    if let Some(b) = s.command_buffers.get(h) {
                        let inner = b.frozen.clone();
                        sub.secondaries.insert(h, inner.clone());
                        gather(s, &inner, sub, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Vk {
    /// `vkQueueSubmit`: the command buffers of every submit info in order, as one submission; the fence (0 for none)
    /// is done when it completes.
    pub fn submit(&self, command_buffers: &[Vec<u64>], fence: u64) -> VkResult<()> {
        let mut clock = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let serial = clock.last + 1;
        let sub = {
            let mut s = self.lock();
            let mut sub = Submission {
                serial,
                ..Submission::default()
            };
            for &h in command_buffers.iter().flatten() {
                let Some(b) = s.command_buffers.get(h) else {
                    self.log.once(
                        "submit unknown",
                        "[RED] a submit names an unknown command buffer",
                    );
                    continue;
                };
                sub.command_buffers.push(CommandBufferSnapshot {
                    handle: h,
                    cmds: b.frozen.clone(),
                });
            }
            let all: Vec<Arc<[Cmd]>> = sub.command_buffers.iter().map(|c| c.cmds.clone()).collect();
            for cmds in &all {
                gather(&s, cmds, &mut sub, 0);
            }
            let inner: Vec<Arc<[Cmd]>> = sub.secondaries.values().cloned().collect();
            for cmds in all.iter().chain(&inner) {
                note_submit(&mut s, cmds, serial);
            }
            if let Some(f) = s.fences.get_mut(fence) {
                *f = Signal::Pending(serial);
            }
            sub
        };
        if let Err(e) = self.backend.submit(&sub) {
            self.log
                .once("submit", format_args!("[RED] submit {serial}: {e}"));
        }
        clock.last = serial;
        Ok(())
    }

    /// The serial of the last submit.
    pub fn last_serial(&self) -> u64 {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last
    }

    /// `vkQueueWaitIdle` and `vkDeviceWaitIdle`: waits until every submit so far has completed.
    pub fn wait_idle(&self) {
        let last = self.last_serial();
        while !self.backend.wait(last, u64::MAX) {}
    }

    /// `vkQueueBindSparse`: sparse binding is not implemented. With no bind infos it only signals the fence.
    pub fn bind_sparse(&self, infos: u32, fence: u64) -> VkResult<()> {
        if let Some(f) = self.lock().fences.get_mut(fence) {
            *f = Signal::Signaled;
        }
        if infos > 0 {
            return Err(self.log.unsupported(
                "sparse",
                VkError::FeatureNotPresent,
                "sparse binding",
            ));
        }
        Ok(())
    }

    /// `vkCreateFence`.
    pub fn create_fence(&self, signaled: bool) -> u64 {
        let s = if signaled {
            Signal::Signaled
        } else {
            Signal::Unsignaled
        };
        self.lock().fences.insert(s)
    }

    /// `vkDestroyFence`.
    pub fn destroy_fence(&self, fence: u64) {
        self.lock().fences.remove(fence);
    }

    /// `vkResetFences`.
    pub fn reset_fences(&self, fences: &[u64]) {
        let mut s = self.lock();
        for &f in fences {
            if let Some(x) = s.fences.get_mut(f) {
                *x = Signal::Unsignaled;
            }
        }
    }

    fn done(&self, sig: Signal) -> bool {
        match sig {
            Signal::Signaled => true,
            Signal::Pending(v) => self.backend.completed() >= v,
            Signal::Unsignaled => false,
        }
    }

    /// `vkGetFenceStatus`: `VK_SUCCESS` or `VK_NOT_READY`.
    pub fn fence_status(&self, fence: u64) -> VkResult<Status> {
        let sig = *self
            .lock()
            .fences
            .get(fence)
            .ok_or_else(|| self.bad("fence", fence))?;
        Ok(if self.done(sig) {
            Status::Success
        } else {
            Status::NotReady
        })
    }

    /// `vkWaitForFences`: all or any of them, for at most `timeout_ns`. A fence that is never submitted never completes.
    pub fn wait_for_fences(&self, fences: &[u64], all: bool, timeout_ns: u64) -> VkResult<Status> {
        let start = Instant::now();
        let mut warned = false;
        loop {
            let sigs: Vec<Signal> = {
                let s = self.lock();
                fences
                    .iter()
                    .filter_map(|&f| s.fences.get(f).copied())
                    .collect()
            };
            let done = sigs.iter().filter(|&&g| self.done(g)).count();
            if (all && done == sigs.len()) || (!all && done > 0) {
                return Ok(Status::Success);
            }
            let spent = start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
            if spent >= timeout_ns {
                return Ok(Status::Timeout);
            }
            if !warned && spent > 5_000_000_000 {
                warned = true;
                self.log.once(
                    "fence wait",
                    format_args!(
                        "[RED] fence wait over 5 s ({} fences, {done} done)",
                        fences.len()
                    ),
                );
            }
            let pending = sigs.iter().filter_map(|g| match g {
                Signal::Pending(v) => Some(*v),
                _ => None,
            });
            let target = if all { pending.max() } else { pending.min() };
            let slice = (timeout_ns - spent).min(WAIT_SLICE.as_nanos() as u64);
            match target {
                Some(v) => {
                    self.backend.wait(v, slice);
                }
                None => std::thread::sleep(Duration::from_nanos(slice.min(1_000_000))),
            }
        }
    }

    /// `vkCreateSemaphore`.
    pub fn create_semaphore(&self) -> u64 {
        self.lock().semaphores.insert(())
    }

    /// `vkDestroySemaphore`.
    pub fn destroy_semaphore(&self, semaphore: u64) {
        self.lock().semaphores.remove(semaphore);
    }

    /// `vkCreateEvent`: reset.
    pub fn create_event(&self) -> u64 {
        self.lock().events.insert(Signal::Unsignaled)
    }

    /// `vkDestroyEvent`.
    pub fn destroy_event(&self, event: u64) {
        self.lock().events.remove(event);
    }

    /// `vkGetEventStatus`: `VK_EVENT_SET` or `VK_EVENT_RESET`.
    pub fn event_status(&self, event: u64) -> VkResult<Status> {
        let sig = *self
            .lock()
            .events
            .get(event)
            .ok_or_else(|| self.bad("event", event))?;
        Ok(if self.done(sig) {
            Status::EventSet
        } else {
            Status::EventReset
        })
    }

    /// `vkSetEvent` (`true`) and `vkResetEvent` (`false`) from the host.
    pub fn set_event(&self, event: u64, set: bool) -> VkResult<()> {
        let mut s = self.lock();
        let e = s
            .events
            .get_mut(event)
            .ok_or_else(|| self.bad("event", event))?;
        *e = if set {
            Signal::Signaled
        } else {
            Signal::Unsignaled
        };
        Ok(())
    }

    /// `vkCreateQueryPool`. Only occlusion and timestamp queries are written; other types are reported.
    pub fn create_query_pool(&self, ty: u32, count: u32) -> VkResult<u64> {
        let qt = match ty {
            query_type::OCCLUSION => QueryType::Occlusion,
            query_type::TIMESTAMP => QueryType::Timestamp,
            other => {
                self.log.once(
                    "query type",
                    format_args!("[RED] query pool type {other} not implemented"),
                );
                QueryType::Other(other)
            }
        };
        let h = self.lock().query_pools.insert(QueryPool {
            ty: qt,
            tickets: vec![None; crate::mem::check_count(count)? as usize],
        });
        if let Err(e) = self.backend.create_query_pool(QueryPoolId(h), qt, count) {
            self.log.once(
                "query pool",
                format_args!("[RED] cannot make a query pool: {e}"),
            );
        }
        Ok(h)
    }

    /// `vkDestroyQueryPool`.
    pub fn destroy_query_pool(&self, pool: u64) {
        if self.lock().query_pools.remove(pool).is_some() {
            self.backend.destroy_query_pool(QueryPoolId(pool));
        }
    }

    /// The values of queries `first..first + count`; `wait` waits for the ones that a submit wrote. A query that no
    /// submit has written stays unavailable instead of waiting for ever.
    pub fn query_values(
        &self,
        pool: u64,
        first: u32,
        count: u32,
        wait: bool,
    ) -> VkResult<QueryResults> {
        let (tickets, ty) = {
            let s = self.lock();
            let q = s
                .query_pools
                .get(pool)
                .ok_or_else(|| self.bad("query pool", pool))?;
            let end = (first as usize)
                .saturating_add(count as usize)
                .min(q.tickets.len());
            (
                q.tickets.get(first as usize..end).unwrap_or(&[]).to_vec(),
                q.ty,
            )
        };
        if matches!(ty, QueryType::Other(_)) {
            return Ok(QueryResults(vec![None; tickets.len()]));
        }
        let out = tickets
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let t = (*t)?;
                if wait {
                    while !self.backend.wait(t, u64::MAX) {}
                }
                (self.backend.completed() >= t).then(|| {
                    self.backend
                        .query_value(QueryPoolId(pool), first + i as u32)
                })
            })
            .collect();
        Ok(QueryResults(out))
    }

    /// `vkGetQueryPoolResults`: writes the values (32 or 64 bits, with availability when asked) to the game's `data`
    /// of `size` bytes, one query per `stride`. `VK_NOT_READY` when one is not available.
    #[allow(clippy::too_many_arguments)]
    pub fn query_pool_results(
        &self,
        m: &impl Mem,
        pool: u64,
        first: u32,
        count: u32,
        size: u64,
        data: u64,
        stride: u64,
        flags: u32,
    ) -> VkResult<Status> {
        use crate::consts::query_result as qr;
        let vals = self.query_values(pool, first, count, flags & qr::WAIT != 0)?;
        let w64 = flags & qr::R64 != 0;
        let word = if w64 { 8 } else { 4 };
        let row = word
            * if flags & qr::WITH_AVAILABILITY != 0 {
                2
            } else {
                1
            };
        let mut status = Status::Success;
        for (i, v) in vals.0.iter().enumerate() {
            let at = i as u64 * stride;
            if at + row > size {
                break;
            }
            let put = |off: u64, x: u64| {
                if w64 {
                    m.put_u64(data + at + off, x)
                } else {
                    m.put_u32(data + at + off, x as u32)
                }
            };
            if v.is_some() || flags & PARTIAL != 0 {
                put(0, v.unwrap_or(0))?;
            }
            if flags & qr::WITH_AVAILABILITY != 0 {
                put(word, u64::from(v.is_some()))?;
            }
            if v.is_none() {
                status = Status::NotReady;
            }
        }
        Ok(status)
    }
}
