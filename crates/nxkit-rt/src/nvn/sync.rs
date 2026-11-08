//! The sync group: `nvnSync*` and the sync part of `nvnQueueFenceSync` and `nvnQueueWaitSync` (see the queue group).
//!
//! The `NVNsync` is 0x40 bytes in the headers. The host uses the first 0x10 bytes as plain data and writes no more:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | magic `SYNC` while initialised |
//! | 4 | `fenced`: 0 never fenced, 1 fenced by a queue, 2 fenced by a window acquire |
//! | 8 | the fence number of a queue fence |
//!
//! A sync object that no queue fenced never signals, so a wait on it ends with a timeout, as on a device.
//! A sync that a queue fenced signals when the GPU completed the work submitted until then ([`Gpu::fence_now`](super::ctx::Gpu::fence_now)).
//! A sync that a window acquire fenced is signalled already, because `Backend::acquire` blocks until the image is free.

use nxkit_nvn::backend::{Fence, WaitResult};

use super::abi::{AbiResult, Obj, guard, magic, other, table, used};
use super::ctx::gpu;

/// The magic of an initialised sync.
pub(super) const SYNC: u32 = magic(b'S', b'Y', b'N', b'C');
/// The sync was never fenced.
pub(super) const NOT_FENCED: u32 = 0;
/// A queue fenced the sync.
pub(super) const QUEUE_FENCE: u32 = 1;
/// A window acquire fenced the sync.
pub(super) const ACQUIRE_FENCE: u32 = 2;

const OFF_FENCED: u64 = 4;
const OFF_FENCE: u64 = 8;

const ALREADY_SIGNALED: u32 = 0;
const CONDITION_SATISFIED: u32 = 1;
const TIMEOUT_EXPIRED: u32 = 2;

/// Marks the sync at `s` as fenced by a queue with `fence`.
pub(super) fn set_queue_fence(s: Obj, fence: Fence) -> AbiResult<()> {
    s.set_u32(OFF_FENCED, QUEUE_FENCE)?;
    s.set_u64(OFF_FENCE, fence.0)
}

/// Whether a queue or a window acquire fenced the sync at `s`.
pub(super) fn is_fenced(s: Obj) -> AbiResult<bool> {
    Ok(s.u32(OFF_FENCED)? != NOT_FENCED)
}

/// Marks the sync at `s` as fenced by a window acquire.
pub(super) fn set_acquire_fence(s: Obj) -> AbiResult<()> {
    s.set_u32(OFF_FENCED, ACQUIRE_FENCE)?;
    s.set_u64(OFF_FENCE, 0)
}

/// What `nvnSyncWait` gives for a sync in the given state. `wait` is the wait on the backend, `(fence, timeout)`.
fn wait_result(
    fenced: u32,
    fence: Fence,
    timeout: u64,
    mut wait: impl FnMut(Fence, u64) -> AbiResult<WaitResult>,
) -> AbiResult<u32> {
    match fenced {
        NOT_FENCED => Ok(TIMEOUT_EXPIRED),
        ACQUIRE_FENCE => Ok(ALREADY_SIGNALED),
        _ => {
            if matches!(wait(fence, 0)?, WaitResult::AlreadyDone | WaitResult::Done) {
                return Ok(ALREADY_SIGNALED);
            }
            match wait(fence, timeout)? {
                WaitResult::AlreadyDone => Ok(ALREADY_SIGNALED),
                WaitResult::Done => Ok(CONDITION_SATISFIED),
                WaitResult::TimedOut => Ok(TIMEOUT_EXPIRED),
                WaitResult::Failed => Err(other("the backend failed to wait for a fence")),
            }
        }
    }
}

extern "C" fn sync_initialize(s: u64, _dev: u64) -> u32 {
    guard("nvnSyncInitialize", || {
        let o = Obj(s);
        o.zero(used::SYNC)?;
        o.set_u32(0, SYNC)?;
        Ok(1)
    })
}

extern "C" fn sync_finalize(s: u64) {
    guard("nvnSyncFinalize", || Obj(s).set_u32(0, 0));
}

extern "C" fn sync_set_debug_label(_s: u64, _label: u64) {}

extern "C" fn sync_wait(s: u64, timeout: u64) -> u32 {
    super::frametime::timed(super::frametime::Part::Wait, || sync_wait_in(s, timeout))
}

fn sync_wait_in(s: u64, timeout: u64) -> u32 {
    guard("nvnSyncWait", || {
        let o = Obj(s);
        let fenced = o.u32(OFF_FENCED)?;
        let fence = Fence(o.u64(OFF_FENCE)?);
        let g = gpu("nvnSyncWait");
        wait_result(fenced, fence, timeout, |f, t| g.wait(f, t))
    })
}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnSyncInitialize" => sync_initialize as extern "C" fn(u64, u64) -> u32,
        "nvnSyncFinalize" => sync_finalize as extern "C" fn(u64),
        "nvnSyncSetDebugLabel" => sync_set_debug_label as extern "C" fn(u64, u64),
        "nvnSyncWait" => sync_wait as extern "C" fn(u64, u64) -> u32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(fenced: u32, results: &[WaitResult]) -> (u32, Vec<u64>) {
        let mut asked = Vec::new();
        let mut it = results.iter().copied();
        let r = wait_result(fenced, Fence(3), 500, |_, t| {
            asked.push(t);
            Ok(it.next().unwrap_or(WaitResult::TimedOut))
        })
        .expect("result");
        (r, asked)
    }

    #[test]
    fn a_sync_that_was_never_fenced_times_out_without_asking_the_backend() {
        assert_eq!(run(NOT_FENCED, &[]), (TIMEOUT_EXPIRED, vec![]));
    }

    #[test]
    fn an_acquire_sync_is_signalled() {
        assert_eq!(run(ACQUIRE_FENCE, &[]), (ALREADY_SIGNALED, vec![]));
    }

    #[test]
    fn a_fence_that_is_done_is_already_signalled_after_one_poll() {
        assert_eq!(
            run(QUEUE_FENCE, &[WaitResult::AlreadyDone]),
            (ALREADY_SIGNALED, vec![0])
        );
        assert_eq!(
            run(QUEUE_FENCE, &[WaitResult::Done]),
            (ALREADY_SIGNALED, vec![0])
        );
    }

    #[test]
    fn a_fence_that_completes_during_the_wait_satisfies_the_condition() {
        assert_eq!(
            run(QUEUE_FENCE, &[WaitResult::TimedOut, WaitResult::Done]),
            (CONDITION_SATISFIED, vec![0, 500])
        );
        assert_eq!(
            run(QUEUE_FENCE, &[WaitResult::TimedOut, WaitResult::TimedOut]),
            (TIMEOUT_EXPIRED, vec![0, 500])
        );
    }

    #[test]
    fn a_failed_wait_is_an_error() {
        let r = wait_result(QUEUE_FENCE, Fence(1), 1, |_, _| Ok(WaitResult::Failed));
        assert!(r.is_err());
    }

    #[test]
    fn the_group_exports_every_name_of_its_row() {
        let got: std::collections::HashSet<&str> = procs().iter().map(|(n, _)| *n).collect();
        let want: std::collections::HashSet<&str> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "sync")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        assert_eq!(got, want);
    }
}
