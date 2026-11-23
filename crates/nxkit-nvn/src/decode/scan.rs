//! The waits and signals between queues of a recording, without its summary.
//!
//! The queue gate needs only the marks that order queues ([`MarkOp::orders_queues`]) to decide whether a submit is held.
//! A summary decodes every record with its state, and a recording that the game makes new in each frame misses the
//! cache, so the summary of the gate was a full decode on the submitting thread (Ocarina of Time, 2026-11-23: a fifth of
//! the render thread). The scan reads the header of each record, follows jumps and calls, and reads the body only of
//! the four records that order queues. The marks come in the order of the summary's stream. The summary itself is made
//! later by the submit, beside the walk.

use super::{MAX_CALL_DEPTH, MAX_RECORDS};
use crate::error::{NvnError, Result};
use crate::ext::ExtCode;
use crate::marks::MarkOp;
use crate::mem::GuestMem;
use crate::record::{HEADER_BYTES, MAX_RECORD_BYTES, Record};

/// The operation codes of the header that the scan follows. They are the ones of `record`.
const OP_END: u16 = 0;
const OP_JUMP: u16 = 1;
const OP_CALL: u16 = 2;

/// Appends the marks that order queues of the recording at `handle` and of the recordings it calls, in record order.
pub fn queue_marks<M: GuestMem>(mem: &M, handle: u64, out: &mut Vec<MarkOp>) -> Result<()> {
    scan(mem, handle, out, 0)
}

fn scan<M: GuestMem>(mem: &M, handle: u64, out: &mut Vec<MarkOp>, depth: u32) -> Result<()> {
    if depth > MAX_CALL_DEPTH {
        return Err(NvnError::BadRecord("calls are nested too deep"));
    }
    let mut pos = handle;
    for _ in 0..MAX_RECORDS {
        let mut h = [0u8; HEADER_BYTES as usize];
        mem.read(pos, &mut h)?;
        let op = u16::from_le_bytes([h[0], h[1]]);
        let size = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
        if size < HEADER_BYTES || size % 8 != 0 || size as usize > MAX_RECORD_BYTES {
            return Err(NvnError::BadRecord("size"));
        }
        match op {
            OP_END => return Ok(()),
            OP_JUMP | OP_CALL => match Record::read(mem, pos)?.0 {
                Record::Jump(to) => {
                    pos = to;
                    continue;
                }
                Record::Call(h) => scan(mem, h, out, depth + 1)?,
                _ => return Err(NvnError::BadRecord("jump or call")),
            },
            op if orders_queues(op) => {
                if let (Record::Ext(x), _) = Record::read(mem, pos)? {
                    let m = match x.code {
                        ExtCode::FenceSync => MarkOp::FenceSync { sync: x.word64(0) },
                        ExtCode::SignalEvent => MarkOp::SignalEvent {
                            event: x.word64(0),
                            mode: x.word(2),
                            value: x.word(5),
                        },
                        ExtCode::WaitSync => MarkOp::WaitSync { sync: x.word64(0) },
                        _ => MarkOp::WaitEvent {
                            event: x.word64(0),
                            mode: x.word(2),
                            value: x.word(3),
                        },
                    };
                    out.push(m);
                }
            }
            _ => {}
        }
        pos += u64::from(size);
    }
    Err(NvnError::BadRecord("recording does not end"))
}

/// Whether the operation of a header is one of the four extended records that order queues.
fn orders_queues(op: u16) -> bool {
    [
        ExtCode::FenceSync,
        ExtCode::SignalEvent,
        ExtCode::WaitSync,
        ExtCode::WaitEvent,
    ]
    .iter()
    .any(|c| *c as u16 == op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::RecCache;
    use crate::decode::{Ctx, State, summary};
    use crate::descriptors::Descriptors;
    use crate::ext::Ext;
    use crate::mem::VecMem;
    use crate::resource::ResourceTable;

    const BASE: u64 = 0x7000_0000;

    fn put(m: &mut VecMem, at: u64, records: &[Record]) {
        let mut pos = at;
        for r in records {
            let e = r.encode().unwrap();
            m.write(pos, e.as_bytes()).unwrap();
            pos += u64::from(e.len());
        }
    }

    fn wait(value: u32) -> Record {
        Record::Ext(
            Ext::new(ExtCode::WaitEvent, 0)
                .u64(0x15_0000_0100)
                .u32(0)
                .u32(value),
        )
    }

    fn signal(value: u32) -> Record {
        Record::Ext(
            Ext::new(ExtCode::SignalEvent, 0)
                .u64(0x15_0000_0100)
                .u32(1)
                .u32(0)
                .u32(0)
                .u32(value),
        )
    }

    /// A recording that waits, draws, calls one that fences, jumps to a block that signals and ends. A second level of
    /// calls has a wait sync. The scan gives the marks of the summary that order queues, in the same order.
    #[test]
    fn the_scan_gives_the_queue_marks_of_the_summary_in_order() {
        let mut m = VecMem::new(BASE, 8192);
        let (a, b, c, d) = (BASE, BASE + 0x400, BASE + 0x800, BASE + 0xc00);
        put(
            &mut m,
            a,
            &[
                wait(3),
                Record::Draw,
                Record::Ext(Ext::new(ExtCode::Draw, 1).u32(4).u32(0).u32(36).u32(0)),
                Record::Call(b),
                Record::Jump(c),
            ],
        );
        put(
            &mut m,
            b,
            &[
                Record::Ext(Ext::new(ExtCode::FenceSync, 0).u64(0x15_0000_0200)),
                Record::Call(d),
                Record::End,
            ],
        );
        put(&mut m, c, &[Record::Draw, signal(4), Record::End]);
        put(
            &mut m,
            d,
            &[
                Record::Ext(Ext::new(ExtCode::WaitSync, 0).u64(0x15_0000_0300)),
                Record::End,
            ],
        );
        let mut scanned = Vec::new();
        queue_marks(&m, a, &mut scanned).unwrap();

        let (table, desc) = (ResourceTable::new(), Descriptors::default());
        let cx = Ctx {
            mem: &m,
            table: &table,
            desc: &desc,
        };
        let mut cache = RecCache::default();
        let i = summary(&mut cache, &cx, a, &State::default(), 0).unwrap();
        let from_summary: Vec<MarkOp> = cache
            .entry(a, i)
            .unwrap()
            .stream
            .marks
            .iter()
            .map(|m| m.op)
            .filter(MarkOp::orders_queues)
            .collect();

        assert_eq!(scanned.len(), 4, "{scanned:?}");
        assert_eq!(scanned, from_summary);
    }

    #[test]
    fn a_recording_that_does_not_end_is_an_error() {
        let mut m = VecMem::new(BASE, 256);
        put(&mut m, BASE, &[Record::Jump(BASE)]);
        assert!(queue_marks(&m, BASE, &mut Vec::new()).is_err());
    }
}
