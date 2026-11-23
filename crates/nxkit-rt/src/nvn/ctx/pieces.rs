//! Submits that the queue gate splits at their waits.
//!
//! On the console a queue that reaches a wait inside a recording stops there, and another queue runs until it signals.
//! Ocarina of Time hands work between its main queue and its lighting queue about ten times in a frame, inside one
//! recording on each side (the main queue's waits are in the recordings that it calls): the main queue waits for the
//! CPU, signals the lighting queue, waits for it, signals it again. A gate that holds whole submits can only run one of
//! the two first, so the lighting ran before the main queue's work that it waits for. The gate therefore holds a submit
//! with such waits as parts: each part of a recording starts at a wait, also one in a called recording, and runs to the
//! next one. A place in a recording is the call records that lead to a record, then the record. The front end decodes the recording once, with
//! the first part, and keeps the later parts (`Gate::pieces`). The backend walks the records of each part when the part
//! runs ([`nxkit_nvn::backend::Backend::submit_range`]). Parts of a queue run in order, as its submits do.
//!
//! `NXKIT_QUEUE_SPLIT=1` turns it on. It is off by default: in Ocarina of Time it leaves no work that a flush runs
//! before its signal (8 parts a frame before), and the analysis sums of the exposure reach guest memory, but the
//! picture is still too bright and the frame rate was not measured cleanly yet.

use nxkit_nvn::cmdbuf::CommandHandle;
use nxkit_nvn::marks::MarkOp;

use super::gate::{Gate, Op};
use super::{Front, Gpu};
use crate::nvn::abi::{AbiResult, log_once};

/// A part of a recording: the records from the place `start` to the place `stop`, and the decoder's marks where the
/// later parts start (the first part only).
pub(super) struct Part<'a> {
    pub(super) handle: CommandHandle,
    pub(super) start: &'a [u64],
    pub(super) stop: Option<&'a [u64]>,
    pub(super) origin: bool,
    pub(super) cuts: &'a [usize],
}

/// The marks of a recording where a part starts: a wait that is not the first record. Each gives its index among the
/// marks and its place.
fn cut_points(marks: &[(MarkOp, Vec<u64>)], handle: u64) -> Vec<(usize, Vec<u64>)> {
    marks
        .iter()
        .enumerate()
        .filter_map(|(i, (m, at))| match m {
            MarkOp::WaitEvent { .. } | MarkOp::WaitSync { .. } if at[..] != [handle] => {
                Some((i, at.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Holds the submit of `handles` on `queue` as parts: whole recordings without a wait to split at go together, as one
/// submit. Returns false, holding nothing, when no recording has such a wait.
pub(super) fn push_split(
    front: &Front,
    gate: &mut Gate,
    queue: u64,
    handles: &[CommandHandle],
    origin: bool,
) -> bool {
    let mut per = Vec::with_capacity(handles.len());
    for h in handles {
        let mut v = Vec::new();
        if front.queue_marks_at(*h, &mut v).is_err() {
            return false;
        }
        per.push(v);
    }
    let cuts: Vec<Vec<(usize, Vec<u64>)>> = handles
        .iter()
        .zip(&per)
        .map(|(h, v)| cut_points(v, h.0))
        .collect();
    if cuts.iter().all(Vec::is_empty) {
        return false;
    }
    let mut plain: Vec<CommandHandle> = Vec::new();
    let mut plain_marks: Vec<MarkOp> = Vec::new();
    let flush_plain = |gate: &mut Gate, plain: &mut Vec<CommandHandle>, marks: &mut Vec<MarkOp>| {
        if !plain.is_empty() {
            let op = Op::Submit {
                handles: std::mem::take(plain),
                origin,
            };
            gate.push_marked(queue, op, Some(std::mem::take(marks)));
        }
    };
    for ((h, v), c) in handles.iter().zip(&per).zip(&cuts) {
        if c.is_empty() {
            plain.push(*h);
            plain_marks.extend(v.iter().map(|(m, _)| *m));
            continue;
        }
        flush_plain(gate, &mut plain, &mut plain_marks);
        let index: Vec<usize> = c.iter().map(|(i, _)| *i).collect();
        let mut bounds = vec![(0, vec![h.0])];
        bounds.extend(c.iter().cloned());
        for (k, (from, start)) in bounds.iter().enumerate() {
            let next = bounds.get(k + 1);
            let to = next.map_or(v.len(), |b| b.0);
            let marks = v[*from..to].iter().map(|(m, _)| *m).collect();
            let op = Op::Piece {
                handle: *h,
                start: start.clone(),
                stop: next.map(|b| b.1.clone()),
                origin,
                cuts: if k == 0 { index.clone() } else { Vec::new() },
            };
            gate.push_marked(queue, op, Some(marks));
        }
    }
    flush_plain(gate, &mut plain, &mut plain_marks);
    true
}

impl Gpu {
    /// Whether the gate splits submits at their waits: `NXKIT_QUEUE_SPLIT=1` and the backend walks parts.
    pub(super) fn splits(&self) -> bool {
        self.queue_split && self.backend().splits()
    }

    /// Runs a part of a recording: the first part decodes the recording and keeps the later parts, a later part takes its
    /// own. The backend walks the records of the part.
    pub(super) fn run_piece(
        &self,
        front: &mut Front,
        gate: &mut Gate,
        queue: u64,
        p: Part<'_>,
    ) -> AbiResult<()> {
        let key = (queue, p.handle.0);
        let piece = if p.start == [p.handle.0] {
            self.queue_flush.submitted();
            let mut parts = front.decode_pieces(queue as usize, p.handle, p.cuts)?;
            if parts.len() != p.cuts.len() + 1 {
                log_once(
                    "queue-split-parts",
                    format_args!(
                        "the gate split a recording into {} parts, the decoder into {}",
                        p.cuts.len() + 1,
                        parts.len()
                    ),
                );
            }
            let first = parts.remove(0);
            gate.pieces.insert(key, parts.into());
            first
        } else {
            let next = gate.pieces.get_mut(&key).and_then(|q| q.pop_front());
            let Some(next) = next else {
                log_once(
                    "queue-split-missing",
                    format_args!("a part of a split recording has no decoded commands"),
                );
                return Ok(());
            };
            next
        };
        if gate.pieces.get(&key).is_some_and(|q| q.is_empty()) {
            gate.pieces.remove(&key);
        }
        if let Some(walk) = self.backend().submit_range(
            queue,
            p.handle,
            p.start.to_vec(),
            p.stop.map(<[u64]>::to_vec),
            p.origin,
        ) {
            walk();
        }
        front.append_piece(queue as usize, &piece);
        Ok(())
    }
}
