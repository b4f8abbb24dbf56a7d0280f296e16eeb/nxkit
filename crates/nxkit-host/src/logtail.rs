//! The last part of the log in memory, for the `log` command of the control socket (W35).
//!
//! The log pipe (`sys/logpipe.rs`) gives every chunk that it writes to a [`Tail`] too. The tail keeps the last
//! [`Tail::MAX`] bytes. A cursor counts every byte that the log ever wrote, so a reader can ask for the lines after
//! its last read and sees when the tail dropped lines that it did not read.

use std::collections::VecDeque;

/// The last bytes of the log.
#[derive(Debug, Default)]
pub struct Tail {
    bytes: VecDeque<u8>,
    /// The bytes that the log wrote in all, also the dropped ones.
    total: u64,
}

/// What a read of the tail gives.
#[derive(Debug, PartialEq, Eq)]
pub struct Lines {
    /// The lines, without their line ends.
    pub lines: Vec<String>,
    /// The cursor after the last complete line: the value for the next read with [`Tail::since`].
    pub cursor: u64,
    /// True when the tail dropped bytes after the cursor of the read.
    pub gap: bool,
}

impl Tail {
    /// The bytes that the tail keeps.
    pub const MAX: usize = 8 << 20;

    /// Adds a chunk of the log.
    pub fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        self.bytes.extend(chunk);
        let over = self.bytes.len().saturating_sub(Self::MAX);
        self.bytes.drain(..over);
    }

    /// The cursor of the first byte that the tail keeps.
    fn first(&self) -> u64 {
        self.total - self.bytes.len() as u64
    }

    /// The complete lines after `cursor`.
    pub fn since(&self, cursor: u64) -> Lines {
        let first = self.first();
        let gap = cursor < first;
        let skip = usize::try_from(cursor.saturating_sub(first)).unwrap_or(usize::MAX);
        let (lines, used) = complete_lines(self.bytes.iter().skip(skip).copied());
        Lines {
            lines,
            cursor: first.max(cursor) + used as u64,
            gap,
        }
    }

    /// The last `n` complete lines.
    pub fn last(&self, n: usize) -> Lines {
        let all = self.since(self.first());
        let start = all.lines.len().saturating_sub(n);
        Lines {
            lines: all.lines[start..].to_vec(),
            cursor: all.cursor,
            gap: false,
        }
    }
}

/// The complete lines of `bytes` and the bytes they take with their line ends.
fn complete_lines(bytes: impl Iterator<Item = u8>) -> (Vec<String>, usize) {
    let (mut lines, mut line, mut used, mut n) = (Vec::new(), Vec::new(), 0, 0);
    for b in bytes {
        n += 1;
        if b == b'\n' {
            lines.push(String::from_utf8_lossy(&line).into_owned());
            line.clear();
            used = n;
        } else {
            line.push(b);
        }
    }
    (lines, used)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_gets_the_lines_after_its_cursor() {
        let mut t = Tail::default();
        t.push(b"one\ntwo\nthr");
        let a = t.since(0);
        assert_eq!(a.lines, ["one", "two"]);
        assert!(!a.gap);
        t.push(b"ee\nfour\n");
        let b = t.since(a.cursor);
        assert_eq!(b.lines, ["three", "four"]);
        assert_eq!(t.since(b.cursor).lines, Vec::<String>::new());
        assert_eq!(t.last(1).lines, ["four"]);
    }

    #[test]
    fn a_reader_sees_the_lines_that_the_tail_dropped() {
        let mut t = Tail::default();
        t.push(&vec![b'x'; Tail::MAX]);
        t.push(b"\nlast\n");
        let r = t.since(0);
        assert!(r.gap);
        assert_eq!(r.lines.last().map(String::as_str), Some("last"));
    }
}
