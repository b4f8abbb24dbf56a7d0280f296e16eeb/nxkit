//! Small helpers of [`super`]: the switches, the bindings of a dispatch, the hash of the weights, the run in the commands.

use super::*;

pub(super) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NXKIT_NDVE").map_or(true, |v| v != "0"))
}

pub(super) fn compare_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NXKIT_NDVE_COMPARE").is_ok_and(|v| v == "1"))
}

/// The storage buffer binding of a compute dispatch at `slot`.
pub(super) fn sbuf_of(dd: &DispatchDetail, slot: u32) -> Option<&UboDetail> {
    dd.sbufs
        .iter()
        .find(|s| s.stage == Stage::Compute && s.slot == slot)
}

pub(super) fn bind_of(list: &[TextureBind], slot: u32) -> Option<&TextureBind> {
    list.iter()
        .find(|t| t.stage == Stage::Compute && t.slot == slot)
}

/// The address where a blob starts, when a binding at `offset` into it has the address `address`.
pub(super) fn blob_start(address: u64, offset: u64) -> Option<u64> {
    address.checked_sub(offset)
}

pub(super) fn hash_blobs(blobs: &[Vec<u8>]) -> u64 {
    let mut h = foldhash::fast::FixedState::with_seed(0).build_hasher();
    for b in blobs {
        h.write_usize(b.len());
        h.write(b);
    }
    h.finish()
}

/// The command indices of the first and the last dispatch of the run, when only dispatches, barriers and fences lie
/// between them. A copy or a draw inside the run would mean that the game does more than the chain.
pub(super) fn run_commands(cmds: &[Cmd], run: Run) -> Option<(usize, usize)> {
    let mut seen = 0usize;
    let mut inside = 0usize;
    let (mut first, mut last) = (None, None);
    for (i, c) in cmds.iter().enumerate() {
        match c {
            Cmd::Dispatch { .. } => {
                if seen >= run.start && seen < run.start + run.len {
                    first.get_or_insert(i);
                    last = Some(i);
                    inside += 1;
                }
                seen += 1;
            }
            Cmd::Barrier(_) | Cmd::Sync => {}
            _ if first.is_some() && seen < run.start + run.len => return None,
            _ => {}
        }
    }
    if inside != run.len {
        return None;
    }
    first.zip(last)
}
