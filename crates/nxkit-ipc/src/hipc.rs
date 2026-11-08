//! HIPC, the message format of the kernel: the header, the special header, the descriptors and the raw data.
//!
//! The layout follows `dm/host/hle_sf.c` of nxnative (checked on 2026-11-07) and the public Horizon documentation.
//! The descriptor bit fields are unverified against hardware.
//!
//! Message start, in words: word 0 has the type (bits 0 to 15) and the counts of X, A, B and W descriptors (4 bits
//! each). Word 1 has the number of raw data words (bits 0 to 9), the receive list mode (bits 10 to 13) and the flag
//! of the special header (bit 31). The special header has the PID flag (bit 0), the number of copy handles (bits 1 to
//! 4) and the number of move handles (bits 5 to 8). The raw data starts at the next 16-byte boundary of the message.

use nxkit_abi::ipc::{Buffer, BufferKind};

/// `Close`: the client closes the session.
pub(crate) const TYPE_CLOSE: u16 = 2;
/// `LegacyRequest`.
pub(crate) const TYPE_LEGACY_REQUEST: u16 = 1;
/// `LegacyControl`.
pub(crate) const TYPE_LEGACY_CONTROL: u16 = 3;
/// `Request`.
pub(crate) const TYPE_REQUEST: u16 = 4;
/// `Control`.
pub(crate) const TYPE_CONTROL: u16 = 5;
/// `RequestWithContext`.
pub(crate) const TYPE_REQUEST_CONTEXT: u16 = 6;
/// `ControlWithContext`.
pub(crate) const TYPE_CONTROL_CONTEXT: u16 = 7;

/// Why a message could not be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParseError {
    /// The counts in the header reach past the end of the message.
    Truncated,
    /// The message uses a form that this server does not serve.
    Unsupported(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Truncated => f.write_str("the message is shorter than its header says"),
            ParseError::Unsupported(what) => write!(f, "unsupported HIPC form: {what}"),
        }
    }
}

/// A parsed message. `raw` is the raw data section from its 16-byte boundary to its end.
#[derive(Debug)]
pub(crate) struct Parsed<'a> {
    pub(crate) msg_type: u16,
    pub(crate) pid: Option<u64>,
    pub(crate) copy_handles: Vec<u32>,
    pub(crate) move_handles: Vec<u32>,
    pub(crate) buffers: Vec<Buffer>,
    pub(crate) raw: &'a [u8],
}

fn word(msg: &[u8], at: usize) -> Result<u32, ParseError> {
    let b = msg.get(at..at + 4).ok_or(ParseError::Truncated)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn align16(n: usize) -> usize {
    (n + 15) & !15
}

/// Parses the message in `msg`, which is the TLS buffer or the user buffer of the request.
pub(crate) fn parse(msg: &[u8]) -> Result<Parsed<'_>, ParseError> {
    let w0 = word(msg, 0)?;
    let w1 = word(msg, 4)?;
    let msg_type = (w0 & 0xFFFF) as u16;
    let counts = [
        (w0 >> 16) & 0xF,
        (w0 >> 20) & 0xF,
        (w0 >> 24) & 0xF,
        (w0 >> 28) & 0xF,
    ];
    let data_words = (w1 & 0x3FF) as usize;
    let c_mode = (w1 >> 10) & 0xF;
    let mut at = 8;

    let mut pid = None;
    let mut copy_handles = Vec::new();
    let mut move_handles = Vec::new();
    if w1 >> 31 != 0 {
        let special = word(msg, at)?;
        at += 4;
        if special & 1 != 0 {
            pid = Some(u64::from(word(msg, at)?) | u64::from(word(msg, at + 4)?) << 32);
            at += 8;
        }
        for _ in 0..(special >> 1) & 0xF {
            copy_handles.push(word(msg, at)?);
            at += 4;
        }
        for _ in 0..(special >> 5) & 0xF {
            move_handles.push(word(msg, at)?);
            at += 4;
        }
    }

    let mut buffers = Vec::new();
    for i in 0..counts[0] {
        let a = word(msg, at)?;
        let b = word(msg, at + 4)?;
        at += 8;
        let addr =
            u64::from(b) | u64::from((a >> 12) & 0xF) << 32 | u64::from((a >> 6) & 0x3F) << 36;
        buffers.push(Buffer {
            addr,
            size: u64::from(a >> 16),
            kind: BufferKind::Pointer,
            index: i,
        });
    }
    let kinds = [BufferKind::Send, BufferKind::Receive, BufferKind::Exchange];
    for (kind, count) in kinds.into_iter().zip(&counts[1..]) {
        for i in 0..*count {
            let size_lo = word(msg, at)?;
            let addr_lo = word(msg, at + 4)?;
            let w2 = word(msg, at + 8)?;
            at += 12;
            let addr = u64::from(addr_lo)
                | u64::from(w2 >> 28) << 32
                | u64::from((w2 >> 2) & 0x3F_FFFF) << 36;
            let size = u64::from(size_lo) | u64::from((w2 >> 24) & 0xF) << 32;
            buffers.push(Buffer {
                addr,
                size,
                kind,
                index: i,
            });
        }
    }

    let raw_start = align16(at);
    let raw_end = at + data_words * 4;
    if raw_end > msg.len() {
        return Err(ParseError::Truncated);
    }
    let raw = if raw_end > raw_start {
        &msg[raw_start..raw_end]
    } else {
        &[]
    };

    // Mode 0 has no receive list. Mode 2 lets the kernel choose the pointer buffer, which this server never uses
    // because `QueryPointerBufferSize` answers 0. A mode above 2 has `mode - 2` entries after the raw data.
    match c_mode {
        0 | 2 => {}
        1 => return Err(ParseError::Unsupported("receive list at an offset")),
        n => {
            let mut c = raw_end;
            for i in 0..n - 2 {
                let lo = word(msg, c)?;
                let hi = word(msg, c + 4)?;
                c += 8;
                buffers.push(Buffer {
                    addr: u64::from(lo) | u64::from(hi & 0xFFFF) << 32,
                    size: u64::from(hi >> 16),
                    kind: BufferKind::ReceiveList,
                    index: i,
                });
            }
        }
    }

    Ok(Parsed {
        msg_type,
        pid,
        copy_handles,
        move_handles,
        buffers,
        raw,
    })
}

/// A reply to encode. The CMIF header and the domain header are part of the encoding.
pub(crate) struct Reply<'a> {
    pub(crate) result: u32,
    pub(crate) data: &'a [u8],
    pub(crate) copy_handles: &'a [u32],
    pub(crate) move_handles: &'a [u32],
    /// `Some` for a reply in a domain: the object IDs of the objects the command returned.
    pub(crate) domain_objects: Option<&'a [u32]>,
}

/// The `SFCO` magic of a CMIF out header, as little-endian bytes.
const SFCO: u32 = 0x4f43_4653;

/// Encodes the reply as the message the game reads from its TLS buffer.
pub(crate) fn encode(r: &Reply<'_>) -> Vec<u8> {
    let special = !r.copy_handles.is_empty() || !r.move_handles.is_empty();
    let mut out = vec![0u8; 8];
    if special {
        let word = ((r.copy_handles.len() as u32) << 1) | ((r.move_handles.len() as u32) << 5);
        out.extend_from_slice(&word.to_le_bytes());
        for h in r.copy_handles.iter().chain(r.move_handles) {
            out.extend_from_slice(&h.to_le_bytes());
        }
    }
    let pad = align16(out.len()) - out.len();
    out.resize(out.len() + pad, 0);
    let body_start = out.len();

    if let Some(ids) = r.domain_objects {
        out.extend_from_slice(&(ids.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 12]);
    }
    out.extend_from_slice(&SFCO.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&r.result.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(r.data);
    if let Some(ids) = r.domain_objects {
        for id in ids {
            out.extend_from_slice(&id.to_le_bytes());
        }
    }
    let end = align16(out.len());
    out.resize(end, 0);

    let words = ((out.len() - body_start + pad) / 4) as u32;
    let w1 = words | (u32::from(special) << 31);
    out[4..8].copy_from_slice(&w1.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(msg: &mut Vec<u8>, w: u32) {
        msg.extend_from_slice(&w.to_le_bytes());
    }

    #[test]
    fn parses_a_plain_request() {
        let mut m = Vec::new();
        put(&mut m, u32::from(TYPE_REQUEST));
        // Two words of padding up to the boundary and eight words of data.
        put(&mut m, 10);
        m.resize(16, 0);
        for w in [0x4943_4653u32, 0, 7, 0, 1, 2, 0, 0] {
            put(&mut m, w);
        }
        let p = parse(&m).unwrap();
        assert_eq!(p.msg_type, TYPE_REQUEST);
        assert_eq!(p.raw.len(), 32);
        assert!(p.pid.is_none() && p.buffers.is_empty());
    }

    #[test]
    fn parses_pid_handles_and_descriptors() {
        let mut m = Vec::new();
        // One X descriptor and one B descriptor, 0 raw words.
        put(&mut m, u32::from(TYPE_REQUEST) | 1 << 16 | 1 << 24);
        put(&mut m, 1 << 31);
        put(&mut m, 1 | 1 << 1 | 1 << 5);
        put(&mut m, 0x55);
        put(&mut m, 0);
        put(&mut m, 0xAA);
        put(&mut m, 0xBB);
        // X: index 0, addr 0x1_2000_0000 (mid = 1), size 0x30.
        put(&mut m, 1 << 12 | 0x30 << 16);
        put(&mut m, 0x2000_0000);
        // B: size 0x1000, addr 0x3000_0000.
        put(&mut m, 0x1000);
        put(&mut m, 0x3000_0000);
        put(&mut m, 0);
        let p = parse(&m).unwrap();
        assert_eq!(p.pid, Some(0x55));
        assert_eq!(p.copy_handles, [0xAA]);
        assert_eq!(p.move_handles, [0xBB]);
        assert_eq!(p.buffers.len(), 2);
        assert_eq!(p.buffers[0].addr, 0x1_2000_0000);
        assert_eq!(p.buffers[0].size, 0x30);
        assert_eq!(p.buffers[0].kind, BufferKind::Pointer);
        assert_eq!(p.buffers[1].addr, 0x3000_0000);
        assert_eq!(p.buffers[1].kind, BufferKind::Receive);
    }

    #[test]
    fn a_short_message_is_truncated() {
        let mut m = Vec::new();
        put(&mut m, u32::from(TYPE_REQUEST) | 2 << 20);
        put(&mut m, 0);
        assert_eq!(parse(&m).unwrap_err(), ParseError::Truncated);
    }

    #[test]
    fn the_reply_puts_its_raw_data_on_a_boundary() {
        let r = Reply {
            result: 0x202,
            data: &[1, 2, 3, 4],
            copy_handles: &[],
            move_handles: &[0x77],
            domain_objects: None,
        };
        let out = encode(&r);
        assert_eq!(out.len() % 16, 0);
        let p = parse(&out).unwrap();
        assert_eq!(p.move_handles, [0x77]);
        assert_eq!(&p.raw[..4], b"SFCO");
        assert_eq!(
            u32::from_le_bytes([p.raw[8], p.raw[9], p.raw[10], p.raw[11]]),
            0x202
        );
        assert_eq!(&p.raw[16..20], [1, 2, 3, 4]);
    }
}
