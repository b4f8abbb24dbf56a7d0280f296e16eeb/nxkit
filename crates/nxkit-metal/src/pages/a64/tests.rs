//! Tests of the decoder and the executor, and their helpers.

use super::decode::decode;
use super::*;

fn d(i: u32) -> Mem {
    decode(i).unwrap_or_else(|e| panic!("{i:#010x} not decoded: {e:?}"))
}

#[allow(clippy::too_many_arguments)]
fn check(
    i: u32,
    form: Form,
    size: u8,
    rt: u8,
    rt2: u8,
    rn: u8,
    mode: Mode,
    signed: bool,
    dest64: bool,
) {
    let m = d(i);
    assert_eq!(m.form, form, "{i:#010x}");
    assert_eq!(m.load, form.is_load(), "{i:#010x}");
    assert_eq!(m.size, size, "{i:#010x}");
    assert_eq!((m.rt, m.rn), (rt, rn), "{i:#010x}");
    if m.pair {
        assert_eq!(m.rt2, rt2, "{i:#010x}");
    }
    assert_eq!(m.mode, mode, "{i:#010x}");
    assert_eq!(m.signed, signed, "{i:#010x}");
    if signed {
        assert_eq!(m.dest64, dest64, "{i:#010x}");
    }
}

use Form::*;
use Mode::*;

mod atomics;
mod decode;
mod exec;

/// A register file and a 64-byte buffer for the executor tests.
struct Rig {
    x: [u64; 31],
    sp: u64,
    v: [u128; 32],
    mem: [u8; 128],
}

impl Rig {
    fn new() -> Self {
        let mut x = [0u64; 31];
        for (k, r) in x.iter_mut().enumerate() {
            *r = 0x1111_1111_1111_1111u64.wrapping_mul(k as u64 + 1) / 3;
        }
        let mut v = [0u128; 32];
        for (k, r) in v.iter_mut().enumerate() {
            *r = (k as u128 + 1) * 0x0101_0101_0101_0101_0101_0101_0101_0101;
        }
        let mut mem = [0u8; 128];
        for (k, b) in mem.iter_mut().enumerate() {
            *b = k as u8 ^ 0x5a;
        }
        Self { x, sp: 0, v, mem }
    }

    fn regs(&mut self) -> Regs {
        Regs {
            x: self.x.as_mut_ptr(),
            sp: &raw mut self.sp,
            v: self.v.as_mut_ptr(),
        }
    }

    /// Runs the instruction with the base register pointing at byte `at` of `mem`.
    fn run(&mut self, i: u32, at: usize) -> Plan {
        let op = d(i);
        let base = self.mem.as_mut_ptr() as u64 + at as u64;
        if op.rn == 31 {
            self.sp = base;
        } else {
            self.x[usize::from(op.rn)] = base;
        }
        let r = self.regs();
        // SAFETY: the register file and the buffer are the rig's own; the access lies inside the buffer.
        unsafe {
            let p = plan(&op, &r);
            let off = p.ea.wrapping_sub(self.mem.as_ptr() as u64) as usize;
            perform(&op, &r, &p, self.mem.as_mut_ptr().add(off));
            p
        }
    }

    fn word(&self, at: usize, n: usize) -> u64 {
        let mut b = [0u8; 8];
        b[..n].copy_from_slice(&self.mem[at..at + n]);
        u64::from_le_bytes(b)
    }
}

fn op(i: u32) -> Op {
    decode_op(i).unwrap_or_else(|e| panic!("{i:#010x} not decoded: {e:?}"))
}

impl Rig {
    fn run_op(&mut self, i: u32, at: usize) -> bool {
        let o = op(i);
        let rn = match o {
            Op::Rmw { rn, .. }
            | Op::Cas { rn, .. }
            | Op::Excl { rn, .. }
            | Op::Ld1 { rn, .. }
            | Op::Lane { rn, .. } => rn,
            Op::Mem(m) => m.rn,
        };
        let base = self.mem.as_mut_ptr() as u64 + at as u64;
        if rn == 31 {
            self.sp = base;
        } else {
            self.x[usize::from(rn)] = base;
        }
        let r = self.regs();
        // SAFETY: the rig's own register file and buffer.
        unsafe {
            let p = plan_op(&o, &r);
            let off = p.ea.wrapping_sub(self.mem.as_ptr() as u64) as usize;
            perform_op(&o, &r, &p, self.mem.as_mut_ptr().add(off))
        }
    }
}
